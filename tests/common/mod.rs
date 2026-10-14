//! Helpers shared by integration tests.
#![allow(dead_code)]

pub mod failing_layer;
pub mod gha_mock;
pub mod lifecycle_proxy;
pub mod store_checks;

use hoard::config::{Config, LifecycleMode, Mode, R2Config, Storage};

/// A spawned child process (a `hoard serve`, usually) that is killed and reaped on drop, so a
/// failed assertion or an early return never leaves a server running.
pub struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    /// Waits for the child to exit and collects its piped output.
    pub fn wait_with_output(mut self) -> std::process::Output {
        let child = self.0.take().expect("child present until consumed");
        child.wait_with_output().unwrap()
    }
}

impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("child present until consumed")
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("child present until consumed")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.as_mut() {
            // A child that already exited and was waited for is not signalled again.
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// `Command::spawn` that returns a [`ChildGuard`].
pub trait SpawnGuarded {
    fn spawn_guarded(&mut self) -> ChildGuard;
}

impl SpawnGuarded for std::process::Command {
    fn spawn_guarded(&mut self) -> ChildGuard {
        ChildGuard(Some(self.spawn().unwrap()))
    }
}

/// Config for the fs backend rooted under `root`: port 0, and the port, token and summary
/// files inside `root`.
pub fn fs_config(root: impl AsRef<std::path::Path>) -> Config {
    let root = root.as_ref();
    Config {
        storage: Storage::Fs,
        mode: Mode::ReadWrite,
        no_reads: false,
        cache_version: "v1".into(),
        port: 0,
        port_file: root.join("port"),
        token_file: root.join("token"),
        token: None,
        summary_file: Some(root.join("summary.md")),
        fs_root: root.join("store"),
        r2: None,
        r2_lifecycle: LifecycleMode::Auto,
        max_age_days: 7,
        max_size: 1 << 30,
        write_behind: false,
        refresh_on_hit: false,
    }
}

use std::sync::Arc;

use hoard::ratelimit::RateGate;
use hoard::server::{AppState, router};
use hoard::stats::Stats;
use hoard::store::{Store, build_operator};
use hoard::writebehind::WriteBehind;

/// A running server on `127.0.0.1:<random>` backed by an fs store in a tempdir.
pub struct TestServer {
    pub base_url: String,
    pub token: String,
    pub stats: Arc<Stats>,
    pub store: Arc<Store>,
    pub rate_gate: Arc<RateGate>,
    /// The write-behind queue, when the server has one.
    pub write_behind: Option<Arc<WriteBehind>>,
    /// Receives a reply channel when `POST /_hoard/shutdown` is called; the handler answers
    /// once the reply is sent (or dropped).
    pub shutdown:
        tokio::sync::mpsc::Receiver<tokio::sync::oneshot::Sender<hoard::stats::StatsSnapshot>>,
    pub addr: std::net::SocketAddr,
    _dir: tempfile::TempDir,
}

pub async fn spawn_test_server(mode: Mode) -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fs_config(dir.path());
    let store = Store::new(build_operator(&cfg).unwrap());
    spawn_test_server_with(store, mode, dir).await
}

/// A running server on `127.0.0.1:<random>` over `store`; `dir` lives as long as the server.
pub async fn spawn_test_server_with(
    store: Store,
    mode: Mode,
    dir: tempfile::TempDir,
) -> TestServer {
    spawn_test_server_opts(store, mode, false, dir).await
}

/// An fs-backed server with `no_reads` set.
pub async fn spawn_no_reads_server() -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(build_operator(&fs_config(dir.path())).unwrap());
    spawn_test_server_opts(store, Mode::ReadWrite, true, dir).await
}

/// An fs store in a fresh tempdir with `layer` on top.
pub fn layered_fs_store(layer: impl opendal::raw::Layer) -> (Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let op = build_operator(&fs_config(dir.path())).unwrap().layer(layer);
    (Store::new(op), dir)
}

/// [`spawn_test_server_with`] with every server option.
pub async fn spawn_test_server_opts(
    store: Store,
    mode: Mode,
    no_reads: bool,
    dir: tempfile::TempDir,
) -> TestServer {
    let parts = ServerParts::new(Arc::new(store));
    spawn_server(parts, mode, no_reads, dir).await
}

/// A read-write server over `store` whose PUTs go through a write-behind queue (spool dir
/// `<dir>/spool`, concurrency 8, spool cap `cap`). `TestServer::write_behind` drains it.
pub async fn spawn_write_behind_server(
    store: Store,
    cap: u64,
    dir: tempfile::TempDir,
) -> TestServer {
    let mut parts = ServerParts::new(Arc::new(store));
    std::fs::create_dir_all(dir.path().join("spool")).unwrap();
    let wb = WriteBehind::new(
        parts.store.clone(),
        parts.stats.clone(),
        parts.rate_gate.clone(),
        dir.path().join("spool"),
        8,
    )
    .with_spool_cap(cap);
    parts.write_behind = Some(Arc::new(wb));
    spawn_server(parts, Mode::ReadWrite, false, dir).await
}

/// A read-write server over `store` that refreshes R2 objects on a GET hit through `refresher`.
pub async fn spawn_refresh_server(
    store: Store,
    refresher: Option<Arc<hoard::r2::refresh::Refresher>>,
    stats: Arc<Stats>,
    dir: tempfile::TempDir,
) -> TestServer {
    let mut parts = ServerParts::new(Arc::new(store));
    parts.stats = stats;
    parts.refresh = refresher;
    spawn_server(parts, Mode::ReadWrite, false, dir).await
}

