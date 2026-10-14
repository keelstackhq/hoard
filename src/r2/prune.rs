//! Size cap (spec §7.2): delete the oldest objects until the prefix fits in `max-size`, at most
//! once per 24 h. The marker `{prefix}_hoard/last-prune` holds the last run's RFC 3339 time.
//! Everything under `{prefix}_hoard/` is hoard's own bookkeeping: never counted, never deleted.

use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use futures::StreamExt;
use opendal::ErrorKind;
use opendal::raw::normalize_path;
use tracing::warn;

use crate::config::Config;
use crate::redact::redacted;
use crate::serve::storage_layout;
use crate::stats::human_bytes;
use crate::store::{ObjectInfo, Store, StoreError, build_operator};

/// Minimum time between two unforced prunes.
const THROTTLE_HOURS: i64 = 24;

/// Deletes in flight at once: thousands of sequential round trips to R2 would take minutes.
const DELETE_CONCURRENCY: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// The marker is younger than 24 h and the run was not forced; nothing was listed.
    pub skipped_throttled: bool,
    /// Objects found under the prefix (excluding `{prefix}_hoard/`).
    pub listed: u64,
    pub deleted: u64,
    pub freed_bytes: u64,
    /// Bytes left under the prefix after the deletes (failed deletes still count).
    pub remaining_bytes: u64,
    /// Deletes that failed; the marker is not written then, so the next run retries.
    pub delete_errors: u64,
    /// The last delete error, for the log.
    pub last_error: Option<String>,
}

/// `{prefix}_hoard/last-prune`.
pub fn marker_key(prefix: &str) -> String {
    format!("{prefix}_hoard/last-prune")
}

/// Keys to delete so the total fits in `max_size`: oldest `last_modified` first, ties broken
/// by key, stopping as soon as the total is ≤ `max_size`.
pub fn plan_deletions(objects: &[ObjectInfo], max_size: u64) -> Vec<String> {
    plan(objects, max_size)
        .into_iter()
        .map(|o| o.key.clone())
        .collect()
}

/// The objects [`plan_deletions`] names, in deletion order.
fn plan(objects: &[ObjectInfo], max_size: u64) -> Vec<&ObjectInfo> {
    let mut total: u64 = objects.iter().map(|o| o.size).sum();
    let mut sorted: Vec<&ObjectInfo> = objects.iter().collect();
    sorted.sort_by(|a, b| (a.last_modified, &a.key).cmp(&(b.last_modified, &b.key)));
    let mut out = Vec::new();
    for o in sorted {
        if total <= max_size {
            break;
        }
        total -= o.size;
        out.push(o);
    }
    out
}

