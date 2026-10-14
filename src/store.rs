//! Object store over an OpenDAL `Operator`. Every object is one HRD1 blob (see `envelope`).
//!
//! Backends: fs and S3/R2; the GitHub cache arrives in a later milestone.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt};
use opendal::{ErrorKind, Operator, Reader, services};

use crate::config::{Config, Storage};
use crate::envelope::{Header, MAX_HEADER_LEN, PREFIX_LEN, decode_header, encode_header};

/// Scratch directory (inside the fs root) for in-flight writes.
const FS_TMP_DIR: &str = ".hoard-tmp";

/// S3 multipart part size. Incoming HTTP frames are small; buffering them into parts of at
/// least 5 MiB (the S3 minimum) keeps multipart completion valid. Bodies smaller than this
/// go up in one conditional PUT.
const S3_CHUNK: usize = 8 * 1024 * 1024;

/// Bytes read to decode a header: fixed prefix plus the largest accepted JSON header.
const HEAD_READ_LEN: u64 = (PREFIX_LEN + MAX_HEADER_LEN) as u64;

/// Errors from the storage backend. Messages never contain secrets.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage error: {0}")]
    Storage(#[from] opendal::Error),
    #[error("body stream error: {0}")]
    Body(#[from] std::io::Error),
    /// A key prefix the backend would rewrite (leading `/`, `//`, whitespace, `..`); using it
    /// could reach keys outside the configured prefix.
    #[error(
        "refusing key prefix {0:?}: it must be non-empty, end with / and be in normal form (for example cache/)"
    )]
    BadPrefix(String),
}

pub enum GetOutcome {
    /// Header, a reader over the whole blob, and the offset where the artifact body starts.
    Hit(Header, Reader, u64),
    Miss,
    /// The object exists but is not a valid HRD1 blob.
    Corrupt,
}

impl std::fmt::Debug for GetOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hit(h, _, off) => f.debug_tuple("Hit").field(h).field(off).finish(),
            Self::Miss => f.write_str("Miss"),
            Self::Corrupt => f.write_str("Corrupt"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Created,
    AlreadyExists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub last_modified: DateTime<Utc>,
}

/// Builds the operator for the configured storage.
pub fn build_operator(cfg: &Config) -> anyhow::Result<Operator> {
    match cfg.storage {
        Storage::Fs => {
            std::fs::create_dir_all(&cfg.fs_root)?;
            let root = cfg
                .fs_root
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("--fs-root is not valid UTF-8"))?;
            // Write to a temp dir inside the root and rename on close: readers never see a
            // partial object and an aborted write leaves nothing behind. Same filesystem as
            // the root, so the rename cannot cross devices.
            let tmp = cfg.fs_root.join(FS_TMP_DIR);
            std::fs::create_dir_all(&tmp)?;
            let tmp = tmp
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("--fs-root is not valid UTF-8"))?;
            let fs = services::Fs::default().root(root).atomic_write_dir(tmp);
            Ok(Operator::new(fs)?)
        }
        Storage::R2 => {
            let r2 = cfg
                .r2
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("storage=r2 needs the r2-* settings"))?;
            crate::r2::s3_operator(
                &r2.endpoint(),
                crate::r2::R2_REGION,
                &r2.bucket,
                &r2.access_key_id,
                &r2.secret_access_key,
            )
        }
        Storage::Github => anyhow::bail!("storage=github is not yet supported in this build"),
    }
}

#[derive(Clone)]
pub struct Store {
    pub op: Operator,
    /// fs backend: hide the scratch dir from `list`.
    is_fs: bool,
    /// Writer chunk size (S3 only).
    chunk: Option<usize>,
}

fn is_condition_failed(e: &opendal::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::ConditionNotMatch | ErrorKind::AlreadyExists
    )
}

impl Store {
    pub fn new(op: Operator) -> Self {
        let scheme = op.info().scheme();
        Self {
            is_fs: scheme == "fs",
            chunk: (scheme == "s3").then_some(S3_CHUNK),
            op,
        }
    }

