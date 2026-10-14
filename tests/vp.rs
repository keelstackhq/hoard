//! `vp restore` / `vp save` (spec §4.1, §6.1) on the fs store, on MinIO (as R2) and on a mock
//! of the GitHub cache service; plus the `hoard vp` binary.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use common::{fs_config, gha_mock, minio_bucket, r2_config, s3_runtime};
use hoard::envelope::Header;
use hoard::store::{Store, build_operator};
use hoard::vp::{
    GhaTwirp, RestoreOutcome, SaveOutcome, VpBackend, default_os_arch, default_run_key, vp_restore,
    vp_save, vp_version,
};

const V: &str = "v1";
const LINUX: &str = "linux-x64";
const MAC: &str = "darwin-arm64";
const BIG: u64 = 1 << 30;

fn write_tree(dir: &Path, files: &[(&str, &[u8])]) {
    for (name, data) in files {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
}

fn read_tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn go(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                go(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    go(dir, dir, &mut out);
    out
}

fn tree(files: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
    files
        .iter()
        .map(|(n, d)| (n.to_string(), d.to_vec()))
        .collect()
}

/// A task-cache directory with `files`, inside a fresh workspace tempdir.
fn cache_with(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("node_modules/.vite/task-cache");
    write_tree(&dir, files);
    (ws, dir)
}

/// An empty target: a fresh workspace without a task cache.
fn empty_target() -> (tempfile::TempDir, PathBuf) {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("node_modules/.vite/task-cache");
    (ws, dir)
}

fn fs_backend(root: &Path) -> (VpBackend, Arc<Store>) {
    let store = Arc::new(Store::new(build_operator(&fs_config(root)).unwrap()));
    let backend = VpBackend::Store {
        store: store.clone(),
        prefix: "cache/".into(),
    };
    (backend, store)
}

fn saved_key(o: SaveOutcome) -> String {
    match o {
        SaveOutcome::Saved { key, .. } => key,
        other => panic!("{other:?}"),
    }
}

// ---- checks shared by every backend ----

async fn check_first_run_not_found(b: &VpBackend) {
    let (_ws, dir) = empty_target();
    assert_eq!(
        vp_restore(b, V, LINUX, &dir).await.unwrap(),
        RestoreOutcome::NotFound
    );
    assert!(!dir.exists());
}

async fn check_other_os_never_restored(b: &VpBackend, prefix: &str) {
    let files: &[(&str, &[u8])] = &[("db.sqlite", b"tasks"), ("out/a.tar", &[1, 2, 3])];
    let (_src, src) = cache_with(files);
    let key = saved_key(vp_save(b, V, LINUX, "100-1", &src, BIG).await.unwrap());
    assert_eq!(key, format!("{prefix}vp/v1/linux-x64/100-1"));

    let (_ws, dir) = empty_target();
    assert_eq!(
        vp_restore(b, V, MAC, &dir).await.unwrap(),
        RestoreOutcome::NotFound
    );
    assert!(!dir.exists());
    // Another cache version does not see it either.
    assert_eq!(
        vp_restore(b, "v2", LINUX, &dir).await.unwrap(),
        RestoreOutcome::NotFound
    );
    match vp_restore(b, V, LINUX, &dir).await.unwrap() {
        RestoreOutcome::Restored { key: k, bytes } => {
            assert_eq!(k, key);
            assert!(bytes > 0);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(read_tree(&dir), tree(files));
}

async fn check_newer_save_wins(b: &VpBackend) {
    let (_a, a) = cache_with(&[("db", b"old")]);
    let (_b, bdir) = cache_with(&[("db", b"new"), ("extra", b"x")]);
    vp_save(b, V, LINUX, "1-1", &a, BIG).await.unwrap();
    let newer = saved_key(vp_save(b, V, LINUX, "2-1", &bdir, BIG).await.unwrap());
    let (_ws, dir) = empty_target();
    match vp_restore(b, V, LINUX, &dir).await.unwrap() {
        RestoreOutcome::Restored { key, .. } => assert_eq!(key, newer),
        other => panic!("{other:?}"),
    }
    assert_eq!(read_tree(&dir), tree(&[("db", b"new"), ("extra", b"x")]));
}

async fn check_restore_replaces_the_directory(b: &VpBackend) {
    let (_s, src) = cache_with(&[("db", b"saved")]);
    vp_save(b, V, LINUX, "5-1", &src, BIG).await.unwrap();
    let (_ws, dir) = cache_with(&[("db", b"stale"), ("leftover", b"x")]);
    assert!(matches!(
        vp_restore(b, V, LINUX, &dir).await.unwrap(),
        RestoreOutcome::Restored { .. }
    ));
    assert_eq!(read_tree(&dir), tree(&[("db", b"saved")]));
    // Nothing but the cache directory is left next to it.
    let names: Vec<_> = std::fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec!["task-cache"]);
}

async fn check_too_large_is_skipped(b: &VpBackend) {
    let (_s, src) = cache_with(&[("a", &[0u8; 600]), ("b/c", &[0u8; 500])]);
    assert_eq!(
        vp_save(b, V, LINUX, "7-1", &src, 1000).await.unwrap(),
        SaveOutcome::SkippedTooLarge { bytes: 1100 }
    );
    let (_ws, dir) = empty_target();
    assert_eq!(
        vp_restore(b, V, LINUX, &dir).await.unwrap(),
        RestoreOutcome::NotFound
    );
}

async fn check_empty_is_skipped(b: &VpBackend) {
    let (_ws, missing) = empty_target();
    assert_eq!(
        vp_save(b, V, LINUX, "8-1", &missing, BIG).await.unwrap(),
        SaveOutcome::SkippedEmpty
    );
    std::fs::create_dir_all(missing.join("sub")).unwrap();
    assert_eq!(
        vp_save(b, V, LINUX, "8-1", &missing, BIG).await.unwrap(),
        SaveOutcome::SkippedEmpty
    );
}

/// Two matrix legs of one run (same GITHUB_RUN_ID and attempt) save at once: both get their
/// own archive, and restore finds one of them.
async fn check_concurrent_saves(b: &VpBackend) {
    let gh = |k: &str| match k {
        "GITHUB_RUN_ID" => Some("10".to_string()),
        "GITHUB_RUN_ATTEMPT" => Some("1".to_string()),
        _ => None,
    };
    let (ka, kb) = (default_run_key(gh), default_run_key(gh));
    assert_ne!(ka, kb);
    let (_a, a) = cache_with(&[("db", b"job-a")]);
    let (_b, bdir) = cache_with(&[("db", b"job-b")]);
    let (ra, rb) = tokio::join!(
        vp_save(b, V, LINUX, &ka, &a, BIG),
        vp_save(b, V, LINUX, &kb, &bdir, BIG)
    );
    let keys = [saved_key(ra.unwrap()), saved_key(rb.unwrap())];
    assert_ne!(keys[0], keys[1]);
    for key in &keys {
        assert!(archive_exists(b, key).await, "{key} missing");
    }
    let (_ws, dir) = empty_target();
    match vp_restore(b, V, LINUX, &dir).await.unwrap() {
        RestoreOutcome::Restored { key, .. } => assert!(keys.contains(&key), "{key}"),
        other => panic!("{other:?}"),
    }
    let db = std::fs::read(dir.join("db")).unwrap();
    assert!(db == b"job-a" || db == b"job-b");
}

/// True when the archive `key` is stored (exact key, no restore-key fallback).
async fn archive_exists(b: &VpBackend, key: &str) -> bool {
    match b {
        VpBackend::Store { store, .. } => store.head(key).await.unwrap().is_some(),
        VpBackend::Github(g) => g
            .get_download_url(key, &[], &vp_version(V))
            .await
            .unwrap()
            .is_some(),
    }
}

/// Saving the same run key twice is success (keys are immutable; the first archive stays).
async fn check_existing_key_is_success(b: &VpBackend) {
    let (_a, a) = cache_with(&[("db", b"first")]);
    let (_b, bdir) = cache_with(&[("db", b"second")]);
    let k1 = saved_key(vp_save(b, V, LINUX, "12-1", &a, BIG).await.unwrap());
    let k2 = saved_key(vp_save(b, V, LINUX, "12-1", &bdir, BIG).await.unwrap());
    assert_eq!(k1, k2);
    let (_ws, dir) = empty_target();
    vp_restore(b, V, LINUX, &dir).await.unwrap();
    assert_eq!(std::fs::read(dir.join("db")).unwrap(), b"first");
}

macro_rules! fs_case {
    ($name:ident, $check:ident $(, $arg:expr)?) => {
        #[tokio::test]
        async fn $name() {
            let root = tempfile::tempdir().unwrap();
            let (b, _) = fs_backend(root.path());
            $check(&b $(, $arg)?).await;
        }
    };
}

fs_case!(fs_first_run_not_found, check_first_run_not_found);
fs_case!(
    fs_other_os_never_restored,
    check_other_os_never_restored,
    "cache/"
);
fs_case!(fs_newer_save_wins, check_newer_save_wins);
fs_case!(fs_restore_replaces, check_restore_replaces_the_directory);
fs_case!(fs_too_large_skipped, check_too_large_is_skipped);
fs_case!(fs_empty_skipped, check_empty_is_skipped);
fs_case!(fs_concurrent_saves, check_concurrent_saves);
fs_case!(fs_existing_key_is_success, check_existing_key_is_success);

// ---- store-backend specifics ----

#[tokio::test]
async fn pointer_missing_is_not_found() {
    let root = tempfile::tempdir().unwrap();
    let (b, store) = fs_backend(root.path());
    let (_s, src) = cache_with(&[("db", b"x")]);
    vp_save(&b, V, LINUX, "1-1", &src, BIG).await.unwrap();
    store.delete("cache/vp/v1/linux-x64/latest").await.unwrap();
    let (_ws, dir) = empty_target();
    assert_eq!(
        vp_restore(&b, V, LINUX, &dir).await.unwrap(),
        RestoreOutcome::NotFound
    );
}

#[tokio::test]
async fn archive_missing_is_not_found() {
    let root = tempfile::tempdir().unwrap();
    let (b, store) = fs_backend(root.path());
    let (_s, src) = cache_with(&[("db", b"x")]);
    let key = saved_key(vp_save(&b, V, LINUX, "1-1", &src, BIG).await.unwrap());
    store.delete(&key).await.unwrap(); // e.g. pruned or expired
    let (_ws, dir) = empty_target();
    assert_eq!(
        vp_restore(&b, V, LINUX, &dir).await.unwrap(),
        RestoreOutcome::NotFound
    );
}

#[tokio::test]
async fn pointer_outside_the_os_prefix_is_ignored() {
    let root = tempfile::tempdir().unwrap();
    let (b, store) = fs_backend(root.path());
    let (_s, src) = cache_with(&[("db", b"mac")]);
    let mac = saved_key(vp_save(&b, V, MAC, "1-1", &src, BIG).await.unwrap());
    for bad in [
        mac.as_str(),
        "cache/turbo/v1/abc",
        "cache/vp/v1/linux-x64/../x",
    ] {
        store
            .op
            .write("cache/vp/v1/linux-x64/latest", bad.to_string())
            .await
            .unwrap();
        let (_ws, dir) = empty_target();
        assert_eq!(
            vp_restore(&b, V, LINUX, &dir).await.unwrap(),
            RestoreOutcome::NotFound,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn failed_extract_leaves_the_old_directory_intact() {
    let root = tempfile::tempdir().unwrap();
    let (b, store) = fs_backend(root.path());
    // A valid envelope whose body is not a tar.zst.
    let key = "cache/vp/v1/linux-x64/9-1";
    let junk = vec![0x5au8; 5000];
    let header = Header {
        v: 1,
        duration_ms: None,
        tag: None,
        size: junk.len() as u64,
    };
    let body = futures::stream::iter([Ok(bytes::Bytes::from(junk))]);
    store.put(key, &header, body).await.unwrap();
    store
        .op
        .write("cache/vp/v1/linux-x64/latest", key.to_string())
        .await
        .unwrap();

    let (_ws, dir) = cache_with(&[("db", b"keep me")]);
    assert!(vp_restore(&b, V, LINUX, &dir).await.is_err());
    assert_eq!(read_tree(&dir), tree(&[("db", b"keep me")]));
    let names: Vec<_> = std::fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec!["task-cache"], "no temp dir left behind");
}

// ---- MinIO (R2) ----

macro_rules! s3_case {
    ($name:ident, $check:ident $(, $arg:expr)?) => {
        #[test]
        fn $name() {
            s3_runtime().block_on(async {
                let Some(bucket) = minio_bucket().await else {
                    return;
                };
                let root = tempfile::tempdir().unwrap();
                let op = build_operator(&r2_config(root.path(), &bucket)).unwrap();
                let b = VpBackend::Store {
                    store: Arc::new(Store::new(op)),
                    prefix: "cache/".into(),
                };
                $check(&b $(, $arg)?).await;
            });
        }
    };
}

s3_case!(s3_first_run_not_found, check_first_run_not_found);
s3_case!(
    s3_other_os_never_restored,
    check_other_os_never_restored,
    "cache/"
);
s3_case!(s3_newer_save_wins, check_newer_save_wins);
s3_case!(s3_restore_replaces, check_restore_replaces_the_directory);
s3_case!(s3_too_large_skipped, check_too_large_is_skipped);
s3_case!(s3_concurrent_saves, check_concurrent_saves);
s3_case!(s3_existing_key_is_success, check_existing_key_is_success);

// ---- GitHub cache (mock Twirp + blob server) ----

const TOKEN: &str = "runtime-token-secret";

async fn gha() -> (VpBackend, gha_mock::GhaMock) {
    let mock = gha_mock::spawn(TOKEN).await;
    let g = GhaTwirp::new(&mock.url, TOKEN).unwrap();
    (VpBackend::Github(g), mock)
}

macro_rules! gha_case {
    ($name:ident, $check:ident $(, $arg:expr)?) => {
        #[tokio::test]
        async fn $name() {
            let (b, _mock) = gha().await;
            $check(&b $(, $arg)?).await;
        }
    };
}

gha_case!(gha_first_run_not_found, check_first_run_not_found);
gha_case!(
    gha_other_os_never_restored,
    check_other_os_never_restored,
    "hoard/"
);
gha_case!(gha_newer_save_wins, check_newer_save_wins);
gha_case!(gha_restore_replaces, check_restore_replaces_the_directory);
gha_case!(gha_too_large_skipped, check_too_large_is_skipped);
gha_case!(gha_concurrent_saves, check_concurrent_saves);
gha_case!(gha_existing_key_is_success, check_existing_key_is_success);

#[tokio::test]
async fn gha_protocol_round_trip() {
    let (b, mock) = gha().await;
    let (_s, src) = cache_with(&[("db", b"tasks")]);
    let key = saved_key(vp_save(&b, V, LINUX, "42-3", &src, BIG).await.unwrap());
    assert_eq!(key, "hoard/vp/v1/linux-x64/42-3");
    let entries = mock.entries();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].finalized);
    assert_eq!(entries[0].version, vp_version("v1"));

    let (_ws, dir) = empty_target();
    vp_restore(&b, V, LINUX, &dir).await.unwrap();
    let reqs = mock.requests();
    let paths: Vec<_> = reqs
        .iter()
        .map(|r| {
            let q = r.query.split('&').find(|p| p.starts_with("comp="));
            format!(
                "{} {}{}",
                r.method,
                r.path,
                q.map(|q| format!("?{q}")).unwrap_or_default()
            )
        })
        .collect();
    let twirp = "/twirp/github.actions.results.api.v1.CacheService";
    assert_eq!(
        paths,
        vec![
            format!("POST {twirp}/CreateCacheEntry"),
            "PUT /blob/b0?comp=block".to_string(),
            "PUT /blob/b0?comp=blocklist".to_string(),
            format!("POST {twirp}/FinalizeCacheEntryUpload"),
            format!("POST {twirp}/GetCacheEntryDownloadURL"),
            "GET /blob/b0".to_string(),
        ]
    );
    for r in &reqs {
        if r.path.starts_with("/twirp/") {
            assert_eq!(
                r.authorization.as_deref(),
                Some("Bearer runtime-token-secret")
            );
        } else {
            assert_eq!(r.authorization, None, "token sent to blob storage");
            assert!(r.x_ms_version.is_some());
        }
    }
    let get = &reqs[4].json.as_ref().unwrap();
    assert_eq!(get["key"], "hoard/vp/v1/linux-x64/");
    assert_eq!(
        get["restore_keys"],
        serde_json::json!(["hoard/vp/v1/linux-x64/"])
    );
    assert_eq!(get["version"], vp_version("v1"));
    let fin = &reqs[3].json.as_ref().unwrap();
    assert_eq!(fin["size_bytes"], entries[0].size.unwrap().to_string());
}

#[tokio::test]
async fn gha_rate_limit_and_errors_are_storage_errors() {
    for status in [429, 500] {
        let (b, mock) = gha().await;
        mock.fail_with(status);
        let (_s, src) = cache_with(&[("db", b"x")]);
        let err = vp_save(&b, V, LINUX, "1-1", &src, BIG)
            .await
            .unwrap_err()
            .to_string();
        if status == 429 {
            assert!(err.contains("rate limit"), "{err}");
        }
        let (_ws, dir) = empty_target();
        let err = vp_restore(&b, V, LINUX, &dir).await.unwrap_err();
        let text = format!("{err:#}");
        assert!(
            !text.contains(gha_mock::MOCK_SIG) && !text.contains(TOKEN),
            "{text}"
        );
    }
}

/// A broken blob download (the service hands out a URL, the blob is gone) leaves no trace
/// of the URL in the error.
#[tokio::test]
async fn gha_download_error_hides_the_url() {
    let (b, mock) = gha().await;
    let (_s, src) = cache_with(&[("db", b"x")]);
    vp_save(&b, V, LINUX, "1-1", &src, BIG).await.unwrap();
    mock.state.lock().unwrap().blobs.clear();
    let (_ws, dir) = empty_target();
    let text = format!("{:#}", vp_restore(&b, V, LINUX, &dir).await.unwrap_err());
    assert!(text.contains("404"), "{text}");
    assert!(
        !text.contains("sig=") && !text.contains("http://"),
        "{text}"
    );
}

// ---- the binary ----

fn hoard(ws: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hoard"));
    for k in [
        "ACTIONS_RESULTS_URL",
        "ACTIONS_RUNTIME_TOKEN",
        "ACTIONS_CACHE_URL",
        "ACTIONS_CACHE_SERVICE_V2",
        "VITE_CACHE_PATH",
        "GITHUB_RUN_ID",
        "GITHUB_RUN_ATTEMPT",
        "RUNNER_TEMP", // the fingerprint then lives in <ws>/.hoard
    ] {
        c.env_remove(k);
    }
    c.env("RUST_LOG", "warn").arg("vp");
    c.current_dir(ws);
    c
}

fn run(mut c: Command) -> (i32, String, String) {
    let out = c.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn binary_fs_save_then_restore() {
    let store = tempfile::tempdir().unwrap();
    let fs_root = store.path().to_str().unwrap().to_string();
    let (src_ws, _src) = cache_with(&[("db", b"tasks")]);
    let mut c = hoard(src_ws.path());
    c.args(["save", "--storage", "fs", "--fs-root", &fs_root])
        .env("GITHUB_RUN_ID", "77")
        .env("GITHUB_RUN_ATTEMPT", "1");
    let (code, out, err) = run(c);
    assert_eq!(code, 0, "{err}");
    let os = default_os_arch();
    assert!(
        out.starts_with(&format!("saved cache/vp/v1/{os}/77-1-")),
        "{out}"
    );
    assert_eq!(out.lines().count(), 1);

    let (dst_ws, dst) = empty_target();
    let mut c = hoard(dst_ws.path());
    c.args(["restore", "--storage", "fs", "--fs-root", &fs_root]);
    let (code, out, err) = run(c);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with(&format!("restored cache/vp/v1/{os}/77-1-")),
        "{out}"
    );
    assert_eq!(std::fs::read(dst.join("db")).unwrap(), b"tasks");

    // Another OS: nothing found, still exit 0.
    let (ws, _) = empty_target();
    let mut c = hoard(ws.path());
    c.args([
        "restore",
        "--storage",
        "fs",
        "--fs-root",
        &fs_root,
        "--os-arch",
        "windows-x64",
    ]);
    let (code, out, _) = run(c);
    assert_eq!((code, out.trim()), (0, "no cache found"));
}

fn fs_cmd(ws: &Path, cmd: &str, fs_root: &str) -> Command {
    let mut c = hoard(ws);
    c.args([cmd, "--storage", "fs", "--fs-root", fs_root]);
    c
}

/// A job that restored the task cache and changed nothing does not upload it again.
#[test]
fn binary_save_unchanged_since_restore_is_skipped() {
    let store = tempfile::tempdir().unwrap();
    let fs_root = store.path().to_str().unwrap().to_string();
    let (src_ws, _) = cache_with(&[("db", b"tasks"), ("out/a", b"x")]);
    let (code, out, err) = run(fs_cmd(src_ws.path(), "save", &fs_root));
    assert_eq!(code, 0, "{err}");
    let first = out.trim().strip_prefix("saved ").unwrap().to_string();
    let first = first.split(' ').next().unwrap().to_string();

    let (ws, dir) = empty_target();
    let (code, out, err) = run(fs_cmd(ws.path(), "restore", &fs_root));
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("restored "), "{out}");
    let os = default_os_arch();
    assert!(
        ws.path()
            .join(format!(".hoard/hoard-vp-{os}.fingerprint"))
            .is_file()
    );

    let (code, out, err) = run(fs_cmd(ws.path(), "save", &fs_root));
    assert_eq!(
        (code, out.trim()),
        (0, "skipped: unchanged since restore"),
        "{err}"
    );
    let pointer = format!("{fs_root}/cache/vp/v1/{os}/latest");
    assert_eq!(std::fs::read_to_string(&pointer).unwrap(), first);

    // A task ran and wrote to the cache: saved.
    std::fs::write(dir.join("db2"), b"more").unwrap();
    let (code, out, err) = run(fs_cmd(ws.path(), "save", &fs_root));
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("saved "), "{out}");
}

/// A restore that finds nothing drops a fingerprint left from earlier, so save uploads.
#[test]
fn binary_restore_without_a_cache_drops_an_old_fingerprint() {
    let store = tempfile::tempdir().unwrap();
    let fs_root = store.path().to_str().unwrap().to_string();
    let (ws, dir) = cache_with(&[("db", b"local")]);
    let os = default_os_arch();
    let fp = ws.path().join(format!(".hoard/hoard-vp-{os}.fingerprint"));
    std::fs::create_dir_all(fp.parent().unwrap()).unwrap();
    std::fs::write(&fp, hoard::vp::fingerprint::fingerprint(&dir).unwrap()).unwrap();

    let (code, out, _) = run(fs_cmd(ws.path(), "restore", &fs_root));
    assert_eq!((code, out.trim()), (0, "no cache found"));
    assert!(!fp.exists());
    let (code, out, err) = run(fs_cmd(ws.path(), "save", &fs_root));
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("saved "), "{out}");
}

