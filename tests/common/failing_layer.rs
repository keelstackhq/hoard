//! A test-only OpenDAL layer that fails every write with a chosen `ErrorKind` and counts the
//! calls that reach the store, to stand in for a backend that rate-limits (GitHub cache 429)
//! or rejects a second create of an immutable key.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Fails writes with `kind` (and `message`, which may carry a fake HTTP response context).
#[derive(Debug, Clone)]
pub struct FailingWrites {
    pub kind: ErrorKind,
    /// Context added as `response`, like OpenDAL's HTTP services do (e.g. a `retry-after`).
    pub response: Option<String>,
    /// Report `write_with_if_not_exists = false`, like the GitHub cache service.
    pub unconditional: bool,
    pub calls: Arc<Calls>,
}

impl FailingWrites {
    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            response: None,
            unconditional: false,
            calls: Arc::default(),
        }
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

    fn write(&self, _ctx: &OperationContext, _path: &str, _args: OpWrite) -> Result<Self::Writer> {
        self.cfg.calls.write.fetch_add(1, Ordering::SeqCst);
        let mut e = Error::new(self.cfg.kind, "injected by FailingWrites");
        if let Some(r) = &self.cfg.response {
            e = e.with_context("response", r);
        }
        Err(e)
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
