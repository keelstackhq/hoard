//! A small client for the GitHub Actions cache service v2 (Twirp, JSON) and the Azure blob
//! URLs it hands out, used for Vite+ archives on `storage=github`.
//!
//! Save: `CreateCacheEntry` → Put Block (32 MiB blocks) + Put Block List on the signed upload
//! URL → `FinalizeCacheEntryUpload`. Restore: `GetCacheEntryDownloadURL` with `restore_keys`
//! (the service returns the newest entry whose key starts with a restore key) → one streaming
//! GET. The v2 service is used even when `ACTIONS_CACHE_SERVICE_V2` is unset: the v1 service
//! is being retired and every github.com runner serves v2 at `ACTIONS_RESULTS_URL`.
//!
//! Signed URLs carry a SAS (`sig=`): no error or log line ever contains a URL; reqwest errors
//! are stripped of theirs. The bearer token is only sent to the Twirp service, never to blob
//! URLs.

use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use futures::{Stream, TryStreamExt};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tokio::io::AsyncReadExt;

/// Twirp service path under `ACTIONS_RESULTS_URL`.
const SERVICE: &str = "twirp/github.actions.results.api.v1.CacheService";

/// Size of one Put Block request.
pub const BLOCK_SIZE: usize = 32 * 1024 * 1024;

/// Azure Storage REST API version sent with blob requests.
const AZURE_VERSION: &str = "2025-01-05";

/// Fix shown when the GitHub Actions cache environment is incomplete (same as `serve`).
const GHAC_HINT: &str = "run hoard through the GitHub Action, or use --storage r2|fs";

const TWIRP_TIMEOUT: Duration = Duration::from_secs(60);
const BLOCK_TIMEOUT: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest pause between two reads of a response body (downloads have no total bound).
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors from the cache service or blob storage. Messages never contain a URL or token.
#[derive(Debug, thiserror::Error)]
pub enum GhaError {
    #[error("GitHub cache rate limit hit (HTTP 429) in {0}")]
    RateLimited(&'static str),
    #[error("GitHub cache {what} failed: HTTP {status}")]
    Status { what: &'static str, status: u16 },
    #[error("GitHub cache {what} failed: {source}")]
    Http {
        what: &'static str,
        source: reqwest::Error,
    },
    #[error("GitHub cache {what}: {msg}")]
    Protocol { what: &'static str, msg: String },
    #[error("cannot read the archive: {0}")]
    Io(#[from] std::io::Error),
}

fn http(what: &'static str) -> impl FnOnce(reqwest::Error) -> GhaError {
    move |e| GhaError::Http {
        what,
        source: e.without_url(),
    }
}

/// The cache entry version for Vite+ archives: SHA-256 (hex) of `hoard-vp-{cache_version}`.
/// Save and restore must agree on it; entries of other versions are invisible.
pub fn vp_version(cache_version: &str) -> String {
    sha2::Sha256::digest(format!("hoard-vp-{cache_version}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Serialize)]
struct CreateReq<'a> {
    key: &'a str,
    version: &'a str,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateResp {
    ok: bool,
    #[serde(alias = "signedUploadUrl")]
    signed_upload_url: String,
}

#[derive(Serialize)]
struct FinalizeReq<'a> {
    key: &'a str,
    /// int64: protobuf JSON accepts it as a string.
    size_bytes: String,
    version: &'a str,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FinalizeResp {
    ok: bool,
}

#[derive(Serialize)]
struct GetReq<'a> {
    key: &'a str,
    restore_keys: &'a [String],
    version: &'a str,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GetResp {
    ok: bool,
    #[serde(alias = "signedDownloadUrl")]
    signed_download_url: String,
    #[serde(alias = "matchedKey")]
    matched_key: String,
}

/// Client for the GitHub Actions cache service v2.
#[derive(Clone)]
pub struct GhaTwirp {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for GhaTwirp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhaTwirp").finish_non_exhaustive()
    }
}

/// Standard base64 (with padding).
fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Block id of block `i`: base64 of a fixed-length (12-byte) name, so all ids have the same
/// length as Azure requires, and no `=` padding.
fn block_id(i: usize) -> String {
    base64(format!("hoard{i:07}").as_bytes())
}

/// `url` with `extra` (already encoded) appended to its query.
fn with_query(url: &str, extra: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{extra}")
}

/// Percent-encodes the base64 characters that are not safe in a query value.
fn query_escape(s: &str) -> String {
    s.replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D")
}

impl GhaTwirp {
    /// A client for the service at `base_url` (`ACTIONS_RESULTS_URL`), authenticating with
    /// `token` (`ACTIONS_RUNTIME_TOKEN`).
    pub fn new(base_url: &str, token: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|e| anyhow::anyhow!("cannot build the HTTP client: {}", e.without_url()))?;
        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            client,
        })
    }

