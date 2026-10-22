//! Where packs and indexes live: the GitHub cache (Twirp client) or an OpenDAL operator (fs).
//!
//! Errors never contain a signed URL or a token.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use opendal::{ErrorKind, Operator};
use tokio::io::AsyncReadExt;

use crate::config::{Config, Storage};
use crate::gha_twirp::{GhaError, GhaTwirp, pack_version};
use crate::redact::redacted;
use crate::store::{BodyStream, is_condition_failed, retry_after};

/// Chunk size for uploads from a file to an OpenDAL writer.
const UPLOAD_CHUNK: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// The backend rate-limited the request (GitHub cache: HTTP 429).
    #[error("storage rate limit hit (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },
    /// The key exists already (pack and index keys are single-use).
    #[error("the key exists already")]
    Exists,
    /// Anything else; redacted, without URLs.
    #[error("{0}")]
    Other(String),
}

impl From<GhaError> for PackError {
    fn from(e: GhaError) -> Self {
        match e {
            GhaError::RateLimited(_) => Self::RateLimited { retry_after: None },
            e => Self::Other(redacted(&e)),
        }
    }
}

impl From<opendal::Error> for PackError {
    fn from(e: opendal::Error) -> Self {
        if e.kind() == ErrorKind::RateLimited {
            Self::RateLimited {
                retry_after: retry_after(&e),
            }
        } else {
            Self::Other(redacted(&e))
        }
    }
}

fn other(e: impl std::fmt::Display) -> PackError {
    PackError::Other(redacted(&e))
}

pub enum PackBackend {
    /// An OpenDAL operator (fs; also usable with other services).
    Store(Operator),
    Github {
        client: GhaTwirp,
        /// Cache entry version: SHA-256 of `hoard-pack-{cache_version}`.
        version: String,
        /// Signed download URL per pack key, resolved lazily. The lock is held while a URL is
        /// resolved, so concurrent first reads of one pack make one lookup.
        urls: tokio::sync::Mutex<HashMap<String, String>>,
    },
}

impl PackBackend {
    pub fn github(client: GhaTwirp, cache_version: &str) -> Self {
        Self::Github {
            client,
            version: pack_version(cache_version),
            urls: Default::default(),
        }
    }

