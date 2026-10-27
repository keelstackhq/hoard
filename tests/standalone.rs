use std::io::{Read, Write};
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

/// Kills the detached server if the test fails before `hoard stop`.
struct PidGuard(std::path::PathBuf);

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(&self.0) {
            let _ = Command::new("kill").args(["-9", pid.trim()]).status();
        }
    }
}

#[test]
fn detach_put_get_stop() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let _guard = PidGuard(d.join("PID"));
    let (code, _, err) = hoard(d, &["serve", "--detach"]);
    assert_eq!(code, 0, "{err}");
    let pid = std::fs::read_to_string(d.join("PID")).unwrap();
    let env = hoard(d, &["env"]).1;
    let f: Vec<_> = env.split(['=', ':', '\n']).collect(); // [3] port, [5] token
    let (port, token) = (f[3].parse().unwrap(), f[5]);
    assert!(http(port, "PUT", token, "hello").starts_with("HTTP/1.1 200"));
    assert!(http(port, "GET", token, "").ends_with("\r\n\r\nhello"));

    let (code, out, err) = hoard(d, &["stop", "--summary", "markdown"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("### hoard\n| Hits | Misses"), "{out}");
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).unwrap_or_default();
    assert!(
        stat.is_empty() || stat.contains(") Z"),
        "server {pid} still running"
    );
    assert!(!d.join("PORT").exists() && !d.join("PID").exists());
}

#[test]
fn stop_without_server_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, err) = hoard(dir.path(), &["stop"]);
    assert_eq!(code, 1);
    assert!(err.contains("no running hoard server found"), "{err}");
}
