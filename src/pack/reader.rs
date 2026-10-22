//! The read side of packing (spec §6.5): load the newest index in the background, answer
//! lookups from it, and serve artifacts with one Range read of a pack.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use tokio::time::Instant;
use tracing::{info, warn};

use super::backend::PackBackend;
use super::index::{Entry, MAX_DECODED, PackIndex};
use super::names::PackNames;
use crate::envelope::{EnvelopeError, Header, MAX_HEADER_LEN, PREFIX_LEN, decode_header};
use crate::redact::redacted;
use crate::server::body::ExactLength;
use crate::store::BodyStream;

pub enum PackGet {
    /// The envelope header and exactly `header.size` body bytes.
    Hit(Header, BodyStream),
    Miss,
    /// The bytes at the indexed range are not the artifact (wrong hash/size/length).
    Corrupt,
}

impl std::fmt::Debug for PackGet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hit(h, _) => f.debug_tuple("Hit").field(h).finish(),
            Self::Miss => f.write_str("Miss"),
            Self::Corrupt => f.write_str("Corrupt"),
        }
    }
}

#[derive(Default)]
struct State {
    index: PackIndex,
    /// The wait gave up: a load that finishes later is discarded.
    abandoned: bool,
}

pub struct PackReader {
    backend: Arc<PackBackend>,
    state: Mutex<State>,
    /// True once the load finished (or none was requested).
    ready: tokio::sync::watch::Sender<bool>,
    deadline: Instant,
    missing: Mutex<HashSet<String>>,
    corrupt: Mutex<HashSet<String>>,
    touched: Mutex<HashMap<String, u64>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

impl PackReader {
    /// With `load`, spawns the background load of the newest index; reads and lookups wait
    /// for it at most `wait` from now (a load that is not done by then leaves the index empty
    /// for good). Without `load` the index is empty and nothing waits.
    pub fn start(
        backend: Arc<PackBackend>,
        names: Arc<PackNames>,
        load: bool,
        wait: Duration,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            backend,
            state: Mutex::default(),
            ready: tokio::sync::watch::channel(!load).0,
            deadline: Instant::now() + wait,
            missing: Mutex::default(),
            corrupt: Mutex::default(),
            touched: Mutex::default(),
        });
        if load {
            let r = this.clone();
            tokio::spawn(async move {
                let idx = Self::load_newest(&r.backend, &names).await;
                {
                    let mut st = lock(&r.state);
                    if st.abandoned {
                        warn!("pack index finished loading after the wait; it is not used");
                    } else {
                        info!(
                            "pack index loaded: {} artifacts in {} packs",
                            idx.len(),
                            idx.pack_count()
                        );
                        st.index = idx;
                    }
                }
                r.ready.send_replace(true);
            });
        }
        this
    }

    /// The newest index of this ref and of the base ref, else of any ref, merged. Errors and
    /// undecodable indexes are warnings and count as absent.
    pub async fn load_newest(backend: &PackBackend, names: &PackNames) -> PackIndex {
        let (ref_p, base_p, any_p) = names.index_prefixes();
        let (a, b) = tokio::join!(load_one(backend, &ref_p), async {
            match &base_p {
                Some(p) => load_one(backend, p).await,
                None => None,
            }
        });
        let mut found = a.into_iter().chain(b);
        let Some(mut idx) = found.next() else {
            return load_one(backend, &any_p).await.unwrap_or_default();
        };
        for other in found {
            idx.merge(&other);
        }
        idx
    }

    /// Waits until the index is loaded, at most until the deadline; after that the index
    /// stays as it is (empty if the load had not finished).
    pub async fn wait_loaded(&self) {
        let mut rx = self.ready.subscribe();
        if *rx.borrow_and_update() {
            return;
        }
        let waited = tokio::time::timeout_at(self.deadline, rx.wait_for(|r| *r)).await;
        if waited.is_err() {
            let mut st = lock(&self.state);
            if !*self.ready.borrow() && !st.abandoned {
                st.abandoned = true;
                warn!("pack index was not loaded in time; continuing with an empty index");
            }
        }
    }

    /// The index entry of `hash`; `None` when absent, its pack is known missing, or its read
    /// was corrupt this run. Waits for the load (up to the deadline).
    pub async fn lookup(&self, hash: &str) -> Option<Entry> {
        self.wait_loaded().await;
        let entry = lock(&self.state).index.get(hash)?;
        if lock(&self.missing).contains(&*entry.pack) || lock(&self.corrupt).contains(hash) {
            return None;
        }
        Some(entry)
    }

    /// Reads the artifact with one Range read of its pack. The envelope's `hash` and `size`
    /// must match the request and the index entry, else it is corrupt. A pack reported not
    /// found is remembered as missing; any other error is a plain miss (warning).
    pub async fn get(&self, hash: &str) -> PackGet {
        let Some(entry) = self.lookup(hash).await else {
            return PackGet::Miss;
        };
        let loc = &entry.loc;
        let stream = match self
            .backend
            .read_range(&entry.pack, loc.offset, loc.len)
            .await
        {
            Ok(Some(s)) => s,
            Ok(None) => {
                warn!(
                    "pack {} was not found; its artifacts are misses",
                    entry.pack
                );
                lock(&self.missing).insert(entry.pack.to_string());
                return PackGet::Miss;
            }
            Err(e) => {
                warn!("cannot read pack {}: {}", entry.pack, redacted(&e));
                return PackGet::Miss;
            }
        };
        let (header, header_len, rest) = match read_header(stream).await {
            Ok(Some(x)) => x,
            Ok(None) => return self.mark_corrupt(hash),
            Err(e) => {
                warn!("cannot read pack {}: {}", entry.pack, redacted(&e));
                return PackGet::Miss;
            }
        };
        if header.hash.as_deref() != Some(hash)
            || header.size != loc.size
            || (header_len as u64).checked_add(header.size) != Some(loc.len)
        {
            return self.mark_corrupt(hash);
        }
        lock(&self.touched).insert(entry.pack.to_string(), now_ms());
        let body = ExactLength::new(rest, header.size);
        PackGet::Hit(header, Box::pin(body))
    }

    fn mark_corrupt(&self, hash: &str) -> PackGet {
        warn!("pack entry for {hash} is corrupt; treating it as a miss");
        lock(&self.corrupt).insert(hash.to_string());
        PackGet::Corrupt
    }

    /// A snapshot of the loaded index, including the packs hit so far as used now.
    pub async fn loaded(&self) -> PackIndex {
        let mut idx = lock(&self.state).index.clone();
        for (pack, ts) in lock(&self.touched).iter() {
            idx.touch(pack, *ts);
        }
        idx
    }

    /// Keys of packs reported not found this run.
    pub fn missing(&self) -> HashSet<String> {
        lock(&self.missing).clone()
    }

    /// Hashes whose read was corrupt this run.
    pub fn corrupt(&self) -> HashSet<String> {
        lock(&self.corrupt).clone()
    }
}

