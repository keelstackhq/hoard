//! `hoard serve` (run the server until shutdown) and `hoard env` (export lines for it).

use std::io::Write;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::config::{
    Config, LifecycleMode, Mode, Storage, TokenPaths, default_port_file, default_token_file,
};
use crate::r2::lifecycle::{LifecycleError, ensure_lifecycle, fix_command, r2_client, rule_id};
use crate::r2::prune::prune;
use crate::server::{AppState, router};
use crate::stats::{Stats, human_bytes, render_summary};
use crate::store::{Store, StoreError, build_operator};

/// Key prefix used by the fs backend.
const FS_PREFIX: &str = "cache/";

/// Upper bound for the shutdown prune: a slow bucket must not hold the CI job hostage. The
/// marker is only written on completion, so an interrupted prune runs again next time.
/// Known limitation: if the LIST alone takes longer than this, the marker is never written.
const PRUNE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Upper bound for the whole startup lifecycle check (Get, maybe Put, SDK retries included).
/// It runs before the port file, the readiness signal, is written, and the GitHub Action fails
/// the job if health is not up within 10 s. A slow R2 is a transient cache problem and must
/// never fail the build, so a timeout is a warning and `Lifecycle: not checked (timed out)`.
const LIFECYCLE_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

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
pub(crate) fn storage_layout(cfg: &Config) -> anyhow::Result<(String, String)> {
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

/// Installs the SIGTERM and Ctrl-C handlers now and returns a future that resolves when
/// either arrives. Installing early means a signal sent right after startup is not lost.
#[cfg(unix)]
fn termination_signal() -> anyhow::Result<impl Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).context("cannot install SIGTERM handler")?;
    let mut int = signal(SignalKind::interrupt()).context("cannot install Ctrl-C handler")?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    })
}

#[cfg(not(unix))]
fn termination_signal() -> anyhow::Result<impl Future<Output = ()>> {
    Ok(async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    })
}

/// The command to load the env, with the file flags when they are not the defaults.
fn env_hint(cfg: &Config) -> String {
    let mut cmd = String::from("hoard env");
    if cfg.port_file != default_port_file() {
        cmd.push_str(&format!(" --port-file {}", cfg.port_file.display()));
    }
    if cfg.token_file != default_token_file() {
        cmd.push_str(&format!(" --token-file {}", cfg.token_file.display()));
    }
    format!("eval \"$({cmd})\"")
}

/// Runs the server until `POST /_hoard/shutdown`, SIGTERM or Ctrl-C, then runs the shutdown
/// work (hooks, summary) and removes the port and token files.
pub async fn run_serve(cfg: Config) -> anyhow::Result<()> {
    let (prefix, storage_line) = storage_layout(&cfg)?;
    let store = Arc::new(Store::new(build_operator(&cfg)?));
    let store_for_shutdown = store.clone();
    let startup_lines: Vec<String> = lifecycle_startup(&cfg).await?.into_iter().collect();

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
        prefix: prefix.clone(),
        cache_version: cfg.cache_version.clone(),
        shutdown: shutdown_tx,
    };

    let signal = termination_signal()?;

    // The token file first: the port file is the readiness signal.
    write_atomic(&cfg.token_file, &token, true)?;
    if let Err(e) = write_atomic(&cfg.port_file, &port.to_string(), false) {
        remove_if_present(&cfg.token_file);
        return Err(e);
    }

    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .into_future(),
    );
    info!(
        "hoard listening on http://127.0.0.1:{port} — run: {}",
        env_hint(&cfg)
    );

    let reply = tokio::select! {
        r = shutdown_rx.recv() => r,
        _ = signal => None,
    };
    info!("shutting down");

    let cache = ShutdownCache {
        store: &store_for_shutdown,
        prefix: &prefix,
    };
    shutdown_work(&cfg, cache, &stats, &storage_line, startup_lines).await;
    remove_if_present(&cfg.port_file);
    remove_if_present(&cfg.token_file);

    if let Some(reply) = reply {
        let _ = reply.send(());
    }
    // Later shutdown requests: their reply channels are dropped here, so they answer 200.
    drop(shutdown_rx);
    let _ = stop_tx.send(());
    server.await.context("server task failed")??;
    Ok(())
}

