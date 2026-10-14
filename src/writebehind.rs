//! Write-behind uploads (spec §5, PUT): a PUT is answered once its body is spooled to a local
//! file; background workers upload the spool files to the store, and shutdown drains them.
//!
//! - Bodies are streamed to disk, never held in memory, and length-checked on the way
//!   (`ExactLength`), so only complete artifacts are queued.
//! - Workers upload with [`Store::put`]: an if-not-exists write of the complete envelope.
//!   A worker aborted mid-upload (drain timeout) leaves no object at the key.
//! - Workers respect the [`RateGate`]: while uploads are paused they wait, and a 429 closes
//!   the gate and retries the item after the pause (not counted as `rate_limited`).
//! - A key already queued or uploading is not spooled again (`Duplicate`).
//! - A body that would push the spool past its cap, one arriving after the drain started, or
//!   any body while the spool dir is missing, is handed back (`Declined`) for a synchronous upload.
//! - A GET of a key that is still queued is a miss; Turborepo has the artifact locally.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tracing::warn;

use crate::envelope::Header;
use crate::ratelimit::RateGate;
use crate::redact::redacted;
use crate::server::body::ExactLength;
use crate::stats::Stats;
use crate::store::{PutOutcome, Store, StoreError};

/// Uploads in flight at once.
pub const CONCURRENCY: usize = 8;

/// How long shutdown waits for the queue; what is left is dropped and counted as `put_errors`.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Most bytes spooled at once (queued plus uploading). A PUT that would exceed it is
/// uploaded synchronously instead.
pub const SPOOL_CAP: u64 = 2_000_000_000;

/// Write buffer while spooling and read size while uploading from a spool file.
const IO_CHUNK: usize = 1 << 20;

/// `<RUNNER_TEMP or ./.hoard>/hoard-spool-<random>`: a fresh directory per server.
pub fn default_spool_dir() -> PathBuf {
    let name = format!(
        "hoard-spool-{}",
        rand::random::<[u8; 8]>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    match std::env::var_os("RUNNER_TEMP") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join(name),
        _ => PathBuf::from("./.hoard").join(name),
    }
}

/// What [`WriteBehind::enqueue`] did with a body.
pub enum Enqueued<S> {
    /// Spooled completely; a worker uploads it.
    Spooled,
    /// The key is already queued or uploading; the body was not read.
    Duplicate,
    /// Not queued (spool full, or the queue is draining); the unread body is handed back for
    /// a synchronous upload.
    Declined(S),
}

impl<S> std::fmt::Debug for Enqueued<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Spooled => "Spooled",
            Self::Duplicate => "Duplicate",
            Self::Declined(_) => "Declined",
        })
    }
}

/// Outcome of [`WriteBehind::drain`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Items this queue uploaded (created) over its lifetime.
    pub uploaded: u64,
    /// Items still queued or uploading when the drain timed out; counted as `put_errors`.
    pub dropped: u64,
}

pub struct WriteBehind {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<Store>,
    stats: Arc<Stats>,
    gate: Arc<RateGate>,
    spool_dir: PathBuf,
    cap: u64,
    permits: Semaphore,
    state: Mutex<State>,
    /// Items reserved and not yet finished (spooling, queued or uploading).
    pending: watch::Sender<u64>,
    uploaded: AtomicU64,
    next_file: AtomicU64,
    warned_full: AtomicBool,
    warned_missing: AtomicBool,
}

#[derive(Default)]
struct State {
    /// Keys queued or uploading.
    keys: HashSet<String>,
    /// Declared sizes of the reserved items.
    spooled_bytes: u64,
    /// Set when the drain starts: new bodies are declined.
    closed: bool,
    /// Upload tasks; taken (and aborted) by a drain that times out, after which no task is
    /// added.
    tasks: Option<JoinSet<()>>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Plain bookkeeping; a panic elsewhere cannot leave it half-updated.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One item from reservation to the end of its upload. Dropping it (finished, failed,
/// aborted, or a PUT handler cancelled mid-spool) removes the spool file and frees the key,
/// the spool bytes and the pending slot.
struct Reservation {
    inner: Arc<Inner>,
    key: String,
    size: u64,
    path: PathBuf,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path)
            && e.kind() != io::ErrorKind::NotFound
        {
            warn!(path = %self.path.display(), error = %e, "cannot remove spool file");
        }
        let mut st = self.inner.lock();
        st.keys.remove(&self.key);
        st.spooled_bytes -= self.size;
        drop(st);
        self.inner.pending.send_modify(|n| *n -= 1);
    }
}