    /// Reads the first `PREFIX_LEN + MAX_HEADER_LEN` bytes (never the body). Objects shorter
    /// than that are fine: a range past EOF is clamped using the object's size. fs reports
    /// such a range as `RangeNotSatisfied`; S3 answers with fewer bytes, which OpenDAL
    /// reports as a permanent `Unexpected` (short read; timeouts and network errors are
    /// temporary and propagate). Keys are immutable, so re-reading is safe.
    async fn read_prefix(&self, reader: &Reader, key: &str) -> Result<Bytes, StoreError> {
        match reader.read(0..HEAD_READ_LEN).await {
            Ok(b) => Ok(b.to_bytes()),
            Err(e)
                if e.kind() == ErrorKind::RangeNotSatisfied
                    || (e.kind() == ErrorKind::Unexpected && !e.is_temporary()) =>
            {
                let size = self.op.stat(key).await?.content_length().min(HEAD_READ_LEN);
                if size == 0 {
                    return Ok(Bytes::new());
                }
                Ok(reader.read(0..size).await?.to_bytes())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Header of `key` via a bounded range read. Missing or corrupt objects both yield `None`
    /// (a corrupt object is a miss); use [`Store::get`] to tell them apart.
    pub async fn head(&self, key: &str) -> Result<Option<Header>, StoreError> {
        match self.get(key).await? {
            GetOutcome::Hit(h, _, _) => Ok(Some(h)),
            GetOutcome::Miss | GetOutcome::Corrupt => Ok(None),
        }
    }

    /// Reads and validates the header only; the caller streams the body from the returned offset.
    pub async fn get(&self, key: &str) -> Result<GetOutcome, StoreError> {
        let reader = match self.op.reader(key).await {
            Ok(r) => r,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(GetOutcome::Miss),
            Err(e) => return Err(e.into()),
        };
        let prefix = match self.read_prefix(&reader, key).await {
            Ok(p) => p,
            Err(StoreError::Storage(e)) if e.kind() == ErrorKind::NotFound => {
                return Ok(GetOutcome::Miss);
            }
            Err(e) => return Err(e),
        };
        match decode_header(&prefix) {
            Ok((h, off)) => Ok(GetOutcome::Hit(h, reader, off as u64)),
            Err(_) => Ok(GetOutcome::Corrupt),
        }
    }

    /// Writes header then streams the body. Keys are immutable: an existing key is
    /// `AlreadyExists`. A failing body stream aborts the write so no partial object remains.
    pub async fn put(
        &self,
        key: &str,
        header: &Header,
        body: impl Stream<Item = Result<Bytes, std::io::Error>> + Send,
    ) -> Result<PutOutcome, StoreError> {
        let conditional = self.op.info().capability().write_with_if_not_exists;
        if !conditional {
            match self.op.stat(key).await {
                Ok(_) => return Ok(PutOutcome::AlreadyExists),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let mut fut = self.op.writer_with(key).if_not_exists(conditional);
        if let Some(chunk) = self.chunk {
            fut = fut.chunk(chunk);
        }
        let mut writer = match fut.await {
            Ok(w) => w,
            Err(e) if conditional && is_condition_failed(&e) => {
                return Ok(PutOutcome::AlreadyExists);
            }
            Err(e) => return Err(e.into()),
        };
        let streamed: Result<(), StoreError> = async {
            writer.write(Bytes::from(encode_header(header))).await?;
            let mut body = std::pin::pin!(body);
            while let Some(chunk) = body.next().await {
                writer.write(chunk?).await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = streamed {
            if conditional && matches!(&e, StoreError::Storage(o) if is_condition_failed(o)) {
                let _ = writer.abort().await;
                return Ok(PutOutcome::AlreadyExists);
            }
            let _ = writer.abort().await;
            return Err(e);
        }
        match writer.close().await {
            Ok(_) => Ok(PutOutcome::Created),
            Err(e) => {
                // A rejected or failed completion leaves S3 multipart parts behind; drop them.
                let _ = writer.abort().await;
                if conditional && is_condition_failed(&e) {
                    Ok(PutOutcome::AlreadyExists)
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// Recursive listing of files under `prefix`; keys are relative to the operator root.
    pub async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
        let entries = match self.op.list_with(prefix).recursive(true).await {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            if entry.metadata().is_dir() {
                continue;
            }
            let key = entry.path().to_string();
            if self.is_fs && key.starts_with(&format!("{FS_TMP_DIR}/")) {
                continue; // in-flight fs write
            }
            let mut meta = entry.metadata().clone();
            if meta.last_modified().is_none() || !meta.is_file() {
                meta = self.op.stat(&key).await?;
            }
            out.push(ObjectInfo {
                key,
                size: meta.content_length(),
                last_modified: meta
                    .last_modified()
                    .map(|t| DateTime::<Utc>::from(std::time::SystemTime::from(t)))
                    .unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Deletes `key`; deleting a missing key succeeds.
    pub async fn delete(&self, key: &str) -> Result<(), StoreError> {
        Ok(self.op.delete(key).await?)
    }
}