/// Startup step for `storage=r2` (spec §7.1): ensure or verify the lifecycle rule, and return
/// the summary's `Lifecycle:` line. A missing rule (`verify`) or AccessDenied is a setup error;
/// a transient error or a check slower than [`LIFECYCLE_STARTUP_TIMEOUT`] is only a warning,
/// since the size cap still bounds the bucket.
async fn lifecycle_startup(cfg: &Config) -> anyhow::Result<Option<String>> {
    lifecycle_startup_within(cfg, LIFECYCLE_STARTUP_TIMEOUT).await
}

/// [`lifecycle_startup`] with the time bound as a parameter (tests).
async fn lifecycle_startup_within(cfg: &Config, bound: Duration) -> anyhow::Result<Option<String>> {
    let Some(r2) = cfg.r2.as_ref().filter(|_| cfg.storage == Storage::R2) else {
        return Ok(None);
    };
    if cfg.r2_lifecycle == LifecycleMode::Manual {
        warn!("r2-lifecycle=manual: the bucket can grow until the size cap runs");
        return Ok(Some("Lifecycle: manual".into()));
    }
    let client = r2_client(&r2.endpoint(), &r2.access_key_id, &r2.secret_access_key);
    let days = cfg.max_age_days;
    let check = ensure_lifecycle(&client, &r2.bucket, &r2.prefix, days, cfg.r2_lifecycle);
    let Ok(result) = tokio::time::timeout(bound, check).await else {
        warn!(
            "the R2 lifecycle check did not finish within {}s; continuing (the size cap still \
             bounds the bucket). `hoard doctor` checks the rule; a bucket admin can add it with:\n{}",
            bound.as_secs_f32(),
            fix_command(&r2.bucket, &r2.prefix, days)
        );
        return Ok(Some("Lifecycle: not checked (timed out)".into()));
    };
    match result {
        Ok(status) => {
            info!(rule = rule_id(&r2.prefix), ?status, "R2 lifecycle rule ok");
            Ok(Some(format!("Lifecycle: {days} days ✓")))
        }
        Err(e @ LifecycleError::Other { .. }) => {
            warn!(
                error = %e,
                "cannot check the R2 lifecycle rule; continuing (the size cap still bounds the bucket)"
            );
            Ok(Some(format!("Lifecycle: not checked ({})", e.reason())))
        }
        Err(e) => Err(e.into()),
    }
}

/// The store the server used and its key prefix, for the shutdown hooks.
#[derive(Clone, Copy)]
struct ShutdownCache<'a> {
    store: &'a Store,
    prefix: &'a str,
}

/// Shutdown hook for `storage=r2`, `mode=read-write` (spec §7.2): enforce `max-size`, bounded
/// by [`PRUNE_TIMEOUT`]. Returns the summary lines: `Pruned: ...` when something was deleted,
/// `Prune: failed (...)` on an error or timeout (also logged), nothing otherwise.
async fn prune_on_shutdown(cfg: &Config, cache: ShutdownCache<'_>) -> Vec<String> {
    prune_on_shutdown_within(cfg, cache, PRUNE_TIMEOUT).await
}

/// [`prune_on_shutdown`] with the time bound as a parameter (tests).
async fn prune_on_shutdown_within(
    cfg: &Config,
    cache: ShutdownCache<'_>,
    bound: Duration,
) -> Vec<String> {
    let mut lines = Vec::new();
    if cfg.storage != Storage::R2 || cfg.mode != Mode::ReadWrite {
        return lines;
    }
    let run = prune(
        cache.store,
        cache.prefix,
        cfg.max_size,
        chrono::Utc::now(),
        false,
    );
    match tokio::time::timeout(bound, run).await {
        Err(_) => {
            warn!(
                "prune did not finish within {}s; it runs again on the next shutdown",
                bound.as_secs()
            );
            lines.push("Prune: failed (timed out)".to_string());
        }
        Ok(Err(e)) => {
            warn!(error = %e, "prune failed; the lifecycle rule still bounds the bucket");
            lines.push(format!("Prune: failed ({})", short_reason(&e)));
        }
        Ok(Ok(r)) => {
            if r.deleted > 0 {
                lines.push(format!(
                    "Pruned: {} objects, {}",
                    r.deleted,
                    human_bytes(r.freed_bytes)
                ));
            }
            if r.delete_errors > 0 {
                warn!(
                    failed = r.delete_errors,
                    last_error = r.last_error.as_deref().unwrap_or(""),
                    "prune could not delete some objects; it runs again on the next shutdown"
                );
                lines.push(format!(
                    "Prune: failed ({} deletes failed)",
                    r.delete_errors
                ));
            }
        }
    }
    lines
}