/// Whether the marker says a prune ran less than 24 h before `now`. A missing, unparseable
/// or future marker does not throttle.
async fn throttled(store: &Store, prefix: &str, now: DateTime<Utc>) -> Result<bool, StoreError> {
    let raw = match store.op.read(&marker_key(prefix)).await {
        Ok(b) => b.to_vec(),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let Some(last) = std::str::from_utf8(&raw)
        .ok()
        .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
    else {
        return Ok(false);
    };
    let age = now - last.with_timezone(&Utc);
    Ok(age >= Duration::zero() && age < Duration::hours(THROTTLE_HOURS))
}

/// Enforces `max_size` on `prefix` (spec §7.2). Listing, marker and throttle errors are
/// returned; a failed delete is counted in the report and the rest go on.
pub async fn prune(
    store: &Store,
    prefix: &str,
    max_size: u64,
    now: DateTime<Utc>,
    force: bool,
) -> Result<PruneReport, StoreError> {
    // Defence in depth (config validates r2-prefix): OpenDAL normalises paths, so `/` would
    // list the whole bucket and `cache//` would no longer match `{prefix}_hoard/`.
    let normal = prefix.ends_with('/')
        && normalize_path(prefix) == prefix
        && !prefix.split('/').any(|seg| seg == "." || seg == "..");
    if !normal || prefix == "/" {
        return Err(StoreError::BadPrefix(prefix.to_string()));
    }
    if !force && throttled(store, prefix, now).await? {
        return Ok(PruneReport {
            skipped_throttled: true,
            ..PruneReport::default()
        });
    }
    let internal = format!("{prefix}_hoard/");
    let objects: Vec<ObjectInfo> = store
        .list(prefix)
        .await?
        .into_iter()
        .filter(|o| !o.key.starts_with(&internal))
        .collect();
    let total: u64 = objects.iter().map(|o| o.size).sum();
    let doomed: Vec<(String, u64)> = plan(&objects, max_size)
        .into_iter()
        .map(|o| (o.key.clone(), o.size))
        .collect();

    let mut report = PruneReport {
        listed: objects.len() as u64,
        ..PruneReport::default()
    };
    // Owned futures (a cloned Store is a cheap handle): borrowing closures here make the
    // spawned serve future fail the `Send` check.
    let mut results = futures::stream::iter(doomed)
        .map(|(key, size)| {
            let store = store.clone();
            async move {
                let res = store.delete(&key).await;
                (key, size, res)
            }
        })
        .buffer_unordered(DELETE_CONCURRENCY);
    while let Some((key, size, res)) = results.next().await {
        match res {
            Ok(()) => {
                report.deleted += 1;
                report.freed_bytes += size;
            }
            Err(e) => {
                let e = redacted(&e);
                warn!(key = %key, error = %e, "prune: cannot delete object");
                report.delete_errors += 1;
                report.last_error = Some(e);
            }
        }
    }
    report.remaining_bytes = total - report.freed_bytes;

    if report.delete_errors == 0 {
        // A plain overwrite: the marker is the one key hoard rewrites.
        store
            .op
            .write(&marker_key(prefix), now.to_rfc3339())
            .await?;
    }
    Ok(report)
}

/// `hoard prune [--force]` (spec §4.1): prunes the configured storage now and prints one
/// line. Fails on a listing, marker or delete error.
pub async fn run_prune(cfg: &Config, force: bool) -> anyhow::Result<()> {
    let (prefix, _) = storage_layout(cfg)?;
    let store = Store::new(build_operator(cfg)?);
    let r = prune(&store, &prefix, cfg.max_size, Utc::now(), force).await?;
    if r.skipped_throttled {
        println!("skipped: last prune < 24 h ago (use --force)");
        return Ok(());
    }
    println!(
        "pruned {} of {} objects, freed {}, {} remain (max {})",
        r.deleted,
        r.listed,
        human_bytes(r.freed_bytes),
        human_bytes(r.remaining_bytes),
        human_bytes(cfg.max_size)
    );
    if r.delete_errors > 0 {
        bail!(
            "{} delete(s) failed (last: {}); run `hoard prune --force` again",
            r.delete_errors,
            r.last_error.as_deref().unwrap_or("unknown")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use opendal::{Operator, services};

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap()
    }

    fn hours(h: i64) -> Duration {
        Duration::hours(h)
    }

    fn obj(key: &str, size: u64, last_modified: DateTime<Utc>) -> ObjectInfo {
        ObjectInfo {
            key: key.into(),
            size,
            last_modified,
        }
    }

    struct FsStore {
        store: Store,
        _dir: tempfile::TempDir,
    }

    impl std::ops::Deref for FsStore {
        type Target = Store;
        fn deref(&self) -> &Store {
            &self.store
        }
    }

    async fn fs_store() -> FsStore {
        let dir = tempfile::tempdir().unwrap();
        let fs = services::Fs::default().root(dir.path().to_str().unwrap());
        FsStore {
            store: Store::new(Operator::new(fs).unwrap()),
            _dir: dir,
        }
    }

    /// Writes `n` raw objects of exactly `size` bytes under `prefix`, oldest first (a short
    /// sleep keeps the mtimes distinct; keys also sort in write order for the tie-break).
    async fn fill(s: &Store, prefix: &str, n: usize, size: usize) {
        for i in 0..n {
            s.op.write(&format!("{prefix}obj{i:03}"), vec![b'x'; size])
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
    }

    async fn keys(s: &Store, prefix: &str) -> Vec<String> {
        let mut k: Vec<String> = s
            .list(prefix)
            .await
            .unwrap()
            .into_iter()
            .map(|o| o.key)
            .collect();
        k.sort();
        k
    }

    #[test]
    fn deletes_oldest_until_under_cap() {
        let objs = vec![
            obj("a", 400, t(1)),
            obj("b", 400, t(3)),
            obj("c", 400, t(2)),
        ];
        assert_eq!(plan_deletions(&objs, 500), vec!["a", "c"]);
        assert!(plan_deletions(&objs, 1200).is_empty());
    }

    #[test]
    fn ties_break_by_key() {
        let objs = vec![obj("b", 10, t(1)), obj("a", 10, t(1)), obj("c", 10, t(0))];
        assert_eq!(plan_deletions(&objs, 10), vec!["c", "a"]);
        assert_eq!(plan_deletions(&objs, 0), vec!["c", "a", "b"]);
    }

    #[test]
    fn marker_key_layout() {
        assert_eq!(marker_key("cache/"), "cache/_hoard/last-prune");
    }

    #[tokio::test]
    async fn throttled_within_24h() {
        let s = fs_store().await;
        fill(&s, "cache/", 5, 100).await;
        let r1 = prune(&s, "cache/", 250, t0(), false).await.unwrap();
        assert_eq!((r1.deleted, r1.remaining_bytes), (3, 200));
        assert_eq!((r1.listed, r1.freed_bytes), (5, 300));
        assert_eq!(
            keys(&s, "cache/obj").await,
            vec!["cache/obj003", "cache/obj004"]
        );
        let r2 = prune(&s, "cache/", 0, t0() + hours(23), false)
            .await
            .unwrap();
        assert!(r2.skipped_throttled);
        let r3 = prune(&s, "cache/", 0, t0() + hours(23), true)
            .await
            .unwrap();
        assert_eq!(r3.remaining_bytes, 0);
        assert!(s.op.exists(&marker_key("cache/")).await.unwrap());
        let r4 = prune(&s, "cache/", 0, t0() + hours(46), false)
            .await
            .unwrap();
        assert!(r4.skipped_throttled, "marker rewritten by the forced run");
        let r5 = prune(&s, "cache/", 0, t0() + hours(47), false)
            .await
            .unwrap();
        assert!(!r5.skipped_throttled);
    }

    #[tokio::test]
    async fn marker_written_when_nothing_to_delete() {
        let s = fs_store().await;
        fill(&s, "cache/", 2, 100).await;
        let r = prune(&s, "cache/", 1000, t0(), false).await.unwrap();
        assert_eq!(
            (r.deleted, r.remaining_bytes, r.skipped_throttled),
            (0, 200, false)
        );
        let m = s.op.read(&marker_key("cache/")).await.unwrap().to_vec();
        assert_eq!(String::from_utf8(m).unwrap(), t0().to_rfc3339());
    }

    #[tokio::test]
    async fn hoard_internal_objects_are_not_counted_or_deleted() {
        let s = fs_store().await;
        fill(&s, "cache/", 3, 100).await;
        s.op.write("cache/_hoard/doctor", vec![b'd'; 1000])
            .await
            .unwrap();
        let r = prune(&s, "cache/", 0, t0(), true).await.unwrap();
        assert_eq!((r.listed, r.deleted, r.remaining_bytes), (3, 3, 0));
        assert!(s.op.exists("cache/_hoard/doctor").await.unwrap());
        // The marker (written now) is not counted either.
        let r = prune(&s, "cache/", 0, t0(), true).await.unwrap();
        assert_eq!((r.listed, r.deleted), (0, 0));
    }

    #[tokio::test]
    async fn garbage_or_future_marker_does_not_throttle() {
        let s = fs_store().await;
        fill(&s, "cache/", 1, 100).await;
        s.op.write(&marker_key("cache/"), "not a time")
            .await
            .unwrap();
        let r = prune(&s, "cache/", 1000, t0(), false).await.unwrap();
        assert!(!r.skipped_throttled);
        s.op.write(&marker_key("cache/"), (t0() + hours(1)).to_rfc3339())
            .await
            .unwrap();
        let r = prune(&s, "cache/", 0, t0(), false).await.unwrap();
        assert!(!r.skipped_throttled);
        assert_eq!(r.deleted, 1);
    }

    /// A prefix OpenDAL would rewrite (`/` = the whole bucket, `cache//` = `cache/`) is refused
    /// before anything is listed or deleted.
    #[tokio::test]
    async fn refuses_prefixes_opendal_would_normalise() {
        let s = fs_store().await;
        fill(&s, "cache/", 2, 100).await;
        fill(&s, "other/", 2, 100).await;
        s.op.write("cache/_hoard/doctor", vec![b'd'; 10])
            .await
            .unwrap();
        for bad in ["/", "", "cache//", " cache/", "/cache/", "a/../cache/"] {
            let err = prune(&s, bad, 0, t0(), true).await.unwrap_err();
            assert!(matches!(err, StoreError::BadPrefix(_)), "{bad:?}: {err}");
        }
        assert_eq!(keys(&s, "cache/").await.len(), 3);
        assert_eq!(keys(&s, "other/").await.len(), 2);
    }

    #[tokio::test]
    async fn other_prefixes_are_untouched() {
        let s = fs_store().await;
        fill(&s, "cache/", 2, 100).await;
        fill(&s, "other/", 2, 100).await;
        let r = prune(&s, "cache/", 0, t0(), false).await.unwrap();
        assert_eq!(r.deleted, 2);
        assert_eq!(keys(&s, "other/").await.len(), 2);
    }
}