/// The shared pieces of a test server's state.
struct ServerParts {
    store: Arc<Store>,
    stats: Arc<Stats>,
    rate_gate: Arc<RateGate>,
    write_behind: Option<Arc<WriteBehind>>,
    refresh: Option<Arc<hoard::r2::refresh::Refresher>>,
}

impl ServerParts {
    fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            stats: Arc::default(),
            rate_gate: Arc::default(),
            write_behind: None,
            refresh: None,
        }
    }
}

async fn spawn_server(
    parts: ServerParts,
    mode: Mode,
    no_reads: bool,
    dir: tempfile::TempDir,
) -> TestServer {
    let token: String = rand::random::<[u8; 16]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let state = AppState {
        store: parts.store.clone(),
        stats: parts.stats.clone(),
        token: token.clone(),
        mode,
        no_reads,
        rate_gate: parts.rate_gate.clone(),
        prefix: "cache/".into(),
        cache_version: "v1".into(),
        write_behind: parts.write_behind.clone(),
        refresh: parts.refresh.clone(),
        shutdown: tx,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    TestServer {
        base_url: format!("http://{addr}"),
        token,
        stats: parts.stats,
        store: parts.store,
        rate_gate: parts.rate_gate,
        write_behind: parts.write_behind,
        shutdown: rx,
        addr,
        _dir: dir,
    }
}

/// Polls until `path` exists and is non-empty (up to 10 s), then returns its contents.
pub async fn wait_for_file(path: &std::path::Path) -> String {
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(path)
            && !s.is_empty()
        {
            return s;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{} did not appear", path.display());
}

/// The runtime every S3 test must run on (`s3_runtime().block_on(async { .. })`), instead of
/// `#[tokio::test]`. OpenDAL's default HTTP client (reqwest, with its connection pool) is
/// process-wide; pooled connections die with the runtime that opened them, so per-test
/// runtimes hand later tests dead connections ("dispatch task is gone: runtime dropped the
/// dispatch task"). The server runs on a single runtime, so this matches production.
pub fn s3_runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    });
    &RT
}

/// A fresh, empty bucket on the MinIO server named by `HOARD_TEST_S3_ENDPOINT`.
#[derive(Debug, Clone)]
pub struct MinioBucket {
    pub endpoint: String,
    pub bucket: String,
    pub key_id: String,
    pub secret: String,
}

/// Creates a uniquely named bucket for one test. `None` (with a note on stderr) when
/// `HOARD_TEST_S3_ENDPOINT` is unset; CI always sets it. Credentials come from
/// `HOARD_TEST_S3_KEY` / `HOARD_TEST_S3_SECRET`, defaulting to MinIO's `minio` / `minio123`.
pub async fn minio_bucket() -> Option<MinioBucket> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let Some(endpoint) = var("HOARD_TEST_S3_ENDPOINT") else {
        eprintln!("skipping: HOARD_TEST_S3_ENDPOINT unset");
        return None;
    };
    let key_id = var("HOARD_TEST_S3_KEY").unwrap_or_else(|| "minio".into());
    let secret = var("HOARD_TEST_S3_SECRET").unwrap_or_else(|| "minio123".into());
    let suffix: String = rand::random::<[u8; 8]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let bucket = format!("hoard-test-{suffix}");
    s3_client(&endpoint, &key_id, &secret)
        .create_bucket()
        .bucket(&bucket)
        .send()
        .await
        .unwrap_or_else(|e| panic!("cannot create bucket {bucket} on {endpoint}: {e:?}"));
    Some(MinioBucket {
        endpoint,
        bucket,
        key_id,
        secret,
    })
}

/// An aws-sdk-s3 client for MinIO with static credentials and path-style addressing.
pub fn s3_client(endpoint: &str, key_id: &str, secret: &str) -> aws_sdk_s3::Client {
    use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
    let conf = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .endpoint_url(endpoint)
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new(key_id, secret, None, None, "hoard-test"))
        .force_path_style(true)
        .build();
    aws_sdk_s3::Client::from_conf(conf)
}

/// Config for the R2 backend pointed at `b` through the `r2-endpoint` override.
pub fn r2_config(root: impl AsRef<std::path::Path>, b: &MinioBucket) -> Config {
    let mut cfg = fs_config(root);
    cfg.storage = Storage::R2;
    cfg.r2 = Some(R2Config {
        account_id: None,
        endpoint: Some(b.endpoint.clone()),
        access_key_id: b.key_id.clone(),
        secret_access_key: b.secret.clone(),
        bucket: b.bucket.clone(),
        prefix: "cache/".into(),
    });
    cfg
}

/// `b` reached through a [`lifecycle_proxy`]: lifecycle rules are kept verbatim (MinIO drops
/// `AbortIncompleteMultipartUpload`), everything else goes to MinIO.
pub async fn proxied(b: &MinioBucket) -> MinioBucket {
    proxied_with_store(b).await.0
}

/// [`proxied`], plus the proxy's lifecycle store (to seed or inspect the raw XML).
pub async fn proxied_with_store(b: &MinioBucket) -> (MinioBucket, lifecycle_proxy::Lifecycles) {
    let (b, store, _) = proxied_with_log(b).await;
    (b, store)
}

/// [`proxied_with_store`], plus the log of every request the proxy received.
pub async fn proxied_with_log(
    b: &MinioBucket,
) -> (
    MinioBucket,
    lifecycle_proxy::Lifecycles,
    lifecycle_proxy::RequestLog,
) {
    let (endpoint, store, log) = lifecycle_proxy::spawn(&b.endpoint).await;
    let b = MinioBucket {
        endpoint,
        ..b.clone()
    };
    (b, store, log)
}
