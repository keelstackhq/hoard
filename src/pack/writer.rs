//! The write side of packing (spec §6.5, Writes and Shutdown): PUT bodies are spooled to disk,
//! sealed into packs of about 128 MiB and uploaded in the background; the shutdown drain seals
//! the last pack, waits for the uploads and writes the merged index.
//!
//! - Bodies are streamed to disk (length-checked by `ExactLength`), then copied into a pack
//!   file next to an encoded header; nothing is held in memory.
//! - Pack and index keys are single-use: every attempt takes a fresh number, and `Exists` is a
//!   failure, so the index never names a key whose upload did not finish.
//! - Pack uploads wait while the [`RateGate`] is paused; a 429 counts `throttled`, pauses and
//!   retries under a new key. The index upload ignores the gate.
//! - The drain keeps `index_reserve` of its budget for the index and writes an index of the
//!   packs that finished, even when others had to be aborted.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::warn;

use super::backend::{PackBackend, PackError};
use super::index::{Loc, MAX_ENTRIES, MAX_PACKS, MAX_UNUSED, PackIndex};
use super::names::PackNames;
use super::reader::PackReader;
use crate::envelope::{Header, encode_header};
use crate::ratelimit::RateGate;
use crate::redact::redacted;
use crate::server::body::ExactLength;
use crate::stats::Stats;
use crate::writebehind::spool;

/// Spooled, unsealed bytes at which a pack is sealed.
pub const PACK_TARGET: u64 = 128 << 20;

/// Most bytes on disk at once (spool files and sealed packs not yet uploaded).
pub const SPOOL_CAP: u64 = 2_000_000_000;

/// Pack uploads in flight at once.
pub const UPLOAD_CONCURRENCY: usize = 2;

/// Part of the drain budget kept for writing the index.
pub const INDEX_RESERVE: Duration = Duration::from_secs(20);

/// What [`PackWriter::put`] did with a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepted {
    /// Spooled completely; it goes into the next pack.
    Spooled,
    /// This server accepted the hash already, or the loaded index has it; the body was not read.
    Duplicate,
    /// Not cached (closed, spool full or missing); the body was not read. The caller counts
    /// `put_errors`.
    NotCached(&'static str),
}

/// Outcome of [`PackWriter::drain`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackDrain {
    /// Packs uploaded over the writer's lifetime.
    pub packs: u64,
    /// Artifacts whose pack upload was aborted by the drain deadline (also `put_errors`).
    pub dropped: u64,
    /// An index was uploaded.
    pub index_written: bool,
}

pub struct PackWriter {
    inner: Arc<Inner>,
}

struct Inner {
    backend: Arc<PackBackend>,
    reader: Arc<PackReader>,
    names: Arc<PackNames>,
    stats: Arc<Stats>,
    gate: Arc<RateGate>,
    spool_dir: PathBuf,
    target: u64,
    cap: u64,
    index_reserve: Duration,
    /// Pause passed to the gate after a 429 instead of the default (tests).
    pause: Option<Duration>,
    permits: Semaphore,
    state: Mutex<State>,
    /// PUTs between admission and the end of their `put` call (spooling, and sealing).
    spooling: watch::Sender<u64>,
    uploads: Mutex<Option<JoinSet<()>>>,
    /// Artifacts in packs whose upload has not finished; decremented where the outcome is counted,
    /// so an aborted upload is never counted twice.
    unfinished: AtomicU64,
    /// Artifacts in packs uploaded successfully.
    uploaded_items: AtomicU64,
    uploaded_bytes: AtomicU64,
    packs: AtomicU64,
    new_index: Mutex<PackIndex>,
    next_file: AtomicU64,
    seq_pack: AtomicU64,
    seq_index: AtomicU64,
    warned_full: AtomicBool,
    warned_missing: AtomicBool,
}

#[derive(Default)]
struct State {
    /// Hashes accepted by this server (spooled, sealed or uploaded).
    accepted: HashSet<String>,
    unsealed: Vec<Spooled>,
    unsealed_bytes: u64,
    /// Bytes of spool files and of sealed packs not yet uploaded.
    spooled_bytes: u64,
    /// The drain started: no new PUTs.
    closed: bool,
    /// The drain sealed the last pack: a PUT finishing spooling now is not cached.
    sealed_final: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn remove_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), error = %e, "cannot remove spool file");
    }
}

