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
    anyhow::ensure!(cfg!(unix), "--detach needs linux or macOS");
    if healthy(&cfg.port_file) {
        let f = cfg.port_file.display();
        bail!("a hoard server is already running ({f}); stop it with `hoard stop`");
    }
    let pid_file = a.pid_file.clone().unwrap_or_else(default_pid_file);
    let log_file = a.log_file.clone().unwrap_or_else(default_log_file);
    // The log is created fresh and private below: O_EXCL never follows a symlink planted there.
    for f in [&cfg.port_file, &cfg.token_file, &log_file] {
        let _ = std::fs::remove_file(f); // a failure surfaces later (create_new, stale port)
    }
    for dir in [&pid_file, &log_file].iter().filter_map(|f| f.parent()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let log = opts.open(&log_file)?;
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
            let f = log_file.display();
            let Some(why) = startup_failure(exit) else {
                bail!("hoard serve exited with a setup error (see above); log: {f}");
            };
            return Err(StartFailed(format!("hoard serve did not start ({why}); log: {f}")).into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid = child.id();
    std::fs::write(&pid_file, format!("{pid}\n"))?;
    eprintln!("hoard started (pid {pid}); load the env with: eval \"$(hoard env)\"");
    Ok(())
}

/// Startup failure that is not a setup error: `main` exits 3 for it (exit 1 stays for setup errors).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StartFailed(String);

/// `Some(why)` for a startup failure that is not a setup error (exit 3); `None` for exit code 1.
fn startup_failure(exit: Option<std::process::ExitStatus>) -> Option<String> {
    let Some(status) = exit else {
        return Some("not ready in 15 s".into());
    };
    (status.code() != Some(1)).then(|| format!("exited ({status})"))
}

/// (comm, state, pgrp) from `/proc/<pid>/stat` (linux).
fn proc_stat(pid: u32) -> Option<(String, String, u32)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (head, tail) = s.rsplit_once(") ")?;
    let mut f = tail.split_whitespace();
    let (comm, state) = (head.split_once(" (")?.1.into(), f.next()?.into());
    Some((comm, state, f.nth(1)?.parse().ok()?))
}

/// A live hoard process leading its own process group (not ours): the only thing `stop` signals,
/// so a stale pid file never reaches another process.
fn hoard_group(pid: Option<u32>) -> Option<u32> {
    let own = proc_stat(std::process::id()).map(|s| s.2);
    let (comm, state, pgrp) = proc_stat(pid?)?;
    (comm == "hoard" && state != "Z" && Some(pgrp) == pid && Some(pgrp) != own).then_some(pgrp)
}

/// Waits up to `secs` for the group leader to exit; true once it is gone.
fn gone_within(pid: Option<u32>, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while hoard_group(pid).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    hoard_group(pid).is_none()
}

fn signal_group(pgrp: u32, sig: &str) {
    let _ = std::process::Command::new("kill")
        .args([sig, "--", &format!("-{pgrp}")])
        .status();
}

pub fn stop(a: &StopArgs) -> anyhow::Result<()> {
    let (port, token) = read_server(&a.paths, "hoard serve --detach")?;
    let pid_file = a.pid_file.clone().unwrap_or_else(default_pid_file);
    let pid = std::fs::read_to_string(&pid_file).unwrap_or_default();
    let pid = pid.trim().parse::<u32>().ok().filter(|&p| p > 1);
    let answered = match http(port, "POST /_hoard/shutdown", &token, 300) {
        Some((code, _)) if !(200..300).contains(&code) => {
            bail!("hoard shutdown failed: HTTP {code}")
        }
        reply => reply.is_some(),
    };
    match (answered, hoard_group(pid)) {
        (false, None) => {
            let f = a.paths.port_file.clone().unwrap_or_else(default_port_file);
            let f = f.display();
            bail!("no hoard server answers on 127.0.0.1:{port} (stale {f}); nothing to stop");
        }
        // Once the shutdown request is answered, hoard no longer acts on SIGTERM.
        (true, Some(p)) if !gone_within(pid, 30) => {
            eprintln!("warning: hoard (pid {p}) did not exit in 30 s; sending SIGKILL");
            signal_group(p, "-KILL");
        }
        (false, Some(p)) => {
            eprintln!("warning: hoard did not answer the shutdown request; sending SIGTERM");
            signal_group(p, "-TERM");
            if !gone_within(pid, 270) {
                eprintln!("warning: hoard (pid {p}) still runs after 270 s; sending SIGKILL");
                signal_group(p, "-KILL");
            }
        }
        _ => {}
    }
    let _ = std::fs::remove_file(&pid_file);
    let file = a.summary_file.clone().unwrap_or_else(default_summary_file);
    match (a.summary, std::fs::read_to_string(&file)) {
        (None, _) => eprintln!("hoard stopped"),
        (_, Ok(md)) => print!("{md}"),
        (_, Err(e)) => eprintln!("warning: cannot read summary {}: {e}", file.display()),
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::startup_failure;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus as S;

    #[test]
    fn only_exit_code_1_is_a_setup_error() {
        let why = |s| startup_failure(Some(S::from_raw(s))).unwrap_or_default();
        assert_eq!(startup_failure(Some(S::from_raw(1 << 8))), None);
        assert!(startup_failure(None).unwrap().contains("not ready in 15 s"));
        assert!(why(101 << 8).contains("101") && why(9).contains("signal: 9"));
    }
}
