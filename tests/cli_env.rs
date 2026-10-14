use std::process::Command;

#[test]
fn empty_hoard_env_vars_behave_as_unset() {
    // Empty HOARD_STORAGE falls back to the default (github), which fails at startup in this
    // milestone, after config parsing; so the command exits instead of serving.
    let out = Command::new(env!("CARGO_BIN_EXE_hoard"))
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
    assert!(stderr.contains("GitHub Action"), "{stderr}");
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
fn prune_github_not_supported_yet() {
    let out = Command::new(env!("CARGO_BIN_EXE_hoard"))
        .args(["prune", "--storage", "github"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--storage fs|r2"), "{stderr}");
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
fn doctor_github_not_supported_yet() {
    let out = Command::new(env!("CARGO_BIN_EXE_hoard"))
        .args(["doctor", "--storage", "github"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--storage fs|r2"), "{stderr}");
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
        .spawn()
        .unwrap();
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
