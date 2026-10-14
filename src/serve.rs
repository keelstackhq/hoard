//! `hoard serve` (run the server until shutdown) and `hoard env` (export lines for it).

use std::io::Write;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::config::{Config, Storage, TokenPaths, default_port_file, default_token_file};
use crate::server::{AppState, router};
use crate::stats::{Stats, render_summary};
use crate::store::{Store, build_operator};

/// Key prefix used by the fs backend.
const FS_PREFIX: &str = "cache/";

/// The three `export` lines that point Turborepo at a server.
pub fn env_lines(port: u16, token: &str) -> String {
    format!(
        "export TURBO_API=http://127.0.0.1:{port}\nexport TURBO_TOKEN={token}\nexport TURBO_TEAM=hoard\n"
    )
}

/// Reads the port and token files of a running server and returns its [`env_lines`].
pub fn env_for(paths: &TokenPaths) -> anyhow::Result<String> {
    let port_file = paths.port_file.clone().unwrap_or_else(default_port_file);
    let token_file = paths.token_file.clone().unwrap_or_else(default_token_file);
    let read = |p: &Path| {
        std::fs::read_to_string(p).with_context(|| {
            format!(
                "no running hoard server found (missing {}); start one with `hoard serve`",
                p.display()
            )
        })
    };
    let port_text = read(&port_file)?;
    let port: u16 = port_text.trim().parse().with_context(|| {
        format!(
            "{} does not hold a port number; restart `hoard serve`",
            port_file.display()
        )
    })?;
    let token = read(&token_file)?;
    Ok(env_lines(port, token.trim()))
}

/// 32 random bytes as 64 lowercase hex characters.
fn random_token() -> String {
    rand::random::<[u8; 32]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Key prefix and the summary's storage line for the configured backend.
fn storage_layout(cfg: &Config) -> anyhow::Result<(String, String)> {
    match cfg.storage {
        Storage::Fs => Ok((
            FS_PREFIX.to_string(),
            format!("Storage: fs ({})", cfg.fs_root.display()),
        )),
        Storage::R2 => {
            let r2 = cfg
                .r2
                .as_ref()
                .context("storage=r2 needs the r2-* settings")?;
            Ok((
                r2.prefix.clone(),
                format!("Storage: r2 ({}/{})", r2.bucket, r2.prefix),
            ))
        }
        Storage::Github => bail!(
            "the GitHub cache backend comes with the GitHub Action (milestone 2); \
             use --storage fs|r2 for now"
        ),
    }
}

/// Writes `contents` to `path` via `<path>.tmp` and a rename, so a reader never sees a
/// partial file. Creates parent directories. `private` files get mode 0600 on unix, set at
/// creation before any content is written.
fn write_atomic(path: &Path, contents: &str, private: bool) -> anyhow::Result<()> {
    let ctx = || format!("cannot write {}", path.display());
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(ctx)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    // A stale temp file could carry other permissions; start fresh.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(ctx),
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut f = opts.open(&tmp).with_context(ctx)?;
    f.write_all(contents.as_bytes()).with_context(ctx)?;
    f.sync_all().with_context(ctx)?;
    drop(f);
    std::fs::rename(&tmp, path).with_context(ctx)
}

fn remove_if_present(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), error = %e, "cannot remove file");
    }
}

/// Resolves on SIGTERM or Ctrl-C.
async fn termination_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

/// Runs the server until `POST /_hoard/shutdown`, SIGTERM or Ctrl-C, then runs the shutdown
/// work (hooks, summary) and removes the port and token files.
pub async fn run_serve(cfg: Config) -> anyhow::Result<()> {
    let (prefix, storage_line) = storage_layout(&cfg)?;
    let store = Arc::new(Store::new(build_operator(&cfg)?));
    // Startup checks that must pass before the server is reachable go here.

    let token = cfg.token.clone().unwrap_or_else(random_token);
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, cfg.port))
        .await
        .with_context(|| {
            format!(
                "cannot listen on 127.0.0.1:{}; pick another --port or use 0",
                cfg.port
            )
        })?;
    let port = listener.local_addr()?.port();

    let stats = Arc::new(Stats::default());
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<oneshot::Sender<()>>(8);
    let state = AppState {
        store,
        stats: stats.clone(),
        token: token.clone(),
        mode: cfg.mode,
        prefix,
        cache_version: cfg.cache_version.clone(),
        shutdown: shutdown_tx,
    };

    // The token file first: the port file is the readiness signal.
    write_atomic(&cfg.token_file, &token, true)?;
    write_atomic(&cfg.port_file, &port.to_string(), false)?;

    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .into_future(),
    );
    info!("hoard listening on http://127.0.0.1:{port} — run: eval \"$(hoard env)\"");

    let reply = tokio::select! {
        r = shutdown_rx.recv() => r,
        _ = termination_signal() => None,
    };
    info!("shutting down");

    let result = shutdown_work(&cfg, &stats, &storage_line).await;
    remove_if_present(&cfg.port_file);
    remove_if_present(&cfg.token_file);

    if let Some(reply) = reply {
        let _ = reply.send(());
    }
    // Later shutdown requests: their reply channels are dropped here, so they answer 200.
    drop(shutdown_rx);
    let _ = stop_tx.send(());
    server.await.context("server task failed")??;
    result
}

/// Work done once on shutdown: hooks contribute extra summary lines, then the summary is
/// written.
async fn shutdown_work(cfg: &Config, stats: &Stats, storage_line: &str) -> anyhow::Result<()> {
    let extra_lines: Vec<String> = Vec::new();
    if let Some(path) = &cfg.summary_file {
        let md = render_summary(&stats.snapshot(), storage_line, &extra_lines);
        write_atomic(path, &md, false)?;
    }
    Ok(())
}
