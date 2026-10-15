//! A test-only OpenDAL layer that fails every write with a chosen `ErrorKind` and counts the
//! calls that reach the store, to stand in for a backend that rate-limits (GitHub cache 429)
//! or rejects a second create of an immutable key. [`FailingWrites::slow`] instead lets
//! writes through and delays each writer's `close` (a slow upload); [`FailingWrites::gated`]
//! blocks each `close` until the test opens a [`Gate`], so tests need no wall-clock bounds.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opendal::raw::*;
use opendal::{Capability, Error, ErrorKind, OperationContext, Result};

/// Calls that reached the wrapped service.
#[derive(Debug, Default)]
pub struct Calls {
    pub stat: AtomicU64,
    pub read: AtomicU64,
    pub write: AtomicU64,
}

impl Calls {
    /// stat + read + write calls so far.
    pub fn total(&self) -> u64 {
        self.stat.load(Ordering::SeqCst)
            + self.read.load(Ordering::SeqCst)
            + self.write.load(Ordering::SeqCst)
    }
}

/// A latch the test controls: while closed, every writer's `close` waits. `entered` counts
/// closes that reached the gate, `finished` those that completed.
#[derive(Debug)]
pub struct Gate {
    open: tokio::sync::watch::Sender<bool>,
    pub entered: AtomicU64,
    pub finished: AtomicU64,
}

impl Gate {
    pub fn new(open: bool) -> Arc<Self> {
        Arc::new(Self {
            open: tokio::sync::watch::channel(open).0,
            entered: AtomicU64::new(0),
            finished: AtomicU64::new(0),
        })
    }

    /// Lets blocked and future closes proceed.
    pub fn release(&self) {
        self.open.send_replace(true);
    }

    /// Blocks closes that start from now on.
    pub fn hold(&self) {
        self.open.send_replace(false);
    }

    async fn pass(&self) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut rx = self.open.subscribe();
        let _ = rx.wait_for(|open| *open).await;
    }
}

/// Fails writes with `kind` (and `message`, which may carry a fake HTTP response context).
#[derive(Debug, Clone)]
pub struct FailingWrites {
    pub kind: ErrorKind,
    /// Context added as `response`, like OpenDAL's HTTP services do (e.g. a `retry-after`).
    pub response: Option<String>,
    /// Report `write_with_if_not_exists = false`, like the GitHub cache service.
    pub unconditional: bool,
    /// Open the writer normally and fail its first `write` call instead (the GitHub cache
    /// writer is lazy: its create request, and so a 429, happens on the first write).
    pub fail_first_write: bool,
    /// Fail only this many writes; later writes pass through (`None`: fail every write).
    pub fail_times: Option<Arc<AtomicU64>>,
    /// Do not fail: pass writes through and sleep this long in each writer's `close`.
    pub slow_close: Option<Duration>,
    /// Do not fail: pass writes through, each writer's `close` waiting for this gate to open.
    pub gate: Option<Arc<Gate>>,
    pub calls: Arc<Calls>,
}

impl FailingWrites {
    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            response: None,
            unconditional: false,
            fail_first_write: false,
            fail_times: None,
            slow_close: None,
            gate: None,
            calls: Arc::default(),
        }
    }

    /// A store whose every upload takes at least `delay` (spent in `close`, so the object
    /// appears only after it).
    pub fn slow(delay: Duration) -> Self {
        Self {
            slow_close: Some(delay),
            ..Self::new(ErrorKind::Unexpected)
        }
    }

    /// A store whose uploads finish only while `gate` is open.
    pub fn gated(gate: Arc<Gate>) -> Self {
        Self {
            gate: Some(gate),
            ..Self::new(ErrorKind::Unexpected)
        }
    }

    /// True when this write should fail (consumes one of `fail_times`).
    fn should_fail(&self) -> bool {
        match &self.fail_times {
            None => true,
            // A compare-exchange loop: `fetch_update` is deprecated on newer Rust and its
            // replacement `try_update` is missing on older stable.
            Some(left) => {
                let mut n = left.load(Ordering::SeqCst);
                loop {
                    let Some(next) = n.checked_sub(1) else {
                        return false;
                    };
                    match left.compare_exchange(n, next, Ordering::SeqCst, Ordering::SeqCst) {
                        Ok(_) => return true,
                        Err(actual) => n = actual,
                    }
                }
            }
        }
    }
}

impl FailingWrites {
    fn error(&self) -> Error {
        let mut e = Error::new(self.kind, "injected by FailingWrites");
        if let Some(r) = &self.response {
            e = e.with_context("response", r);
        }
        e
    }
}