/// Counts a PUT in `spooling` until dropped.
struct SpoolingGuard(Arc<Inner>);

impl Drop for SpoolingGuard {
    fn drop(&mut self) {
        self.0.spooling.send_modify(|n| *n -= 1);
    }
}

/// An accepted artifact, from admission until its pack file is written. Dropping it removes
/// the spool file and frees its bytes; unless it was sealed, the hash is no longer accepted.
struct Spooled {
    inner: Arc<Inner>,
    hash: String,
    header: Header,
    path: PathBuf,
    sealed: bool,
}

impl Drop for Spooled {
    fn drop(&mut self) {
        remove_file(&self.path);
        let mut st = lock(&self.inner.state);
        st.spooled_bytes = st.spooled_bytes.saturating_sub(self.header.size);
        if !self.sealed {
            st.accepted.remove(&self.hash);
        }
    }
}

/// A pack file waiting for or in its upload. Dropping it (done, failed, aborted) removes the
/// file and frees its bytes.
struct SealedPack {
    inner: Arc<Inner>,
    path: PathBuf,
    entries: Vec<(String, Loc)>,
    bytes: u64,
    /// The upload succeeded: the hashes stay accepted.
    uploaded: AtomicBool,
}

impl Drop for SealedPack {
    fn drop(&mut self) {
        remove_file(&self.path);
        let mut st = lock(&self.inner.state);
        st.spooled_bytes = st.spooled_bytes.saturating_sub(self.bytes);
        if !self.uploaded.load(Ordering::Relaxed) {
            // Failed or aborted: a later PUT of these hashes is not a duplicate.
            for (hash, _) in &self.entries {
                st.accepted.remove(hash);
            }
        }
    }
}