#[test]
fn binary_skips_print_one_line() {
    let store = tempfile::tempdir().unwrap();
    let fs_root = store.path().to_str().unwrap().to_string();
    let (ws, _) = empty_target();
    let mut c = hoard(ws.path());
    c.args(["save", "--storage", "fs", "--fs-root", &fs_root]);
    let (code, out, _) = run(c);
    assert_eq!(
        (code, out.trim()),
        (0, "skipped: cache directory is empty or missing")
    );

    let (ws, _) = cache_with(&[("db", &[0u8; 2000])]);
    let mut c = hoard(ws.path());
    c.args([
        "save",
        "--storage",
        "fs",
        "--fs-root",
        &fs_root,
        "--vp-max-size",
        "1KB",
    ]);
    let (code, out, _) = run(c);
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "skipped: the task cache is 2.0 KB, larger than vp-max-size (1.0 KB); run `vp cache clean`"
    );
}

#[test]
fn binary_storage_error_exits_zero() {
    let (ws, _) = cache_with(&[("db", b"x")]);
    for cmd in ["save", "restore"] {
        let mut c = hoard(ws.path());
        c.args([
            cmd,
            "--storage",
            "r2",
            "--r2-endpoint",
            "http://127.0.0.1:1",
            "--r2-bucket",
            "b",
        ])
        .env("HOARD_R2_ACCESS_KEY_ID", "akid-secret-1")
        .env("HOARD_R2_SECRET_ACCESS_KEY", "sk-secret-2");
        let (code, out, err) = run(c);
        assert_eq!(code, 0, "{out}{err}");
        assert!(
            out.starts_with(&format!("warning: Vite+ cache {cmd} failed (")),
            "{out}"
        );
        assert!(
            out.trim_end().ends_with("the build continues without it"),
            "{out}"
        );
        for s in ["akid-secret-1", "sk-secret-2"] {
            assert!(!out.contains(s) && !err.contains(s), "{s} leaked");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_github_failure_exits_zero_without_leaking() {
    let mock = gha_mock::spawn(TOKEN).await;
    let (ws, _) = cache_with(&[("db", b"x")]);
    // Save goes through, then the service starts failing.
    let url = mock.url.clone();
    let ws_path = ws.path().to_path_buf();
    let gh = move |cmd: &'static str| {
        let mut c = hoard(&ws_path);
        c.args([cmd, "--storage", "github"])
            .env("ACTIONS_RESULTS_URL", &url)
            .env("ACTIONS_RUNTIME_TOKEN", TOKEN)
            .env("GITHUB_RUN_ID", "5")
            .env("GITHUB_RUN_ATTEMPT", "1");
        run(c)
    };
    let gh2 = gh.clone();
    let (code, out, err) = tokio::task::spawn_blocking(move || gh2("save"))
        .await
        .unwrap();
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("saved hoard/vp/v1/"), "{out}");
    mock.fail_with(503);
    for cmd in ["save", "restore"] {
        let gh = gh.clone();
        let (code, out, err) = tokio::task::spawn_blocking(move || gh(cmd)).await.unwrap();
        assert_eq!(code, 0, "{out}{err}");
        assert!(out.starts_with("warning: Vite+ cache"), "{out}");
        for s in [gha_mock::MOCK_SIG, TOKEN, "sig="] {
            assert!(
                !out.contains(s) && !err.contains(s),
                "{s} leaked: {out}{err}"
            );
        }
    }
}

#[test]
fn binary_setup_errors_fail() {
    let (ws, _) = cache_with(&[("db", b"x")]);
    let mut c = hoard(ws.path());
    c.args(["restore", "--storage", "github"]);
    let (code, _, err) = run(c);
    assert_ne!(code, 0);
    assert!(
        err.contains(
            "ACTIONS_RESULTS_URL is not set: run hoard through the GitHub Action, or use --storage r2|fs"
        ),
        "{err}"
    );
    for (flag, value) in [
        ("--os-arch", "linux/x64"),
        ("--run-key", "latest"),
        ("--run-key", ".."),
    ] {
        let mut c = hoard(ws.path());
        c.args(["save", "--storage", "fs", flag, value]);
        let (code, _, err) = run(c);
        assert_ne!(code, 0, "{flag} {value}");
        assert!(err.contains(&flag[2..]) && err.contains("HOARD_"), "{err}");
    }
    let mut c = hoard(ws.path());
    c.args(["save", "--storage", "fs", "--vp-max-size", "lots"]);
    let (code, _, err) = run(c);
    assert_ne!(code, 0);
    assert!(err.contains("invalid size"), "{err}");
}

/// mode=off is checked before any storage setup: no GitHub env needed.
#[test]
fn binary_mode_off_needs_no_storage() {
    let (ws, _) = cache_with(&[("db", b"x")]);
    for cmd in ["save", "restore"] {
        let mut c = hoard(ws.path());
        c.args([cmd, "--storage", "github", "--mode", "off"]);
        let (code, out, err) = run(c);
        assert_eq!((code, out.trim()), (0, "skipped: mode=off"), "{err}");
    }
}

#[test]
fn binary_respects_mode() {
    let store = tempfile::tempdir().unwrap();
    let fs_root = store.path().to_str().unwrap().to_string();
    let (ws, _) = cache_with(&[("db", b"x")]);
    for (cmd, extra, want) in [
        (
            "save",
            "--mode=read",
            "skipped: mode=read (nothing is written)",
        ),
        ("save", "--mode=off", "skipped: mode=off"),
        ("restore", "--mode=off", "skipped: mode=off"),
        (
            "restore",
            "--no-reads",
            "skipped: no-reads (the cache is not read)",
        ),
    ] {
        let mut c = hoard(ws.path());
        c.args([cmd, "--storage", "fs", "--fs-root", &fs_root, extra]);
        let (code, out, _) = run(c);
        assert_eq!((code, out.trim()), (0, want));
    }
    assert!(!store.path().join("cache").exists(), "nothing written");
}

/// An archive over one block goes up as several Put Block requests and comes back intact.
#[tokio::test]
async fn gha_archive_larger_than_a_block() {
    let (b, mock) = gha().await;
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let data: Vec<u8> = (0..(hoard::vp::gha_twirp::BLOCK_SIZE + 1_000_000))
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let (_s, src) = cache_with(&[("big", &data)]);
    vp_save(&b, V, LINUX, "1-1", &src, BIG).await.unwrap();
    let blocks = mock
        .requests()
        .iter()
        .filter(|r| r.query.contains("comp=block&"))
        .count();
    assert_eq!(blocks, 2);
    let (_ws, dir) = empty_target();
    vp_restore(&b, V, LINUX, &dir).await.unwrap();
    assert!(std::fs::read(dir.join("big")).unwrap() == data);
}
