mod common;

use common::SpawnGuarded;
use std::process::Command;

#[test]
fn empty_hoard_env_vars_behave_as_unset() {
    // Empty HOARD_STORAGE falls back to the default (github), which fails at startup without
    // the GitHub Actions env, after config parsing; so the command exits instead of serving.
    let out = hoard_without_actions_env()
        .args(["serve"])
        .env("HOARD_STORAGE", "")
        .env("HOARD_PORT", "")
        .env("HOARD_CACHE_VERSION", "")
        .env("HOARD_R2_BUCKET", "")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("invalid value"), "{stderr}");
    assert!(!stderr.contains("cache-version"), "{stderr}");
    assert!(
        stderr.contains("ACTIONS_RESULTS_URL is not set"),
        "{stderr}"
    );
}

/// The `hoard` binary with the GitHub Actions cache variables removed from its environment.
fn hoard_without_actions_env() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hoard"));
    for k in [
        "ACTIONS_RESULTS_URL",
        "ACTIONS_RUNTIME_TOKEN",
        "ACTIONS_CACHE_URL",
        "ACTIONS_CACHE_SERVICE_V2",
    ] {
        c.env_remove(k);
    }
    c
}

/// No silent fallback: `serve --storage github` without `ACTIONS_RESULTS_URL` exits non-zero
/// with a message naming the variable, before it binds or writes the port file.
#[test]
fn serve_github_without_results_url_fails_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let out = hoard_without_actions_env()
        .args(["serve", "--storage", "github"])
        .env("ACTIONS_RUNTIME_TOKEN", "secret-tok-123")
        .env("HOARD_PORT_FILE", dir.path().join("port"))
        .env("HOARD_TOKEN_FILE", dir.path().join("token"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "ACTIONS_RESULTS_URL is not set: run hoard through the GitHub Action, or use --storage r2|fs"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.contains("secret-tok-123"), "{stderr}");
    assert!(!dir.path().join("port").exists());
    assert!(!dir.path().join("token").exists());
}

#[test]
fn serve_github_without_runtime_token_fails_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let out = hoard_without_actions_env()
        .args(["serve", "--storage", "github"])
        .env("ACTIONS_RESULTS_URL", "https://results.example/")
        .env("ACTIONS_RUNTIME_TOKEN", "")
        .env("HOARD_PORT_FILE", dir.path().join("port"))
        .env("HOARD_TOKEN_FILE", dir.path().join("token"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "ACTIONS_RUNTIME_TOKEN is not set: run hoard through the GitHub Action, or use --storage r2|fs"
        ),
        "{stderr}"
    );
    assert!(!dir.path().join("port").exists());
}

/// Polls for a non-empty file for up to 10 s.
fn wait_for(path: &std::path::Path) -> String {
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(path)
            && !s.is_empty()
        {
            return s;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("{} did not appear", path.display());
}

/// One HTTP/1.1 request over a plain socket; returns the status code and the body.
fn http(port: u16, method: &str, path: &str, token: &str, body: &[u8]) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    s.write_all(body).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let status = resp[9..12].parse().unwrap();
    let body = resp.split_once("\r\n\r\n").map(|(_, b)| b.to_string());
    (status, body.unwrap_or_default())
}

