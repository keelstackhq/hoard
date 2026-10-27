//! `serve --detach` and `stop`, for CI systems without a JavaScript action.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Args, ValueEnum};

use crate::config::{Config, TokenPaths, default_log_file, default_pid_file};
use crate::config::{default_port_file, default_summary_file};
use crate::serve::read_server;

#[derive(Args, Debug, Clone)]
pub struct DetachArgs {
    /// Run in the background; exit 0 once ready (files: --pid-file, --log-file).
    #[arg(long, env = "HOARD_DETACH", value_parser = clap::builder::BoolishValueParser::new())]
    pub detach: bool,
    #[arg(long, env = "HOARD_PID_FILE")]
    pub pid_file: Option<PathBuf>,
    #[arg(long, env = "HOARD_LOG_FILE")]
    pub log_file: Option<PathBuf>,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum SummaryFormat {
    Markdown,
}

#[derive(Args, Debug, Clone)]
pub struct StopArgs {
    #[command(flatten)]
    pub paths: TokenPaths,
    #[arg(long, env = "HOARD_PID_FILE")]
    pub pid_file: Option<PathBuf>,
    #[arg(long, env = "HOARD_SUMMARY_FILE")]
    pub summary_file: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub summary: Option<SummaryFormat>,
}

/// One HTTP/1.0 request (`"GET /path"`) to 127.0.0.1: (status, body); None if nothing answers.
fn http(port: u16, req: &str, token: &str, secs: u64) -> Option<(u16, String)> {
    let t = Duration::from_secs(secs);
    let mut s = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), t).ok()?;
    s.set_read_timeout(Some(t)).ok()?;
    write!(s, "{req} HTTP/1.0\r\nAuthorization: Bearer {token}\r\n\r\n").ok()?;
    let mut resp = String::new();
    s.read_to_string(&mut resp).ok()?;
    let (head, body) = resp.split_once("\r\n\r\n")?;
    Some((head.get(9..12)?.parse().ok()?, body.to_string()))
}

fn healthy(port_file: &Path) -> bool {
    let port = std::fs::read_to_string(port_file).unwrap_or_default();
    let ok = |p| http(p, "GET /_hoard/health", "", 1) == Some((200, "ok".into()));
    port.trim().parse().is_ok_and(ok)
}

/// Re-runs this command without `--detach` in its own process group; returns once it is healthy.
pub fn detach(cfg: &Config, a: &DetachArgs) -> anyhow::Result<()> {
    if cfg!(not(unix)) {
        bail!("--detach needs linux or macOS");
    }
    if healthy(&cfg.port_file) {
        let f = cfg.port_file.display();
        bail!("a hoard server is already running ({f}); stop it with `hoard stop`");
    }
    let _ = std::fs::remove_file(&cfg.port_file);
    let _ = std::fs::remove_file(&cfg.token_file);
    let pid_file = a.pid_file.clone().unwrap_or_else(default_pid_file);
    let log_file = a.log_file.clone().unwrap_or_else(default_log_file);
    for dir in [&pid_file, &log_file].iter().filter_map(|f| f.parent()) {
        std::fs::create_dir_all(dir)?;
    }
    let log = std::fs::File::options()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_file)?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(std::env::args_os().skip(1).filter(|a| a != "--detach"))
        .env_remove("HOARD_DETACH")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().context("cannot start hoard serve")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !healthy(&cfg.port_file) {
        let exit = child.try_wait()?;
        if exit.is_some() || Instant::now() > deadline {
            let _ = child.kill();
            let log = std::fs::read_to_string(&log_file).unwrap_or_default();
            let lines: Vec<_> = log.lines().collect();
            eprintln!("{}", lines[lines.len().saturating_sub(20)..].join("\n"));
            let why = exit.map_or("was not ready in 15 s".into(), |s| format!("exited ({s})"));
            let f = log_file.display();
            bail!("hoard serve {why} during startup; log: {f}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid = child.id();
    std::fs::write(&pid_file, format!("{pid}\n"))?;
    eprintln!("hoard started (pid {pid}); load the env with: eval \"$(hoard env)\"");
    Ok(())
}

pub fn stop(a: &StopArgs) -> anyhow::Result<()> {
    let (port, token) = read_server(&a.paths, "hoard serve --detach")?;
    let Some((code, _)) = http(port, "POST /_hoard/shutdown", &token, 300) else {
        let f = a.paths.port_file.clone().unwrap_or_else(default_port_file);
        let f = f.display();
        bail!("no hoard server answers on 127.0.0.1:{port} (stale {f}); nothing to stop");
    };
    if !(200..300).contains(&code) {
        bail!("hoard shutdown failed: HTTP {code}");
    }
    let pid_file = a.pid_file.clone().unwrap_or_else(default_pid_file);
    if let Ok(pid) = std::fs::read_to_string(&pid_file) {
        let stat = format!("/proc/{}/stat", pid.trim());
        let alive = || std::fs::read_to_string(&stat).is_ok_and(|s| !s.contains(") Z"));
        let deadline = Instant::now() + Duration::from_secs(30);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_file(&pid_file);
    }
    let file = a.summary_file.clone().unwrap_or_else(default_summary_file);
    match (a.summary, std::fs::read_to_string(&file)) {
        (None, _) => eprintln!("hoard stopped"),
        (_, Ok(md)) => print!("{md}"),
        (_, Err(e)) => eprintln!("warning: cannot read summary {}: {e}", file.display()),
    }
    Ok(())
}
