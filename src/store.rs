//! Object store over an OpenDAL `Operator`. Every object is one HRD1 blob (see `envelope`).
//!
//! Backends: fs, S3/R2 and the GitHub Actions cache (`ghac`).

use bytes::Bytes;
use chrono::{DateTime, Utc};
use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt, TryStreamExt};
use opendal::{BufferStream, BytesRange, ErrorKind, Operator, services};

use crate::config::{Config, Storage};
use crate::envelope::{
    EnvelopeError, Header, MAX_HEADER_LEN, PREFIX_LEN, decode_header, encode_header,
};

/// Scratch directory (inside the fs root) for in-flight writes.
const FS_TMP_DIR: &str = ".hoard-tmp";

/// Upload chunk size (S3 multipart part, GitHub cache block). Incoming HTTP frames are small;
/// buffering them into parts of at least 5 MiB (the S3 minimum) keeps multipart completion
/// valid, and on the GitHub cache it keeps one block upload per 8 MiB instead of one per
/// frame. Bodies smaller than this go up in one request.
const UPLOAD_CHUNK: usize = 8 * 1024 * 1024;

/// Fix shown when the GitHub Actions cache environment is incomplete.
const GHAC_HINT: &str = "run hoard through the GitHub Action, or use --storage r2|fs";

/// Bytes read by HEAD to decode a header: fixed prefix plus the largest accepted JSON header.
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
    /// The backend rate-limited a write (GitHub cache: HTTP 429). `retry_after` is the
    /// response's `Retry-After` in seconds, when it had one.
    #[error("storage rate limit hit (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },
}

/// `Retry-After` (delta-seconds) of the HTTP response an OpenDAL error came from. OpenDAL
/// keeps the response only as `response: Parts { .. headers: {"retry-after": "30", ..} }` in
/// the error's display text; an HTTP-date or a missing header yields `None`.
pub fn retry_after(e: &opendal::Error) -> Option<Duration> {
    const NEEDLE: &str = "\"retry-after\": \"";
    let text = e.to_string();
    let start = text.find(NEEDLE)? + NEEDLE.len();
    let rest = &text[start..];
    let value = &rest[..rest.find('"')?];
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// `RateLimited` for an OpenDAL rate-limit error; anything else unchanged.
fn rate_limited(e: StoreError) -> StoreError {
    match e {
        StoreError::Storage(o) if o.kind() == ErrorKind::RateLimited => StoreError::RateLimited {
            retry_after: retry_after(&o),
        },
        e => e,
    }
}

/// The GitHub Actions cache operator, configured from the process environment (empty values
/// count as unset). See [`ghac_operator_from`].
pub fn ghac_operator(cache_version: &str) -> anyhow::Result<Operator> {
    ghac_operator_from(cache_version, |k| std::env::var(k).ok())
}

/// The GitHub Actions cache operator, with environment variables read through `lookup`.
/// `ACTIONS_RESULTS_URL` and `ACTIONS_RUNTIME_TOKEN` must be set (only JavaScript actions get
/// them; the action passes them on). Entries are versioned `hoard-{cache_version}`, so
/// different cache versions never collide.
pub fn ghac_operator_from(
    cache_version: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Operator> {
    let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
    let need = |k: &str| var(k).ok_or_else(|| anyhow::anyhow!("{k} is not set: {GHAC_HINT}"));
    let results_url = need("ACTIONS_RESULTS_URL")?;
    let token = need("ACTIONS_RUNTIME_TOKEN")?;
    opendal::install_default();
    let mut ghac = services::Ghac::default()
        .version(&format!("hoard-{cache_version}"))
        .runtime_token(&token);
    // OpenDAL picks the endpoint itself: `ACTIONS_CACHE_URL`, then `ACTIONS_RESULTS_URL`, on
    // the v1 cache service (GHES), and `ACTIONS_RESULTS_URL` on v2. Without
    // `ACTIONS_CACHE_URL` both choices are the results URL, so pass it explicitly.
    if var("ACTIONS_CACHE_URL").is_none() {
        ghac = ghac.endpoint(&results_url);
    }
    Ok(Operator::new(ghac)?.layer(crate::r2::Timeouts::DEFAULT.layer()))
}

/// An artifact body: the bytes after the envelope header, streamed from the one storage read.
pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

pub enum GetOutcome {
    /// Header and the artifact body (everything after the header, as stored; the caller holds
    /// it to `header.size`).
    Hit(Header, BodyStream),
    Miss,
    /// The object exists but is not a valid HRD1 blob.
    Corrupt,
}

impl std::fmt::Debug for GetOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hit(h, _) => f.debug_tuple("Hit").field(h).finish(),
            Self::Miss => f.write_str("Miss"),
            Self::Corrupt => f.write_str("Corrupt"),
        }
    }
}