/// `serve` the binary with `envs`, wait until ready, run `f(port, token)`, then shut it down
/// and return the summary.
fn with_server(args: &[&str], envs: &[(&str, &str)], f: impl FnOnce(u16, &str)) -> String {
    let dir = tempfile::tempdir().unwrap();
    let (port_file, token_file, summary) = (
        dir.path().join("port"),
        dir.path().join("token"),
        dir.path().join("summary.md"),
    );
    let mut cmd = hoard_without_actions_env();
    cmd.arg("serve")
        .args(args)
        .arg("--fs-root")
        .arg(dir.path().join("store"))
        .arg("--summary-file")
        .arg(&summary)
        .env("HOARD_PORT_FILE", &port_file)
        .env("HOARD_TOKEN_FILE", &token_file)
        .stderr(std::process::Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn_guarded();
    let port: u16 = wait_for(&port_file).trim().parse().unwrap();
    let token = std::fs::read_to_string(&token_file).unwrap();
    f(port, &token);
    assert_eq!(http(port, "POST", "/_hoard/shutdown", &token, b"").0, 200);
    assert!(child.wait().unwrap().success());
    std::fs::read_to_string(&summary).unwrap()
}

/// With the GitHub Actions env present, `serve --storage github` starts (no request is made
/// at startup), and the summary names the storage; no lifecycle or prune line.
#[test]
fn serve_github_starts_and_summary_names_storage() {
    let summary = with_server(
        &["--storage", "github"],
        &[
            ("ACTIONS_RESULTS_URL", "http://127.0.0.1:9/"),
            ("ACTIONS_RUNTIME_TOKEN", "tok"),
        ],
        |_, _| {},
    );
    assert!(summary.ends_with("|\nStorage: github\n"), "{summary}");
}

#[test]
fn no_reads_env_values() {
    for (value, get_status) in [("1", 404), ("true", 404), ("0", 200), ("false", 200)] {
        with_server(
            &["--storage", "fs"],
            &[("HOARD_NO_READS", value)],
            |port, token| {
                let (put, _) = http(port, "PUT", "/v8/artifacts/ab", token, b"body");
                assert_eq!(put, 200);
                let (get, _) = http(port, "GET", "/v8/artifacts/ab", token, b"");
                assert_eq!(get, get_status, "HOARD_NO_READS={value}");
            },
        );
    }
}

#[test]
fn env_without_running_server_fails_with_hint() {
    let dir = tempfile::tempdir().unwrap();
    let port_file = dir.path().join("port");
    let out = Command::new(env!("CARGO_BIN_EXE_hoard"))
        .arg("env")
        .arg("--port-file")
        .arg(&port_file)
        .arg("--token-file")
        .arg(dir.path().join("token"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!(
            "no running hoard server found (missing {}); start one with `hoard serve`",
            port_file.display()
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(out.stdout.is_empty());
}

#[test]
fn env_prints_export_lines_for_running_server() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("port"), "4123").unwrap();
    std::fs::write(dir.path().join("token"), "t0k").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_hoard"))
        .arg("env")
        .env("HOARD_PORT_FILE", dir.path().join("port"))
        .env("HOARD_TOKEN_FILE", dir.path().join("token"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "export TURBO_API=http://127.0.0.1:4123\nexport TURBO_TOKEN=t0k\nexport TURBO_TEAM=hoard\n"
    );
}

fn prune_cmd(root: &std::path::Path, max_size: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hoard"));
    c.args([
        "prune",
        "--storage",
        "fs",
        "--max-size",
        max_size,
        "--fs-root",
    ])
    .arg(root)
    .env("RUST_LOG", "warn");
    c
}

#[test]
fn prune_fs_prints_report_and_throttles() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    std::fs::create_dir_all(root.join("cache")).unwrap();
    for i in 0..5 {
        std::fs::write(root.join(format!("cache/obj{i:03}")), [b'x'; 100]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    let out = prune_cmd(&root, "250").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout,
        "pruned 3 of 5 objects, freed 300 B, 200 B remain (max 250 B)\n"
    );
    assert!(!root.join("cache/obj000").exists());
    assert!(root.join("cache/obj004").exists());

    let out = prune_cmd(&root, "250").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "skipped: last prune < 24 h ago (use --force)\n"
    );

    let out = prune_cmd(&root, "0").arg("--force").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "pruned 2 of 2 objects, freed 200 B, 0 B remain (max 0 B)\n"
    );
}

#[test]
fn prune_github_is_refused() {
    let out = hoard_without_actions_env()
        .args(["prune", "--storage", "github"])
        .env("ACTIONS_RESULTS_URL", "http://127.0.0.1:9/")
        .env("ACTIONS_RUNTIME_TOKEN", "tok")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("prune is only for r2/fs storage; GitHub evicts old cache entries itself"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn doctor_fs_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let out = Command::new(env!("CARGO_BIN_EXE_hoard"))
        .args(["doctor", "--storage", "fs", "--fs-root"])
        .arg(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("✓ bucket read/write"), "{stdout}");
    assert!(!root.join("cache/_hoard/doctor").exists());
}

#[test]
fn doctor_github_checks_the_environment_without_writing() {
    let out = hoard_without_actions_env()
        .args(["doctor", "--storage", "github"])
        .env("ACTIONS_RESULTS_URL", "http://127.0.0.1:9/")
        .env("ACTIONS_RUNTIME_TOKEN", "secret-tok")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}{stderr}");
    // Port 9 refuses connections: a write attempt would fail the doctor.
    assert_eq!(stdout, "✓ GitHub cache environment\n");
    assert!(!stdout.contains("secret-tok") && !stderr.contains("secret-tok"));
}

#[test]
fn doctor_github_without_env_fails_with_fix() {
    let out = hoard_without_actions_env()
        .args(["doctor", "--storage", "github"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(
            "✗ GitHub cache environment: ACTIONS_RESULTS_URL is not set: run hoard through the GitHub Action, or use --storage r2|fs"
        ),
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn serve_exits_0_on_sigterm_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let (port_file, token_file, summary) = (
        dir.path().join("port"),
        dir.path().join("token"),
        dir.path().join("summary.md"),
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_hoard"))
        .args(["serve", "--storage", "fs"])
        .arg("--fs-root")
        .arg(dir.path().join("store"))
        .arg("--summary-file")
        .arg(&summary)
        .env("HOARD_PORT_FILE", &port_file)
        .env("HOARD_TOKEN_FILE", &token_file)
        .stderr(std::process::Stdio::null())
        .spawn_guarded();
    let mut ready = false;
    for _ in 0..200 {
        if std::fs::read_to_string(&port_file).is_ok_and(|s| !s.is_empty()) {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if !ready {
        let _ = child.kill();
        panic!("server did not write its port file");
    }
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(
        std::fs::read_to_string(&summary)
            .unwrap()
            .contains("### hoard")
    );
    assert!(!port_file.exists() && !token_file.exists());
}
