//! Vite+ task-cache restore and save (spec §4.1, §6.1). Vite+ remote caching is unreleased
//! upstream, so hoard saves and restores Vite Task's cache directory as one archive per run:
//! `{prefix}vp/{cache_version}/{os}-{arch}/{run_id}-{run_attempt}`. Restore picks the newest
//! archive for the same OS and architecture (task-cache paths are OS-specific): on r2/fs via
//! the pointer object `…/latest`, on the GitHub cache via `restore_keys`.

pub mod archive;
pub mod cli;
pub mod gha_twirp;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use opendal::ErrorKind;
use tracing::warn;

pub use archive::{archive_dir, dir_size, extract_archive, extract_stream};
pub use gha_twirp::{GhaTwirp, vp_version};

use crate::envelope::Header;
use crate::serve::GITHUB_PREFIX;
use crate::store::{GetOutcome, Store};

/// Vite Task's cache directory, relative to the workspace root (Vite+ 1.2.0).
pub const DEFAULT_CACHE_DIR: &str = "node_modules/.vite/task-cache";

/// Name of the pointer object holding the newest archive key (r2/fs).
pub const POINTER: &str = "latest";

/// The task-cache directory: `VITE_CACHE_PATH` when it is absolute (Vite+ requires an
/// absolute path there), else [`DEFAULT_CACHE_DIR`] under `workspace_root`.
pub fn vp_cache_dir(workspace_root: &Path, env: &HashMap<String, String>) -> PathBuf {
    match env.get("VITE_CACHE_PATH").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => workspace_root.join(DEFAULT_CACHE_DIR),
    }
}

/// `{os}-{arch}` with Node-style names (`linux-x64`, `darwin-arm64`, `windows-x64`, …), so the
/// action and the binary agree.
pub fn default_os_arch() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// `{GITHUB_RUN_ID}-{GITHUB_RUN_ATTEMPT}` when both are set, else `local-{unix secs}-{random}`.
pub fn default_run_key(lookup: impl Fn(&str) -> Option<String>) -> String {
    let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
    if let (Some(id), Some(attempt)) = (var("GITHUB_RUN_ID"), var("GITHUB_RUN_ATTEMPT")) {
        return format!("{id}-{attempt}");
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let rand: String = rand::random::<[u8; 4]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("local-{secs}-{rand}")
}

/// `{prefix}vp/{cache_version}/{os_arch}/`: every archive of one OS and architecture.
pub fn key_prefix(prefix: &str, cache_version: &str, os_arch: &str) -> String {
    format!("{prefix}vp/{cache_version}/{os_arch}/")
}

/// Where Vite+ archives go.
pub enum VpBackend {
    /// r2 or fs: archives are HRD1 envelopes, the newest is named by the pointer object.
    Store { store: Arc<Store>, prefix: String },
    /// The GitHub Actions cache: the newest is found with `restore_keys`.
    Github(GhaTwirp),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// `bytes` is the archive size.
    Restored {
        key: String,
        bytes: u64,
    },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// `bytes` is the archive size. An archive that already existed under `key` is success.
    Saved { key: String, bytes: u64 },
    /// The directory's files add up to `bytes`, more than `vp-max-size`.
    SkippedTooLarge { bytes: u64 },
    /// The directory is missing or has no files.
    SkippedEmpty,
}

/// A temp file removed on drop (the spooled archive).
struct Spool(PathBuf);

impl Spool {
    fn new() -> Self {
        let name: String = rand::random::<[u8; 8]>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Self(std::env::temp_dir().join(format!("hoard-vp-{name}.tar.zst")))
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The file at `path` as a stream of chunks.
fn file_stream(
    file: tokio::fs::File,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    futures::stream::try_unfold(file, |mut f| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 256 * 1024];
        let n = f.read(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.truncate(n);
        Ok(Some((Bytes::from(buf), f)))
    })
}

/// `body`, failing at the end unless it had exactly `size` bytes.
fn exact_len(
    body: impl Stream<Item = std::io::Result<Bytes>> + Send,
    size: u64,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send {
    let mut seen = 0u64;
    body.map(Some)
        .chain(futures::stream::iter([None]))
        .filter_map(move |item| {
            let out = match item {
                Some(Ok(b)) => {
                    seen += b.len() as u64;
                    Some(Ok(b))
                }
                Some(Err(e)) => Some(Err(e)),
                None if seen != size => Some(Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("archive has {seen} bytes, expected {size}"),
                ))),
                None => None,
            };
            std::future::ready(out)
        })
}

fn random_suffix() -> String {
    rand::random::<[u8; 6]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Extracts `body` into a new directory next to `dir`, then swaps it in place of `dir`. A
/// failed extraction removes the new directory and leaves `dir` as it was.
async fn restore_into(
    dir: &Path,
    body: impl Stream<Item = std::io::Result<Bytes>> + Send,
) -> anyhow::Result<()> {
    let name = dir
        .file_name()
        .with_context(|| format!("{} has no directory name", dir.display()))?
        .to_string_lossy()
        .into_owned();
    let parent = match dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    tokio::fs::create_dir_all(&parent)
        .await
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let fresh = parent.join(format!(".{name}.hoard-restore-{}", random_suffix()));
    tokio::fs::create_dir(&fresh)
        .await
        .with_context(|| format!("cannot create {}", fresh.display()))?;
    if let Err(e) = extract_stream(body, &fresh).await {
        let _ = tokio::fs::remove_dir_all(&fresh).await;
        return Err(anyhow::Error::new(e).context("cannot extract the Vite+ cache archive"));
    }
    let old = parent.join(format!(".{name}.hoard-old-{}", random_suffix()));
    let had_old = match tokio::fs::rename(dir, &old).await {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&fresh).await;
            return Err(anyhow::Error::new(e).context(format!("cannot replace {}", dir.display())));
        }
    };
    if let Err(e) = tokio::fs::rename(&fresh, dir).await {
        if had_old {
            let _ = tokio::fs::rename(&old, dir).await;
        }
        let _ = tokio::fs::remove_dir_all(&fresh).await;
        return Err(anyhow::Error::new(e).context(format!("cannot replace {}", dir.display())));
    }
    if had_old && let Err(e) = tokio::fs::remove_dir_all(&old).await {
        warn!(path = %old.display(), error = %e, "cannot remove the previous Vite+ cache");
    }
    Ok(())
}