/// A writer whose first `write` fails with `pending`; later calls go to `inner`.
struct FailFirstWrite {
    inner: oio::Writer,
    pending: Option<Error>,
}

impl oio::Write for FailFirstWrite {
    async fn write(&mut self, bs: opendal::Buffer) -> Result<()> {
        match self.pending.take() {
            Some(e) => Err(e),
            None => self.inner.write(bs).await,
        }
    }

    async fn close(&mut self) -> Result<opendal::Metadata> {
        self.inner.close().await
    }

    async fn abort(&mut self) -> Result<()> {
        self.inner.abort().await
    }
}

/// A writer whose `close` waits for the gate and/or sleeps first.
struct SlowClose {
    inner: oio::Writer,
    delay: Option<Duration>,
    gate: Option<Arc<Gate>>,
}

impl oio::Write for SlowClose {
    async fn write(&mut self, bs: opendal::Buffer) -> Result<()> {
        self.inner.write(bs).await
    }

    async fn close(&mut self) -> Result<opendal::Metadata> {
        if let Some(gate) = &self.gate {
            gate.pass().await;
        }
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        let done = self.inner.close().await;
        if let Some(gate) = &self.gate {
            gate.finished.fetch_add(1, Ordering::SeqCst);
        }
        done
    }

    async fn abort(&mut self) -> Result<()> {
        self.inner.abort().await
    }
}

impl Layer for FailingWrites {
    fn apply_service(&self, inner: Servicer) -> Servicer {
        Arc::new(FailingService {
            inner,
            cfg: self.clone(),
        })
    }
}

#[derive(Debug)]
struct FailingService {
    inner: Servicer,
    cfg: FailingWrites,
}

impl Service for FailingService {
    type Reader = oio::Reader;
    type Writer = oio::Writer;
    type Lister = oio::Lister;
    type Deleter = oio::Deleter;
    type Copier = oio::Copier;
    type Composer = oio::Composer;

    fn info(&self) -> ServiceInfo {
        self.inner.info()
    }

    fn capability(&self) -> Capability {
        let mut c = self.inner.capability();
        if self.cfg.unconditional {
            c.write_with_if_not_exists = false;
        }
        c
    }

    async fn create_dir(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpCreateDir,
    ) -> Result<RpCreateDir> {
        self.inner.create_dir(ctx, path, args).await
    }

    async fn stat(&self, ctx: &OperationContext, path: &str, args: OpStat) -> Result<RpStat> {
        self.cfg.calls.stat.fetch_add(1, Ordering::SeqCst);
        self.inner.stat(ctx, path, args).await
    }

    fn read(&self, ctx: &OperationContext, path: &str, args: OpRead) -> Result<Self::Reader> {
        self.cfg.calls.read.fetch_add(1, Ordering::SeqCst);
        self.inner.read(ctx, path, args)
    }

    fn write(&self, ctx: &OperationContext, path: &str, args: OpWrite) -> Result<Self::Writer> {
        self.cfg.calls.write.fetch_add(1, Ordering::SeqCst);
        if self.cfg.slow_close.is_some() || self.cfg.gate.is_some() {
            let inner = self.inner.write(ctx, path, args)?;
            return Ok(Box::new(SlowClose {
                inner,
                delay: self.cfg.slow_close,
                gate: self.cfg.gate.clone(),
            }));
        }
        if !self.cfg.should_fail() {
            return self.inner.write(ctx, path, args);
        }
        if self.cfg.fail_first_write {
            let inner = self.inner.write(ctx, path, args)?;
            return Ok(Box::new(FailFirstWrite {
                inner,
                pending: Some(self.cfg.error()),
            }));
        }
        Err(self.cfg.error())
    }

    fn delete(&self, ctx: &OperationContext) -> Result<Self::Deleter> {
        self.inner.delete(ctx)
    }

    fn list(&self, ctx: &OperationContext, path: &str, args: OpList) -> Result<Self::Lister> {
        self.inner.list(ctx, path, args)
    }

    fn copy(
        &self,
        ctx: &OperationContext,
        from: &str,
        to: &str,
        args: OpCopy,
    ) -> Result<Self::Copier> {
        self.inner.copy(ctx, from, to, args)
    }

    async fn rename(
        &self,
        ctx: &OperationContext,
        from: &str,
        to: &str,
        args: OpRename,
    ) -> Result<RpRename> {
        self.inner.rename(ctx, from, to, args).await
    }

    async fn presign(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpPresign,
    ) -> Result<RpPresign> {
        self.inner.presign(ctx, path, args).await
    }
}