/// A read whose envelope header has been parsed: the header, the bytes of the last frame that
/// follow it, and the rest of the same read.
enum Opened {
    Hit(Header, Vec<Bytes>, Box<BufferStream>),
    Miss,
    Corrupt,
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
        Storage::Github => ghac_operator(&cfg.cache_version),
    }
}

#[derive(Clone)]
pub struct Store {
    pub op: Operator,
    /// fs backend: hide the scratch dir from `list`.
    is_fs: bool,
    /// Writer chunk size (S3 and GitHub cache).
    chunk: Option<usize>,
    /// Abort a failed write. Not on the GitHub cache: its abort is a no-op that first creates
    /// the cache entry if the failed write had not, one more request against the rate limit.
    abort_failed: bool,
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
            chunk: matches!(scheme, "s3" | "ghac").then_some(UPLOAD_CHUNK),
            abort_failed: scheme != "ghac",
            op,
        }
    }

    /// Opens one streaming read of `key` over `range` and parses the envelope header from its
    /// first bytes. At most `PREFIX_LEN + MAX_HEADER_LEN` bytes are copied into the header
    /// buffer; the rest of the frame and of the read are handed back untouched.
    ///
    /// A short object is fine as long as the header is complete: the header is parsed before
    /// the stream reaches its end. An object that ends inside the header is corrupt; with a
    /// bounded `range` OpenDAL reports that end as a permanent `Unexpected` ("too little data",
    /// S3) or `RangeNotSatisfied` (an empty object), so both count as corrupt here. Timeouts
    /// and network errors are temporary and propagate.
    async fn open(&self, key: &str, range: BytesRange) -> Result<Opened, StoreError> {
        let bounded = range.size().is_some();
        let reader = match self.op.reader(key).await {
            Ok(r) => r,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Opened::Miss),
            Err(e) => return Err(e.into()),
        };
        let mut stream = match reader.into_stream(range).await {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Opened::Miss),
            Err(e) => return Err(e.into()),
        };
        let mut head: Vec<u8> = Vec::with_capacity(PREFIX_LEN);
        let mut need = PREFIX_LEN;
        loop {
            let buf = match stream.next().await {
                Some(Ok(buf)) => buf,
                Some(Err(e)) if e.kind() == ErrorKind::NotFound => return Ok(Opened::Miss),
                Some(Err(e))
                    if e.kind() == ErrorKind::RangeNotSatisfied
                        || (bounded && e.kind() == ErrorKind::Unexpected && !e.is_temporary()) =>
                {
                    return Ok(Opened::Corrupt);
                }
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(Opened::Corrupt), // ended inside the header
            };
            // `Buffer` is both an iterator and a stream of its `Bytes` parts; use the iterator.
            let mut frames = buf;
            while let Some(mut bytes) = Iterator::next(&mut frames) {
                while !bytes.is_empty() {
                    let n = (need - head.len()).min(bytes.len());
                    head.extend_from_slice(&bytes.split_to(n));
                    if head.len() < need {
                        continue;
                    }
                    match decode_header(&head) {
                        Ok((h, _)) => {
                            let mut rest = vec![bytes];
                            rest.extend(frames);
                            rest.retain(|b| !b.is_empty());
                            return Ok(Opened::Hit(h, rest, Box::new(stream)));
                        }
                        // The fixed prefix is complete and valid: now read the JSON header
                        // (its length is at most MAX_HEADER_LEN, checked by decode_header).
                        Err(EnvelopeError::Incomplete) if need == PREFIX_LEN => {
                            let len = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
                            need = PREFIX_LEN + len as usize;
                        }
                        Err(_) => return Ok(Opened::Corrupt),
                    }
                }
            }
        }
    }

    /// Header of `key` via one bounded range read (never the body). Missing or corrupt
    /// objects both yield `None` (a corrupt object is a miss); use [`Store::get`] to tell them
    /// apart.
    pub async fn head(&self, key: &str) -> Result<Option<Header>, StoreError> {
        match self.open(key, (0..HEAD_READ_LEN).into()).await? {
            Opened::Hit(h, _, _) => Ok(Some(h)),
            Opened::Miss | Opened::Corrupt => Ok(None),
        }
    }

    /// One streaming read of the whole object: the header is parsed from its first bytes and
    /// the body is the rest of the same read, so a hit costs exactly one storage request.
    pub async fn get(&self, key: &str) -> Result<GetOutcome, StoreError> {
        let (h, rest, stream) = match self.open(key, (0..).into()).await? {
            Opened::Hit(h, rest, stream) => (h, rest, stream),
            Opened::Miss => return Ok(GetOutcome::Miss),
            Opened::Corrupt => return Ok(GetOutcome::Corrupt),
        };
        let tail = (*stream)
            .map_ok(|buf| futures::stream::iter(Iterator::map(buf, Ok::<_, opendal::Error>)))
            .try_flatten()
            .map_err(std::io::Error::other);
        let body = futures::stream::iter(rest.into_iter().map(Ok)).chain(tail);
        Ok(GetOutcome::Hit(h, Box::pin(body)))
    }

    /// Writes header then streams the body. Keys are immutable: an existing key is
    /// `AlreadyExists`, also when a backend without conditional writes (the GitHub cache)
    /// rejects a second create. A failing body stream aborts the write so no partial object
    /// remains. A backend rate limit is [`StoreError::RateLimited`].
    pub async fn put(
        &self,
        key: &str,
        header: &Header,
        body: impl Stream<Item = Result<Bytes, std::io::Error>> + Send,
    ) -> Result<PutOutcome, StoreError> {
        self.put_inner(key, header, body)
            .await
            .map_err(rate_limited)
    }

    async fn abort(&self, writer: &mut opendal::Writer) {
        if self.abort_failed {
            let _ = writer.abort().await;
        }
    }

    async fn put_inner(
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
        // A condition or "already exists" failure means the key exists: with if-not-exists
        // that is the condition, without it the backend refused a second create.
        let mut writer = match fut.await {
            Ok(w) => w,
            Err(e) if is_condition_failed(&e) => return Ok(PutOutcome::AlreadyExists),
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
            self.abort(&mut writer).await;
            if matches!(&e, StoreError::Storage(o) if is_condition_failed(o)) {
                return Ok(PutOutcome::AlreadyExists);
            }
            return Err(e);
        }
        match writer.close().await {
            Ok(_) => Ok(PutOutcome::Created),
            Err(e) => {
                // A rejected or failed completion leaves S3 multipart parts behind; drop them.
                self.abort(&mut writer).await;
                if is_condition_failed(&e) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn github_storage_requires_results_url() {
        for vars in [
            &[][..],
            &[("ACTIONS_RUNTIME_TOKEN", "tok")][..],
            &[
                ("ACTIONS_RESULTS_URL", ""),
                ("ACTIONS_RUNTIME_TOKEN", "tok"),
            ][..],
        ] {
            let err = ghac_operator_from("v1", env(vars)).unwrap_err().to_string();
            assert_eq!(
                err,
                "ACTIONS_RESULTS_URL is not set: run hoard through the GitHub Action, or use --storage r2|fs"
            );
        }
    }

    #[test]
    fn github_storage_requires_runtime_token() {
        for vars in [
            &[("ACTIONS_RESULTS_URL", "https://results.example/")][..],
            &[
                ("ACTIONS_RESULTS_URL", "https://results.example/"),
                ("ACTIONS_RUNTIME_TOKEN", ""),
            ][..],
        ] {
            let err = ghac_operator_from("v1", env(vars)).unwrap_err().to_string();
            assert_eq!(
                err,
                "ACTIONS_RUNTIME_TOKEN is not set: run hoard through the GitHub Action, or use --storage r2|fs"
            );
        }
    }

    #[test]
    fn github_operator_is_versioned_per_cache_version() {
        let vars = [
            ("ACTIONS_RESULTS_URL", "https://results.example/"),
            ("ACTIONS_RUNTIME_TOKEN", "secret-runtime-token"),
        ];
        let op = ghac_operator_from("v7", env(&vars)).unwrap();
        assert_eq!(op.info().scheme(), "ghac");
        // OpenDAL names the operator after the cache version; the v2 cache service hashes it.
        let name = op.info().name().to_string();
        let hashed: String = {
            use sha2::Digest;
            sha2::Sha256::digest(b"hoard-v7")
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        assert!(name == "hoard-v7" || name == hashed, "{name}");
        assert!(!format!("{op:?}").contains("secret-runtime-token"));
        // Keys are immutable on GitHub: no conditional writes, so `put` stats first.
        assert!(!op.info().capability().write_with_if_not_exists);
    }

    #[test]
    fn retry_after_comes_from_the_response_headers() {
        let err = |ctx: &str| {
            opendal::Error::new(ErrorKind::RateLimited, "slow down").with_context("response", ctx)
        };
        let parts = r#"Parts { status: 429, version: HTTP/1.1, headers: {"content-length": "0", "retry-after": "30"} }"#;
        assert_eq!(
            retry_after(&err(parts)),
            Some(std::time::Duration::from_secs(30))
        );
        let date =
            r#"Parts { status: 429, headers: {"retry-after": "Wed, 21 Oct 2015 07:28:00 GMT"} }"#;
        assert_eq!(retry_after(&err(date)), None);
        assert_eq!(
            retry_after(&err("Parts { status: 429, headers: {} }")),
            None
        );
    }
}