/// Restores the newest archive for `os_arch` into `dir`, replacing what is there. No archive
/// (a new branch, another OS, a pointer to a deleted archive) is `NotFound`.
pub async fn vp_restore(
    backend: &VpBackend,
    cache_version: &str,
    os_arch: &str,
    dir: &Path,
) -> anyhow::Result<RestoreOutcome> {
    match backend {
        VpBackend::Store { store, prefix } => {
            let pfx = key_prefix(prefix, cache_version, os_arch);
            let raw = match store.op.read(&format!("{pfx}{POINTER}")).await {
                Ok(b) => b.to_vec(),
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(RestoreOutcome::NotFound),
                Err(e) => return Err(e.into()),
            };
            let key = String::from_utf8_lossy(&raw).trim().to_string();
            // The pointer may only name an archive of this OS and cache version.
            let valid = key
                .strip_prefix(&pfx)
                .is_some_and(|run| crate::config::valid_segment(run) && run != POINTER);
            if !valid {
                warn!(pointer = %key, "ignoring a Vite+ cache pointer outside {pfx}");
                return Ok(RestoreOutcome::NotFound);
            }
            match store.get(&key).await? {
                GetOutcome::Hit(h, body) => {
                    restore_into(dir, exact_len(body, h.size)).await?;
                    Ok(RestoreOutcome::Restored { key, bytes: h.size })
                }
                GetOutcome::Miss | GetOutcome::Corrupt => Ok(RestoreOutcome::NotFound),
            }
        }
        VpBackend::Github(g) => {
            let pfx = key_prefix(GITHUB_PREFIX, cache_version, os_arch);
            let version = vp_version(cache_version);
            let Some((key, url)) = g
                .get_download_url(&pfx, std::slice::from_ref(&pfx), &version)
                .await?
            else {
                return Ok(RestoreOutcome::NotFound);
            };
            if !key.starts_with(&pfx) {
                return Ok(RestoreOutcome::NotFound);
            }
            let (len, body) = g.download(&url).await?;
            let mut seen = 0u64;
            let body = body.inspect(|c| {
                if let Ok(b) = c {
                    seen += b.len() as u64;
                }
            });
            match len {
                Some(n) => restore_into(dir, exact_len(body, n)).await?,
                None => restore_into(dir, body).await?,
            }
            Ok(RestoreOutcome::Restored {
                key,
                bytes: len.unwrap_or(seen),
            })
        }
    }
}