enum Reserve {
    Ok(Spooled, SpoolingGuard),
    Duplicate,
    NotCached(&'static str),
}

impl PackWriter {
    /// A writer that spools into `spool_dir` (the caller creates it; [`Self::drain`] removes
    /// it). `reader` supplies the loaded index; uploads obey `gate`.
    pub fn new(
        backend: Arc<PackBackend>,
        reader: Arc<PackReader>,
        names: Arc<PackNames>,
        stats: Arc<Stats>,
        gate: Arc<RateGate>,
        spool_dir: PathBuf,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                backend,
                reader,
                names,
                stats,
                gate,
                spool_dir,
                target: PACK_TARGET,
                cap: SPOOL_CAP,
                index_reserve: INDEX_RESERVE,
                pause: None,
                permits: Semaphore::new(UPLOAD_CONCURRENCY),
                state: Mutex::default(),
                spooling: watch::Sender::new(0),
                uploads: Mutex::new(Some(JoinSet::new())),
                unfinished: AtomicU64::new(0),
                uploaded_items: AtomicU64::new(0),
                uploaded_bytes: AtomicU64::new(0),
                packs: AtomicU64::new(0),
                new_index: Mutex::default(),
                next_file: AtomicU64::new(0),
                seq_pack: AtomicU64::new(0),
                seq_index: AtomicU64::new(0),
                warned_full: AtomicBool::new(false),
                warned_missing: AtomicBool::new(false),
            }),
        }
    }

    fn configure(mut self, f: impl FnOnce(&mut Inner)) -> Self {
        f(Arc::get_mut(&mut self.inner).expect("configure before the writer is shared"));
        self
    }

    /// Seal at `bytes` of unsealed artifacts instead of [`PACK_TARGET`]; call before the first
    /// put (tests).
    pub fn with_target(self, bytes: u64) -> Self {
        self.configure(|i| i.target = bytes)
    }

    /// Replaces the spool cap ([`SPOOL_CAP`]); call before the first put (tests).
    pub fn with_spool_cap(self, bytes: u64) -> Self {
        self.configure(|i| i.cap = bytes)
    }

    /// Replaces the part of the drain budget kept for the index ([`INDEX_RESERVE`]); call
    /// before the first put (tests).
    pub fn with_index_reserve(self, reserve: Duration) -> Self {
        self.configure(|i| i.index_reserve = reserve)
    }

    /// Pause the gate for `pause` after a 429 instead of the default minute (tests).
    pub fn with_rate_pause(self, pause: Duration) -> Self {
        self.configure(|i| i.pause = Some(pause))
    }

    pub fn spool_dir(&self) -> &Path {
        &self.inner.spool_dir
    }

    /// Admission. `closed` is read and `spooling` bumped under the state lock, and the drain
    /// sets `closed` under the same lock, so once the drain waits no new PUT can slip in.
    fn reserve(&self, hash: &str, header: &Header) -> Reserve {
        let inner = &self.inner;
        if !inner.spool_dir.is_dir() {
            if !inner.warned_missing.swap(true, Ordering::Relaxed) {
                warn!(
                    path = %inner.spool_dir.display(),
                    "pack spool dir is missing; artifacts are not cached"
                );
            }
            return Reserve::NotCached("spool dir missing");
        }
        let mut st = lock(&inner.state);
        if st.closed {
            return Reserve::NotCached("shutting down");
        }
        if st.accepted.contains(hash) {
            return Reserve::Duplicate;
        }
        if st.spooled_bytes.saturating_add(header.size) > inner.cap {
            drop(st);
            if !inner.warned_full.swap(true, Ordering::Relaxed) {
                warn!(
                    cap = inner.cap,
                    "pack spool is full; artifacts that do not fit are not cached"
                );
            }
            return Reserve::NotCached("spool full");
        }
        st.accepted.insert(hash.to_owned());
        st.spooled_bytes += header.size;
        inner.spooling.send_modify(|n| *n += 1);
        drop(st);
        let n = inner.next_file.fetch_add(1, Ordering::Relaxed);
        Reserve::Ok(
            Spooled {
                inner: inner.clone(),
                hash: hash.to_owned(),
                header: header.clone(),
                path: inner.spool_dir.join(format!("{n}.spool")),
                sealed: false,
            },
            SpoolingGuard(inner.clone()),
        )
    }

    /// Spools `body` (exactly `header.size` bytes) and adds it to the next pack; returns once
    /// the body is on disk. When the unsealed bytes reach the target this call also seals the
    /// pack (outside the lock). An error leaves no spool file and the hash not accepted.
    pub async fn put<S>(&self, hash: &str, header: Header, body: S) -> io::Result<Accepted>
    where
        S: Stream<Item = io::Result<Bytes>> + Send + Unpin,
    {
        self.inner.reader.wait_loaded().await;
        if self.inner.reader.lookup(hash).await.is_some() {
            return Ok(Accepted::Duplicate);
        }
        let (item, _guard) = match self.reserve(hash, &header) {
            Reserve::Ok(item, guard) => (item, guard),
            Reserve::Duplicate => return Ok(Accepted::Duplicate),
            Reserve::NotCached(why) => return Ok(Accepted::NotCached(why)),
        };
        spool(&item.path, ExactLength::new(body, header.size)).await?;
        let sealing = {
            let mut st = lock(&self.inner.state);
            if st.sealed_final {
                return Ok(Accepted::NotCached("shutting down"));
            }
            st.unsealed_bytes += header.size;
            st.unsealed.push(item);
            if st.unsealed_bytes >= self.inner.target {
                st.unsealed_bytes = 0;
                Some(std::mem::take(&mut st.unsealed))
            } else {
                None
            }
        };
        if let Some(items) = sealing {
            self.seal(items).await;
        }
        Ok(Accepted::Spooled)
    }

    /// Writes `items` into one pack file and starts its upload. A failure counts the items as
    /// `put_errors`.
    async fn seal(&self, mut items: Vec<Spooled>) {
        if items.is_empty() {
            return;
        }
        let inner = &self.inner;
        let path = inner.spool_dir.join(format!(
            "pack-{}",
            inner.next_file.fetch_add(1, Ordering::Relaxed)
        ));
        let entries = match write_pack_file(&path, &items).await {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "cannot write a pack file; {} artifacts not cached", items.len());
                remove_file(&path);
                inner
                    .stats
                    .put_errors
                    .fetch_add(items.len() as u64, Ordering::Relaxed);
                return;
            }
        };
        let bytes: u64 = items.iter().map(|i| i.header.size).sum();
        for i in &mut items {
            i.sealed = true;
        }
        let n = items.len() as u64;
        lock(&inner.state).spooled_bytes += bytes;
        let pack = SealedPack {
            inner: inner.clone(),
            path,
            entries,
            bytes,
            uploaded: AtomicBool::new(false),
        };
        // Frees the spool files (and their bytes, now counted by the pack).
        drop(items);
        let mut uploads = lock(&inner.uploads);
        let Some(set) = uploads.as_mut() else {
            inner.stats.put_errors.fetch_add(n, Ordering::Relaxed);
            return;
        };
        // Reap finished tasks so the set does not grow with the run.
        while set.try_join_next().is_some() {}
        inner.unfinished.fetch_add(n, Ordering::Relaxed);
        let inner = inner.clone();
        set.spawn(async move { upload_pack(&inner, &pack).await });
    }

    /// Shutdown: stops accepting PUTs, waits for those still spooling, seals the rest, waits
    /// for the pack uploads until `index_reserve` before `timeout` (then aborts them and counts
    /// their artifacts as `put_errors`), writes the merged index when anything changed, and
    /// removes the spool directory.
    pub async fn drain(&self, timeout: Duration) -> PackDrain {
        let inner = &self.inner;
        let start = Instant::now();
        let deadline = start + timeout;
        let uploads_deadline = deadline.checked_sub(inner.index_reserve).unwrap_or(start);
        lock(&inner.state).closed = true;

        let mut spooling = inner.spooling.subscribe();
        if tokio::time::timeout_at(uploads_deadline, spooling.wait_for(|n| *n == 0))
            .await
            .is_err()
        {
            warn!(
                left = *spooling.borrow(),
                "PUTs were still spooling at shutdown; they are not cached"
            );
        }
        let rest = {
            let mut st = lock(&inner.state);
            st.sealed_final = true;
            st.unsealed_bytes = 0;
            std::mem::take(&mut st.unsealed)
        };
        self.seal(rest).await;

        let mut dropped = 0;
        let set = lock(&inner.uploads).take();
        if let Some(mut set) = set {
            let wait = async { while set.join_next().await.is_some() {} };
            if tokio::time::timeout_at(uploads_deadline, wait)
                .await
                .is_err()
            {
                set.abort_all();
                while set.join_next().await.is_some() {}
                dropped = inner.unfinished.swap(0, Ordering::Relaxed);
                if dropped > 0 {
                    inner.stats.put_errors.fetch_add(dropped, Ordering::Relaxed);
                    warn!(
                        dropped,
                        "pack uploads did not finish in time: {dropped} artifacts dropped"
                    );
                }
            }
        }

        let index_written = match tokio::time::timeout_at(deadline, self.write_index()).await {
            Ok(w) => w,
            Err(_) => {
                warn!("the pack index was not written before the shutdown deadline");
                self.index_failed();
                false
            }
        };

        match std::fs::remove_dir_all(&inner.spool_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                warn!(path = %inner.spool_dir.display(), error = %e, "cannot remove spool dir")
            }
        }
        PackDrain {
            packs: inner.packs.load(Ordering::Relaxed),
            dropped,
            index_written,
        }
    }

    /// The index could not be written: this job's packs are unknown to later jobs.
    fn index_failed(&self) {
        let stats = &self.inner.stats;
        let n = self.inner.uploaded_items.swap(0, Ordering::Relaxed);
        let bytes = self.inner.uploaded_bytes.swap(0, Ordering::Relaxed);
        if n > 0 {
            // Move them: `puts` and `put_errors` stay disjoint. `packs` stays: they were uploaded.
            stats.puts.fetch_sub(n, Ordering::Relaxed);
            stats.bytes_up.fetch_sub(bytes, Ordering::Relaxed);
            stats.put_errors.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Merges the newest indexes with what was loaded and uploaded and uploads the result.
    /// Nothing to write (no new pack, nothing missing or corrupt): `false`, no request.
    async fn write_index(&self) -> bool {
        let inner = &self.inner;
        let missing = inner.reader.missing();
        let corrupt = inner.reader.corrupt();
        let new = lock(&inner.new_index).clone();
        if new.is_empty() && missing.is_empty() && corrupt.is_empty() {
            return false;
        }
        let mut idx = PackReader::load_newest(&inner.backend, &inner.names).await;
        idx.merge(&inner.reader.loaded().await);
        // Missing packs and corrupt entries are dropped before this job's packs are merged in:
        // a hash read as corrupt and packed again by this job must keep its new entry.
        idx.drop_packs(&missing);
        idx.remove_hashes(&corrupt);
        idx.merge(&new);
        idx.cap(now_ms(), MAX_UNUSED, MAX_PACKS, MAX_ENTRIES);
        let bytes = match tokio::task::spawn_blocking(move || idx.encode()).await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %redacted(&e), "cannot encode the pack index");
                self.index_failed();
                return false;
            }
        };
        let path = inner.spool_dir.join("index");
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            warn!(error = %e, "cannot write the pack index file");
            self.index_failed();
            return false;
        }
        drop(bytes);
        for _ in 0..2 {
            let m = inner.seq_index.fetch_add(1, Ordering::Relaxed) + 1;
            match inner.backend.upload(&inner.names.index_key(m), &path).await {
                Ok(_) => {
                    remove_file(&path);
                    return true;
                }
                Err(PackError::RateLimited { .. }) => {
                    inner.stats.throttled.fetch_add(1, Ordering::Relaxed);
                    warn!("pack index upload was rate limited");
                }
                Err(e) => warn!(error = %redacted(&e), "cannot upload the pack index"),
            }
        }
        remove_file(&path);
        warn!("the pack index was not written; this job's packs are not visible to later jobs");
        self.index_failed();
        false
    }
}