/// A short, secret-free reason for the summary; the full error goes to the log.
fn short_reason(e: &StoreError) -> String {
    match e {
        StoreError::Storage(e) => e.kind().to_string(),
        StoreError::Body(e) => e.kind().to_string(),
        StoreError::BadPrefix(_) => "bad prefix".into(),
    }
}

/// Work done once on shutdown: hooks contribute extra summary lines (after the startup
/// lines), then the summary is written. Contract: this never fails. Hooks and the summary
/// report problems as warnings or summary lines, so shutdown always exits 0 (the cache must
/// never fail the build).
async fn shutdown_work(
    cfg: &Config,
    cache: ShutdownCache<'_>,
    stats: &Stats,
    storage_line: &str,
    startup_lines: Vec<String>,
) {
    let mut extra_lines = startup_lines;
    extra_lines.extend(prune_on_shutdown(cfg, cache).await);
    if let Some(path) = &cfg.summary_file {
        let md = render_summary(&stats.snapshot(), storage_line, &extra_lines);
        if let Err(e) = write_atomic(path, &md, false) {
            warn!(path = %path.display(), error = format!("{e:#}"), "cannot write summary file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServeArgs;
    use clap::Parser;

    /// A prune slower than the bound becomes a summary line, never an error.
    // Sends S3 requests to a server that never answers, so no pooled connection outlives the
    // test runtime (see `r2::tests::hung_endpoint_times_out`).
    #[tokio::test]
    async fn shutdown_prune_timeout_is_a_summary_line() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        let endpoint = format!("http://{addr}");
        let cfg = Config::try_from(ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-endpoint",
            &endpoint,
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "s",
            "--r2-bucket",
            "b",
        ]))
        .unwrap();
        let store = Store::new(build_operator(&cfg).unwrap());
        let cache = ShutdownCache {
            store: &store,
            prefix: "cache/",
        };
        let start = std::time::Instant::now();
        let lines = prune_on_shutdown_within(&cfg, cache, Duration::from_millis(200)).await;
        assert_eq!(lines, vec!["Prune: failed (timed out)"]);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    /// A lifecycle endpoint that never answers: startup gives up within the bound and goes on.
    #[tokio::test]
    async fn startup_lifecycle_check_is_bounded() {
        let endpoint = black_hole().await;
        let cfg = Config::try_from(ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-endpoint",
            &endpoint,
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "s",
            "--r2-bucket",
            "b",
        ]))
        .unwrap();
        let start = std::time::Instant::now();
        let line = lifecycle_startup_within(&cfg, Duration::from_millis(300))
            .await
            .unwrap();
        assert_eq!(line.as_deref(), Some("Lifecycle: not checked (timed out)"));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }

    /// An S3 endpoint that accepts connections and never answers.
    async fn black_hole() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn shutdown_prune_skipped_for_fs_and_read_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg =
            Config::try_from(ServeArgs::parse_from(["serve", "--storage", "fs"])).unwrap();
        cfg.fs_root = dir.path().to_path_buf();
        cfg.max_size = 0;
        let store = Store::new(build_operator(&cfg).unwrap());
        store.op.write("cache/aa", vec![0u8; 10]).await.unwrap();
        let cache = ShutdownCache {
            store: &store,
            prefix: "cache/",
        };
        assert!(prune_on_shutdown(&cfg, cache).await.is_empty());
        cfg.storage = Storage::R2;
        cfg.mode = Mode::Read;
        assert!(prune_on_shutdown(&cfg, cache).await.is_empty());
        assert!(store.op.exists("cache/aa").await.unwrap());
    }
}