/// Archives `dir` and stores it as `{prefix}vp/{cache_version}/{os_arch}/{run_key}`, unless it
/// is empty or its files add up to more than `max_size`.
pub async fn vp_save(
    backend: &VpBackend,
    cache_version: &str,
    os_arch: &str,
    run_key: &str,
    dir: &Path,
    max_size: u64,
) -> anyhow::Result<SaveOutcome> {
    let owned = dir.to_path_buf();
    let (bytes, files) = tokio::task::spawn_blocking(move || dir_size(&owned))
        .await?
        .with_context(|| format!("cannot read {}", dir.display()))?;
    if files == 0 {
        return Ok(SaveOutcome::SkippedEmpty);
    }
    if bytes > max_size {
        return Ok(SaveOutcome::SkippedTooLarge { bytes });
    }
    let spool = Spool::new();
    let mut file = tokio::fs::File::create(&spool.0)
        .await
        .with_context(|| format!("cannot create {}", spool.0.display()))?;
    let size = archive_dir(dir, &mut file)
        .await
        .context("cannot archive the Vite+ cache")?;
    drop(file);
    match backend {
        VpBackend::Store { store, prefix } => {
            let pfx = key_prefix(prefix, cache_version, os_arch);
            let key = format!("{pfx}{run_key}");
            let header = Header {
                v: 1,
                duration_ms: None,
                tag: None,
                size,
            };
            let body = file_stream(tokio::fs::File::open(&spool.0).await?);
            store.put(&key, &header, body).await?; // an existing key is success
            store
                .op
                .write(&format!("{pfx}{POINTER}"), key.clone())
                .await?;
            Ok(SaveOutcome::Saved { key, bytes: size })
        }
        VpBackend::Github(g) => {
            let key = format!(
                "{}{run_key}",
                key_prefix(GITHUB_PREFIX, cache_version, os_arch)
            );
            let version = vp_version(cache_version);
            if let Some(url) = g.create_entry(&key, &version).await? {
                let sent = g.upload_blocks(&url, &spool.0).await?;
                g.finalize(&key, &version, sent).await?;
            }
            Ok(SaveOutcome::Saved { key, bytes: size })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_defaults_under_the_workspace() {
        let root = Path::new("/work/repo");
        let mut env = HashMap::new();
        assert_eq!(
            vp_cache_dir(root, &env),
            root.join("node_modules/.vite/task-cache")
        );
        env.insert("VITE_CACHE_PATH".into(), "relative/cache".into());
        assert_eq!(
            vp_cache_dir(root, &env),
            root.join("node_modules/.vite/task-cache")
        );
        let abs = std::env::temp_dir().join("vp-cache");
        env.insert("VITE_CACHE_PATH".into(), abs.to_string_lossy().into());
        assert_eq!(vp_cache_dir(root, &env), abs);
    }

    #[test]
    fn os_arch_uses_node_names() {
        let s = default_os_arch();
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(s, "linux-x64");
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(s, "darwin-arm64");
        assert!(crate::config::valid_segment(&s), "{s}");
    }

    #[test]
    fn run_key_from_github_env() {
        let env = |k: &str| match k {
            "GITHUB_RUN_ID" => Some("123".to_string()),
            "GITHUB_RUN_ATTEMPT" => Some("2".to_string()),
            _ => None,
        };
        assert_eq!(default_run_key(env), "123-2");
        let local = default_run_key(|_| None);
        assert!(local.starts_with("local-"), "{local}");
        assert!(crate::config::valid_segment(&local));
        assert_ne!(local, default_run_key(|_| None));
    }

    #[test]
    fn keys_follow_the_layout() {
        assert_eq!(
            key_prefix("cache/", "v1", "linux-x64"),
            "cache/vp/v1/linux-x64/"
        );
    }
}