/// The newest index under `prefix`, decoded. Errors are warnings: `None`.
async fn load_one(backend: &PackBackend, prefix: &str) -> Option<PackIndex> {
    let (key, bytes) = match backend.newest(prefix, MAX_DECODED).await {
        Ok(Some(x)) => x,
        Ok(None) => return None,
        Err(e) => {
            warn!(
                "cannot load the pack index under {prefix}: {}",
                redacted(&e)
            );
            return None;
        }
    };
    match tokio::task::spawn_blocking(move || PackIndex::decode(&bytes)).await {
        Ok(Ok(idx)) => {
            info!(
                "pack index {key}: {} artifacts in {} packs",
                idx.len(),
                idx.pack_count()
            );
            Some(idx)
        }
        Ok(Err(e)) => {
            warn!("ignoring the pack index {key}: {}", redacted(&e));
            None
        }
        Err(e) => {
            warn!("ignoring the pack index {key}: {}", redacted(&e));
            None
        }
    }
}

type Rest = futures::stream::Chain<
    futures::stream::Iter<std::option::IntoIter<std::io::Result<Bytes>>>,
    BodyStream,
>;

/// Parses the envelope header from the start of `stream` (at most `PREFIX_LEN +
/// MAX_HEADER_LEN` bytes are buffered). Returns the header, its length in bytes and the rest
/// of the stream (the bytes after the header). `Ok(None)`: a complete but invalid header.
/// `Err`: a read error, also a stream that ends inside the header (a truncated pack).
async fn read_header(mut stream: BodyStream) -> std::io::Result<Option<(Header, usize, Rest)>> {
    let mut head: Vec<u8> = Vec::with_capacity(PREFIX_LEN + 256);
    let mut pending = Bytes::new();
    let mut need = PREFIX_LEN;
    loop {
        while head.len() < need && !pending.is_empty() {
            let n = (need - head.len()).min(pending.len());
            head.extend_from_slice(&pending.split_to(n));
        }
        if head.len() >= need {
            match decode_header(&head) {
                Ok((h, end)) => {
                    let first = (!pending.is_empty()).then(|| Ok(pending));
                    let rest = futures::stream::iter(first).chain(stream);
                    return Ok(Some((h, end, rest)));
                }
                Err(EnvelopeError::Incomplete) if need == PREFIX_LEN => {
                    let len = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as usize;
                    if len > MAX_HEADER_LEN {
                        return Ok(None);
                    }
                    need = PREFIX_LEN + len;
                    continue;
                }
                Err(_) => return Ok(None),
            }
        }
        match stream.next().await {
            Some(chunk) => pending = chunk?,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the pack ended inside the envelope header",
                ));
            }
        }
    }
}