enum Reserve {
    Ok(Reservation),
    Duplicate,
    Declined,
}

impl WriteBehind {
    /// A queue that spools into `spool_dir` (the caller creates it; [`Self::drain`] removes it)
    /// and uploads at most `concurrency` items at once. Workers update `stats` and obey
    /// `gate`, shared with the request handlers.
    pub fn new(
        store: Arc<Store>,
        stats: Arc<Stats>,
        gate: Arc<RateGate>,
        spool_dir: PathBuf,
        concurrency: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                stats,
                gate,
                spool_dir,
                cap: SPOOL_CAP,
                permits: Semaphore::new(concurrency.max(1)),
                state: Mutex::new(State {
                    tasks: Some(JoinSet::new()),
                    ..State::default()
                }),
                pending: watch::Sender::new(0),
                uploaded: AtomicU64::new(0),
                next_file: AtomicU64::new(0),
                warned_full: AtomicBool::new(false),
                warned_missing: AtomicBool::new(false),
            }),
        }
    }

    /// Replaces the spool cap ([`SPOOL_CAP`]); call before the first enqueue (tests).
    pub fn with_spool_cap(mut self, cap: u64) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("with_spool_cap before the queue is shared")
            .cap = cap;
        self
    }

    pub fn spool_dir(&self) -> &Path {
        &self.inner.spool_dir
    }

    /// Admission. `closed` is read and `pending` bumped under the state lock, and the drain
    /// sets `closed` under the same lock, so once the drain is waiting no new item can slip in.
    fn reserve(&self, key: &str, size: u64) -> Reserve {
        let inner = &self.inner;
        // The dir is created at startup; if it is gone, upload synchronously.
        if !inner.spool_dir.is_dir() {
            if !inner.warned_missing.swap(true, Ordering::Relaxed) {
                warn!(
                    path = %inner.spool_dir.display(),
                    "write-behind spool dir is missing; uploading PUTs synchronously"
                );
            }
            return Reserve::Declined;
        }
        let mut st = inner.lock();
        if st.closed {
            return Reserve::Declined;
        }
        if st.keys.contains(key) {
            return Reserve::Duplicate;
        }
        if st.spooled_bytes.saturating_add(size) > inner.cap {
            drop(st);
            if !inner.warned_full.swap(true, Ordering::Relaxed) {
                warn!(
                    cap = inner.cap,
                    "write-behind spool is full; uploading the PUTs that do not fit synchronously"
                );
            }
            return Reserve::Declined;
        }
        st.keys.insert(key.to_owned());
        st.spooled_bytes += size;
        inner.pending.send_modify(|n| *n += 1);
        drop(st);
        let n = inner.next_file.fetch_add(1, Ordering::Relaxed);
        Reserve::Ok(Reservation {
            inner: inner.clone(),
            key: key.to_owned(),
            size,
            path: inner.spool_dir.join(format!("{n}.spool")),
        })
    }

    /// Spools `body` to a file and queues its upload; returns once the body is on disk and is
    /// exactly `header.size` bytes long. An error (disk full, a short or long body, the queue
    /// aborted by a drain) leaves no spool file and nothing queued.
    pub async fn enqueue<S>(&self, key: String, header: Header, body: S) -> io::Result<Enqueued<S>>
    where
        S: Stream<Item = io::Result<Bytes>> + Send + Unpin,
    {
        let res = match self.reserve(&key, header.size) {
            Reserve::Ok(r) => r,
            Reserve::Duplicate => return Ok(Enqueued::Duplicate),
            Reserve::Declined => return Ok(Enqueued::Declined(body)),
        };
        spool(&res.path, ExactLength::new(body, header.size)).await?;
        let mut st = self.inner.lock();
        let Some(tasks) = st.tasks.as_mut() else {
            return Err(io::Error::other("the upload queue was dropped at shutdown"));
        };
        // Reap finished tasks so the set does not grow with the run.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(upload(res, header));
        Ok(Enqueued::Spooled)
    }

    /// Stops accepting bodies and waits up to `timeout` for every queued upload. Uploads still
    /// running then are aborted (no object is left at their keys) and counted as `put_errors`.
    /// Removes the spool directory.
    pub async fn drain(&self, timeout: Duration) -> DrainReport {
        let inner = &self.inner;
        inner.lock().closed = true;
        let mut pending = inner.pending.subscribe();
        let finished = tokio::time::timeout(timeout, pending.wait_for(|n| *n == 0))
            .await
            .is_ok();
        let mut dropped = 0;
        if !finished {
            let tasks = inner.lock().tasks.take();
            if let Some(mut tasks) = tasks {
                tasks.abort_all();
                while let Some(r) = tasks.join_next().await {
                    if r.is_err_and(|e| e.is_cancelled()) {
                        dropped += 1;
                    }
                }
            }
            if dropped > 0 {
                inner.stats.put_errors.fetch_add(dropped, Ordering::Relaxed);
                warn!(
                    dropped,
                    "upload queue did not drain within {}s: {dropped} uploads dropped",
                    timeout.as_secs_f32()
                );
            }
        }
        match std::fs::remove_dir_all(&inner.spool_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                warn!(path = %inner.spool_dir.display(), error = %e, "cannot remove spool dir")
            }
        }
        DrainReport {
            uploaded: inner.uploaded.load(Ordering::Relaxed),
            dropped,
        }
    }
}