/// Writes each item's encoded header (with `hash` set) and spool body into `path`; returns
/// where each artifact sits.
async fn write_pack_file(path: &Path, items: &[Spooled]) -> io::Result<Vec<(String, Loc)>> {
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let mut offset = 0u64;
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let mut header = item.header.clone();
        header.hash = Some(item.hash.clone());
        let head = encode_header(&header);
        w.write_all(&head).await?;
        let mut src = tokio::fs::File::open(&item.path).await?;
        let copied = tokio::io::copy(&mut src, &mut w).await?;
        if copied != header.size {
            return Err(io::Error::other(format!(
                "spool file of {} has {copied} bytes, expected {}",
                item.hash, header.size
            )));
        }
        let len = head.len() as u64 + copied;
        entries.push((
            item.hash.clone(),
            Loc {
                offset,
                len,
                size: header.size,
                duration_ms: header.duration_ms,
                tag: header.tag.clone(),
            },
        ));
        offset += len;
    }
    w.flush().await?;
    w.into_inner().shutdown().await?;
    Ok(entries)
}

/// One pack upload: wait for a permit and an open gate, upload under a fresh key, retry under
/// another after a 429, count the outcome, add the pack to the new index on success.
async fn upload_pack(inner: &Arc<Inner>, pack: &SealedPack) {
    let stats = &inner.stats;
    let items = pack.entries.len() as u64;
    // The outcome and the `unfinished` decrement happen together (no await between them), so
    // the drain deadline cannot abort an upload that already counted its artifacts.
    let finish = || inner.unfinished.fetch_sub(items, Ordering::Relaxed);
    let Ok(_permit) = inner.permits.acquire().await else {
        finish();
        return;
    };
    loop {
        loop {
            let wait = inner.gate.remaining_pause(std::time::Instant::now());
            if wait.is_zero() {
                break;
            }
            tokio::time::sleep(wait).await;
        }
        let n = inner.seq_pack.fetch_add(1, Ordering::Relaxed) + 1;
        let key = inner.names.pack_key(n);
        match inner.backend.upload(&key, &pack.path).await {
            Ok(_) => {
                stats.puts.fetch_add(items, Ordering::Relaxed);
                stats.bytes_up.fetch_add(pack.bytes, Ordering::Relaxed);
                stats.packs.fetch_add(1, Ordering::Relaxed);
                inner.packs.fetch_add(1, Ordering::Relaxed);
                inner.uploaded_items.fetch_add(items, Ordering::Relaxed);
                inner
                    .uploaded_bytes
                    .fetch_add(pack.bytes, Ordering::Relaxed);
                lock(&inner.new_index).add_pack(&key, now_ms(), pack.entries.clone());
                pack.uploaded.store(true, Ordering::Relaxed);
                finish();
            }
            Err(PackError::RateLimited { retry_after }) => {
                stats.throttled.fetch_add(1, Ordering::Relaxed);
                inner
                    .gate
                    .on_rate_limited(std::time::Instant::now(), inner.pause.or(retry_after));
                continue;
            }
            Err(e) => {
                warn!(error = %redacted(&e), "cannot upload a pack; {items} artifacts not cached");
                stats.put_errors.fetch_add(items, Ordering::Relaxed);
                finish();
            }
        }
        return;
    }
}