    /// The client configured from `ACTIONS_RESULTS_URL` and `ACTIONS_RUNTIME_TOKEN`, read
    /// through `lookup` (empty counts as unset). Missing either is a setup error.
    pub fn from_env(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
        let need = |k: &str| var(k).ok_or_else(|| anyhow::anyhow!("{k} is not set: {GHAC_HINT}"));
        let base = need("ACTIONS_RESULTS_URL")?;
        let token = need("ACTIONS_RUNTIME_TOKEN")?;
        Self::new(&base, &token)
    }

    async fn call<Req: Serialize, Resp: for<'de> Deserialize<'de> + Default>(
        &self,
        what: &'static str,
        req: &Req,
    ) -> Result<Resp, GhaError> {
        let resp = self
            .client
            .post(format!("{}/{SERVICE}/{what}", self.base))
            .bearer_auth(&self.token)
            .timeout(TWIRP_TIMEOUT)
            .json(req)
            .send()
            .await
            .map_err(http(what))?;
        match resp.status().as_u16() {
            200..=299 => resp.json::<Resp>().await.map_err(http(what)),
            429 => Err(GhaError::RateLimited(what)),
            status => Err(GhaError::Status { what, status }),
        }
    }

    /// Reserves `key`; returns the signed upload URL, or `None` when the key already exists
    /// (or another job is creating it): keys are immutable, so that is success.
    pub async fn create_entry(&self, key: &str, version: &str) -> Result<Option<String>, GhaError> {
        let what = "CreateCacheEntry";
        match self
            .call::<_, CreateResp>(what, &CreateReq { key, version })
            .await
        {
            Ok(r) if r.ok && !r.signed_upload_url.is_empty() => Ok(Some(r.signed_upload_url)),
            Ok(r) if !r.ok => Ok(None),
            Ok(_) => Err(GhaError::Protocol {
                what,
                msg: "no upload URL in the response".into(),
            }),
            Err(GhaError::Status { status: 409, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Uploads the file at `path` to the signed `url` as Put Block requests of
    /// [`BLOCK_SIZE`] (one block in memory at a time) and one Put Block List. Returns the
    /// number of bytes uploaded.
    pub async fn upload_blocks(&self, url: &str, path: &Path) -> Result<u64, GhaError> {
        let mut file = tokio::fs::File::open(path).await?;
        let mut ids = Vec::new();
        let mut total = 0u64;
        loop {
            let mut block = Vec::with_capacity(BLOCK_SIZE);
            (&mut file)
                .take(BLOCK_SIZE as u64)
                .read_to_end(&mut block)
                .await?;
            if block.is_empty() && !ids.is_empty() {
                break;
            }
            let id = block_id(ids.len());
            let n = block.len();
            let what = "Put Block";
            let resp = self
                .client
                .put(with_query(
                    url,
                    &format!("comp=block&blockid={}", query_escape(&id)),
                ))
                .header("x-ms-version", AZURE_VERSION)
                .timeout(BLOCK_TIMEOUT)
                .body(block)
                .send()
                .await
                .map_err(http(what))?;
            blob_status(what, resp.status())?;
            ids.push(id);
            total += n as u64;
            if n < BLOCK_SIZE {
                break;
            }
        }
        let mut xml = String::from(r#"<?xml version="1.0" encoding="utf-8"?><BlockList>"#);
        for id in &ids {
            xml.push_str(&format!("<Latest>{id}</Latest>"));
        }
        xml.push_str("</BlockList>");
        let what = "Put Block List";
        let resp = self
            .client
            .put(with_query(url, "comp=blocklist"))
            .header("x-ms-version", AZURE_VERSION)
            .header("content-type", "application/xml")
            .timeout(TWIRP_TIMEOUT)
            .body(xml)
            .send()
            .await
            .map_err(http(what))?;
        blob_status(what, resp.status())?;
        Ok(total)
    }

    /// Commits the uploaded entry.
    pub async fn finalize(&self, key: &str, version: &str, size: u64) -> Result<(), GhaError> {
        let what = "FinalizeCacheEntryUpload";
        let req = FinalizeReq {
            key,
            size_bytes: size.to_string(),
            version,
        };
        let r: FinalizeResp = self.call(what, &req).await?;
        if !r.ok {
            return Err(GhaError::Protocol {
                what,
                msg: "the service did not accept the upload".into(),
            });
        }
        Ok(())
    }

    /// The entry for `key`, else the newest entry whose key starts with one of
    /// `restore_keys`: `(matched key, signed download URL)`, or `None` if there is none.
    pub async fn get_download_url(
        &self,
        key: &str,
        restore_keys: &[String],
        version: &str,
    ) -> Result<Option<(String, String)>, GhaError> {
        let what = "GetCacheEntryDownloadURL";
        let req = GetReq {
            key,
            restore_keys,
            version,
        };
        match self.call::<_, GetResp>(what, &req).await {
            Ok(r) if r.ok && !r.signed_download_url.is_empty() => {
                let matched = if r.matched_key.is_empty() {
                    key.to_string()
                } else {
                    r.matched_key
                };
                Ok(Some((matched, r.signed_download_url)))
            }
            Ok(_) | Err(GhaError::Status { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// One streaming GET of a signed download URL: `(content length, body)`.
    pub async fn download(
        &self,
        url: &str,
    ) -> Result<
        (
            Option<u64>,
            impl Stream<Item = std::io::Result<Bytes>> + Send + use<>,
        ),
        GhaError,
    > {
        let what = "download";
        let resp = self
            .client
            .get(url)
            .header("x-ms-version", AZURE_VERSION)
            .send()
            .await
            .map_err(http(what))?;
        blob_status(what, resp.status())?;
        let len = resp.content_length();
        let body = resp
            .bytes_stream()
            .map_err(|e| std::io::Error::other(e.without_url()));
        Ok((len, body))
    }
}

fn blob_status(what: &'static str, status: reqwest::StatusCode) -> Result<(), GhaError> {
    match status.as_u16() {
        200..=299 => Ok(()),
        429 | 503 => Err(GhaError::RateLimited(what)),
        status => Err(GhaError::Status { what, status }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
    }

    #[test]
    fn block_ids_have_equal_length_and_no_padding() {
        let ids: Vec<_> = [0, 1, 99, 9_999_999].map(block_id).into();
        assert!(
            ids.iter().all(|i| i.len() == 16 && !i.contains('=')),
            "{ids:?}"
        );
        assert_eq!(ids[0], base64(b"hoard0000000"));
    }

    #[test]
    fn version_is_sha256_of_a_stable_string() {
        let v = vp_version("v1");
        assert_eq!(v.len(), 64);
        assert_eq!(v, vp_version("v1"));
        assert_ne!(v, vp_version("v2"));
    }

    #[test]
    fn missing_env_is_a_setup_error() {
        let err = GhaTwirp::from_env(|_| None).unwrap_err().to_string();
        assert_eq!(
            err,
            "ACTIONS_RESULTS_URL is not set: run hoard through the GitHub Action, or use --storage r2|fs"
        );
        let err = GhaTwirp::from_env(|k| (k == "ACTIONS_RESULTS_URL").then(|| "http://x/".into()))
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("ACTIONS_RUNTIME_TOKEN is not set"), "{err}");
    }

    #[test]
    fn debug_hides_the_token() {
        let g = GhaTwirp::new("http://127.0.0.1:1/", "secret-token-xyz").unwrap();
        assert!(!format!("{g:?}").contains("secret-token-xyz"));
    }
}