/// Streams `body` into a new file at `path`. On error the caller's reservation removes it.
async fn spool<S>(path: &Path, mut body: S) -> io::Result<()>
where
    S: Stream<Item = io::Result<Bytes>> + Unpin,
{
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
    let mut w = tokio::io::BufWriter::with_capacity(IO_CHUNK, file);
    while let Some(chunk) = body.next().await {
        w.write_all(&chunk?).await?;
    }
    w.flush().await?;
    w.into_inner().shutdown().await
}

/// The spool file as a body stream of at most [`IO_CHUNK`]-byte chunks.
fn file_stream(file: tokio::fs::File) -> impl Stream<Item = io::Result<Bytes>> + Send + Unpin {
    Box::pin(futures::stream::try_unfold(file, |mut f| async move {
        let mut buf = vec![0u8; IO_CHUNK];
        let n = f.read(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.truncate(n);
        Ok(Some((Bytes::from(buf), f)))
    }))
}

/// One worker run: wait for a permit and an open rate gate, upload, count the outcome. A
/// rate limit closes the gate and the item is retried once it opens.
async fn upload(res: Reservation, header: Header) {
    let inner = res.inner.clone();
    let Ok(_permit) = inner.permits.acquire().await else {
        return;
    };
    let stats = &inner.stats;
    loop {
        loop {
            let wait = inner.gate.remaining_pause(Instant::now());
            if wait.is_zero() {
                break;
            }
            tokio::time::sleep(wait).await;
        }
        let file = match tokio::fs::File::open(&res.path).await {
            Ok(f) => f,
            Err(e) => {
                warn!(key = %res.key, error = %e, "write-behind: cannot open the spool file, not cached");
                stats.put_errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        let body = ExactLength::new(file_stream(file), header.size);
        match inner.store.put(&res.key, &header, body).await {
            Ok(PutOutcome::Created) => {
                stats.puts.fetch_add(1, Ordering::Relaxed);
                stats.bytes_up.fetch_add(header.size, Ordering::Relaxed);
                inner.uploaded.fetch_add(1, Ordering::Relaxed);
            }
            Ok(PutOutcome::AlreadyExists) => {
                stats.put_skipped_existing.fetch_add(1, Ordering::Relaxed);
            }
            // Not counted as `rate_limited` (that counts PUTs skipped by the handler): the item
            // is retried, and one that never gets through ends as dropped (`put_errors`).
            Err(StoreError::RateLimited { retry_after }) => {
                inner.gate.on_rate_limited(Instant::now(), retry_after);
                continue;
            }
            Err(e) => {
                warn!(key = %res.key, error = %redacted(&e), "write-behind: storage error, not cached");
                stats.put_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        return;
    }
}
