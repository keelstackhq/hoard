use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::process::Command;

fn hoard(d: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hoard"));
    for f in ["PORT", "TOKEN", "PID", "LOG", "SUMMARY"] {
        c.env(format!("HOARD_{f}_FILE"), d.join(f));
    }
    c.env("HOARD_STORAGE", "fs")
        .env("HOARD_FS_ROOT", d.join("store"));
    let o = c.args(args).output().unwrap();
    let text = |b| String::from_utf8_lossy(b).into_owned();
    (o.status.code().unwrap(), text(&o.stdout), text(&o.stderr))
}

fn http(port: u16, method: &str, token: &str, body: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let n = body.len();
    let head =
        format!("{method} /v8/artifacts/abc123 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
    let rest = format!("Authorization: Bearer {token}\r\nContent-Length: {n}\r\n\r\n{body}");
    s.write_all((head + &rest).as_bytes()).unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).unwrap();
    r
}

/// The process has exited (no `/proc` entry, or a zombie).
fn gone(pid: &str) -> bool {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()));
    stat.map_or(true, |s| s.contains(") Z"))
}

/// Kills the detached server if the test fails before `hoard stop`.
struct PidGuard(std::path::PathBuf);

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(&self.0) {
            let pid = pid.lines().next().unwrap_or_default();
            let _ = Command::new("kill").args(["-9", pid]).status();
        }
    }
}

struct Killer(String);

impl Drop for Killer {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-9", &self.0]).status();
    }
}

#[test]
fn detach_put_get_stop() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let _guard = PidGuard(d.join("PID"));
    std::fs::write(d.join("victim"), "keep").unwrap();
    std::os::unix::fs::symlink(d.join("victim"), d.join("LOG")).unwrap();
    let (code, _, err) = hoard(d, &["serve", "--detach"]);
    assert_eq!(code, 0, "{err}");
    let log = std::fs::symlink_metadata(d.join("LOG")).unwrap(); // fresh, private, no symlink
    assert!(log.file_type().is_file() && log.mode() & 0o777 == 0o600);
    assert_eq!(std::fs::read_to_string(d.join("victim")).unwrap(), "keep");
    let pid = std::fs::read_to_string(d.join("PID")).unwrap();
    let pid = pid.lines().next().unwrap().to_string();
    let (code, _, err) = hoard(d, &["serve", "--detach"]);
    assert!(code == 1 && err.contains("already running"), "{code} {err}");
    let env = hoard(d, &["env"]).1;
    let f: Vec<_> = env.split(['=', ':', '\n']).collect(); // [3] port, [5] token
    let (port, token) = (f[3].parse().unwrap(), f[5]);
    assert!(http(port, "PUT", token, "hello").starts_with("HTTP/1.1 200"));
    assert!(http(port, "GET", token, "").ends_with("\r\n\r\nhello"));

    let (code, out, err) = hoard(d, &["stop", "--summary", "markdown"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("### hoard\n| Hits | Misses"), "{out}");
    assert!(gone(&pid), "server {pid} still running");
    assert!(!d.join("PORT").exists() && !d.join("PID").exists());
}

#[test]
fn startup_failure_prints_log_tail() {
    let dir = tempfile::tempdir().unwrap();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port().to_string();
    let (code, _, err) = hoard(dir.path(), &["serve", "--detach", "--port", &port]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("cannot listen on 127.0.0.1"), "{err}");
}

#[test]
fn stop_sigterms_a_hoard_that_does_not_answer() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let _guard = PidGuard(d.join("PID"));
    assert_eq!(hoard(d, &["serve", "--detach"]).0, 0);
    let pid = std::fs::read_to_string(d.join("PID")).unwrap();
    let pid = pid.lines().next().unwrap().to_string();
    std::fs::write(d.join("PORT"), "1").unwrap(); // stop asks port 1: nothing answers
    let (code, _, err) = hoard(d, &["stop"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("did not answer the shutdown request"), "{err}");
    assert!(gone(&pid), "{pid} still runs");
}

#[test]
fn stop_never_signals_a_pid_that_is_not_a_hoard_group() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (code, _, err) = hoard(d, &["stop"]); // no server at all
    assert!(
        code == 1 && err.contains("no running hoard server found"),
        "{err}"
    );
    std::fs::write(d.join("PORT"), "1").unwrap();
    std::fs::write(d.join("TOKEN"), "t").unwrap();
    let mut sleeper = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    for pid in [sleeper.id().to_string(), "1".into(), "".into()] {
        std::fs::write(d.join("PID"), &pid).unwrap();
        let (code, _, err) = hoard(d, &["stop"]);
        assert_eq!(code, 1, "{pid}: {err}");
        assert!(err.contains("nothing to stop"), "{pid}: {err}");
    }
    assert!(sleeper.try_wait().unwrap().is_none(), "sleep was signalled");
    let _ = sleeper.kill();
}

fn start_with_port_1(d: &std::path::Path) -> String {
    assert_eq!(hoard(d, &["serve", "--detach"]).0, 0);
    let pid = std::fs::read_to_string(d.join("PID")).unwrap();
    std::fs::write(d.join("PORT"), "1").unwrap(); // nothing answers on port 1
    pid
}

#[test]
fn stop_ignores_a_reused_pid_with_another_start_time() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let _guard = PidGuard(d.join("PID"));
    let saved = start_with_port_1(d);
    let (pid, start) = saved
        .trim()
        .split_once('\n')
        .expect("pid file: pid and start time");
    assert!(start.parse::<u64>().is_ok(), "{saved:?}");
    std::fs::write(d.join("PID"), format!("{pid}\n1\n")).unwrap();
    let (code, _, err) = hoard(d, &["stop"]);
    assert!(code == 1 && err.contains("nothing to stop"), "{code} {err}");
    assert!(!gone(pid), "server {pid} was signalled");
    std::fs::write(d.join("PID"), &saved).unwrap();
    std::fs::write(d.join("PORT"), "1").unwrap();
    assert_eq!(hoard(d, &["stop"]).0, 0);
    assert!(gone(pid));
}

#[test]
fn stop_warns_and_returns_when_kill_cannot_run() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let _guard = PidGuard(d.join("PID"));
    let pid = start_with_port_1(d);
    let pid = pid.lines().next().unwrap().to_string();
    let _kill = Killer(pid.clone()); // `stop` removes the pid file, so the guard above cannot
    let empty = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let o = Command::new(env!("CARGO_BIN_EXE_hoard"))
        .env("PATH", empty.path())
        .env("HOARD_PORT_FILE", d.join("PORT"))
        .env("HOARD_TOKEN_FILE", d.join("TOKEN"))
        .env("HOARD_PID_FILE", d.join("PID"))
        .env("HOARD_SUMMARY_FILE", d.join("SUMMARY"))
        .arg("stop")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(
        err.contains("warning: could not send SIGTERM to hoard process group"),
        "{err}"
    );
    assert!(t.elapsed().as_secs() < 10, "waited {:?}", t.elapsed());
    assert!(!gone(&pid));
    assert!(!err.contains("hoard stopped"), "{err}");
    assert!(d.join("PID").exists(), "pid file kept for a later stop");
}
