//! Helpers shared by integration tests.
#![allow(dead_code)]

pub mod store_checks;

use hoard::config::{Config, LifecycleMode, Mode, Storage};

/// Config for the fs backend rooted under `root`: port 0, and the port, token and summary
/// files inside `root`.
pub fn fs_config(root: impl AsRef<std::path::Path>) -> Config {
    let root = root.as_ref();
    Config {
        storage: Storage::Fs,
        mode: Mode::ReadWrite,
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
    }
}

use std::sync::Arc;

use hoard::server::{AppState, router};
use hoard::stats::Stats;
use hoard::store::{Store, build_operator};

/// A running server on `127.0.0.1:<random>` backed by an fs store in a tempdir.
pub struct TestServer {
    pub base_url: String,
    pub token: String,
    pub stats: Arc<Stats>,
    pub store: Arc<Store>,
    /// Receives a reply channel when `POST /_hoard/shutdown` is called; the handler answers
    /// once the reply is sent (or dropped).
    pub shutdown: tokio::sync::mpsc::Receiver<tokio::sync::oneshot::Sender<()>>,
    pub addr: std::net::SocketAddr,
    _dir: tempfile::TempDir,
}

pub async fn spawn_test_server(mode: Mode) -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fs_config(dir.path());
    let store = Arc::new(Store::new(build_operator(&cfg).unwrap()));
    let stats = Arc::new(Stats::default());
    let token: String = rand::random::<[u8; 16]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let state = AppState {
        store: store.clone(),
        stats: stats.clone(),
        token: token.clone(),
        mode,
        prefix: "cache/".into(),
        cache_version: "v1".into(),
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
        stats,
        store,
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