    /// The backend for `cfg.storage`: the cache service (`github`, from the process
    /// environment) or the fs operator. `r2` is a setup error: packing ignores it.
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        match cfg.storage {
            Storage::Github => Ok(Self::github(
                GhaTwirp::from_env(|k| std::env::var(k).ok())?,
                &cfg.cache_version,
            )),
            Storage::Fs => Ok(Self::Store(crate::store::build_operator(cfg)?)),
            Storage::R2 => anyhow::bail!("pack is not supported for storage=r2: use pack=off"),
        }
    }

    /// The newest object under `prefix` and its compressed bytes (at most `max_bytes`; a
    /// larger one is an error). github: the newest entry whose key starts with `prefix`
    /// (current branch scope, then the default branch); other stores: the object with the
    /// latest (last-modified, key) anywhere below `prefix`.
    pub async fn newest(
        &self,
        prefix: &str,
        max_bytes: u64,
    ) -> Result<Option<(String, Vec<u8>)>, PackError> {
        match self {
            Self::Github {
                client, version, ..
            } => {
                let Some((key, url)) = client
                    .get_download_url(prefix, &[prefix.to_string()], version)
                    .await?
                else {
                    return Ok(None);
                };
                let (len, body) = client.download(&url).await?;
                if len.is_some_and(|l| l > max_bytes) {
                    return Err(PackError::Other(format!(
                        "{key} is larger than {max_bytes} bytes"
                    )));
                }
                let mut out = Vec::new();
                let mut body = std::pin::pin!(body);
                while let Some(chunk) = body.try_next().await.map_err(other)? {
                    if (out.len() + chunk.len()) as u64 > max_bytes {
                        return Err(PackError::Other(format!(
                            "{key} is larger than {max_bytes} bytes"
                        )));
                    }
                    out.extend_from_slice(&chunk);
                }
                Ok(Some((key, out)))
            }
            Self::Store(op) => {
                let entries = match op.list_with(prefix).recursive(true).await {
                    Ok(e) => e,
                    Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e.into()),
                };
                let mut best: Option<(std::time::SystemTime, String, u64)> = None;
                for entry in entries {
                    if entry.metadata().is_dir() {
                        continue;
                    }
                    let key = entry.path().to_string();
                    let mut meta = entry.metadata().clone();
                    if meta.last_modified().is_none() || !meta.is_file() {
                        meta = match op.stat(&key).await {
                            Ok(m) => m,
                            Err(e) if e.kind() == ErrorKind::NotFound => continue,
                            Err(e) => return Err(e.into()),
                        };
                    }
                    let modified = meta
                        .last_modified()
                        .map(std::time::SystemTime::from)
                        .unwrap_or(std::time::UNIX_EPOCH);
                    let cand = (modified, key, meta.content_length());
                    if best
                        .as_ref()
                        .is_none_or(|b| (&cand.0, &cand.1) > (&b.0, &b.1))
                    {
                        best = Some(cand);
                    }
                }
                let Some((_, key, size)) = best else {
                    return Ok(None);
                };
                if size > max_bytes {
                    return Err(PackError::Other(format!(
                        "{key} is larger than {max_bytes} bytes"
                    )));
                }
                match op.read(&key).await {
                    Ok(buf) => Ok(Some((key, buf.to_vec()))),
                    Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    /// Uploads the file at `path` as `key` unless `key` exists (then [`PackError::Exists`]).
    /// Returns the number of bytes uploaded.
    pub async fn upload(&self, key: &str, path: &Path) -> Result<u64, PackError> {
        match self {
            Self::Github {
                client, version, ..
            } => {
                let Some(url) = client.create_entry(key, version).await? else {
                    return Err(PackError::Exists);
                };
                let n = client.upload_blocks(&url, path).await?;
                client.finalize(key, version, n).await?;
                Ok(n)
            }
            Self::Store(op) => upload_to_store(op, key, path).await,
        }
    }

    /// One streaming read of `len` bytes at `offset` of the pack `key`. `Ok(None)` only when
    /// the pack is reported not found.
    pub async fn read_range(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> Result<Option<BodyStream>, PackError> {
        match self {
            Self::Github {
                client,
                version,
                urls,
            } => {
                let mut stale: Option<String> = None;
                for attempt in 0..2 {
                    let Some(url) = resolve(client, version, urls, key, stale.take()).await? else {
                        return Ok(None);
                    };
                    match client.download_range(&url, offset, len).await {
                        Ok(s) => return Ok(Some(Box::pin(s))),
                        Err(GhaError::Status {
                            status: 401 | 403, ..
                        }) if attempt == 0 => stale = Some(url),
                        Err(GhaError::Status { status: 404, .. }) => {
                            urls.lock().await.remove(key);
                            return Ok(None);
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                unreachable!("the second attempt always returns")
            }
            Self::Store(op) => {
                let end = offset
                    .checked_add(len)
                    .ok_or_else(|| PackError::Other("invalid range".into()))?;
                let reader = match op.reader(key).await {
                    Ok(r) => r,
                    Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e.into()),
                };
                let mut stream = match reader.into_stream(offset..end).await {
                    Ok(s) => s,
                    Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e.into()),
                };
                // The object may only turn out missing on the first read.
                let first = match stream.next().await {
                    Some(Err(e)) if e.kind() == ErrorKind::NotFound => return Ok(None),
                    Some(Err(e)) => return Err(e.into()),
                    first => first,
                };
                let flat = futures::stream::iter(first)
                    .chain(stream)
                    .map_ok(|buf| {
                        futures::stream::iter(Iterator::map(buf, Ok::<Bytes, opendal::Error>))
                    })
                    .try_flatten()
                    .map_err(std::io::Error::other);
                Ok(Some(Box::pin(flat)))
            }
        }
    }
}

/// The signed URL of pack `key`: cached, else resolved with the exact key. `stale` is a URL
/// that just failed: a cached URL equal to it is dropped (another read may have refreshed it).
async fn resolve(
    client: &GhaTwirp,
    version: &str,
    urls: &tokio::sync::Mutex<HashMap<String, String>>,
    key: &str,
    stale: Option<String>,
) -> Result<Option<String>, PackError> {
    let mut urls = urls.lock().await;
    if let Some(cur) = urls.get(key) {
        if stale.as_deref() != Some(cur.as_str()) {
            return Ok(Some(cur.clone()));
        }
        urls.remove(key);
    }
    let Some((_, url)) = client.get_download_url(key, &[], version).await? else {
        return Ok(None);
    };
    urls.insert(key.to_string(), url.clone());
    Ok(Some(url))
}

/// A failed write: the key exists (the if-not-exists condition may fail on any call), or not.
fn from_write_error(e: opendal::Error) -> PackError {
    if is_condition_failed(&e) {
        PackError::Exists
    } else {
        e.into()
    }
}

async fn upload_to_store(op: &Operator, key: &str, path: &Path) -> Result<u64, PackError> {
    let conditional = op.info().capability().write_with_if_not_exists;
    if !conditional {
        match op.stat(key).await {
            Ok(_) => return Err(PackError::Exists),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut file = tokio::fs::File::open(path).await.map_err(other)?;
    let mut writer = op
        .writer_with(key)
        .if_not_exists(conditional)
        .await
        .map_err(from_write_error)?;
    let copied: Result<u64, PackError> = async {
        let mut total = 0u64;
        loop {
            let mut buf = Vec::with_capacity(UPLOAD_CHUNK);
            (&mut file)
                .take(UPLOAD_CHUNK as u64)
                .read_to_end(&mut buf)
                .await
                .map_err(other)?;
            if buf.is_empty() {
                return Ok(total);
            }
            total += buf.len() as u64;
            writer
                .write(Bytes::from(buf))
                .await
                .map_err(from_write_error)?;
        }
    }
    .await;
    let total = match copied {
        Ok(t) => t,
        Err(e) => {
            let _ = writer.abort().await;
            return Err(e);
        }
    };
    match writer.close().await {
        Ok(_) => Ok(total),
        Err(e) => {
            let _ = writer.abort().await;
            Err(from_write_error(e))
        }
    }
}
