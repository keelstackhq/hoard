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

#[test]
fn unimplemented_commands_fail_cleanly() {
    for cmd in ["prune", "doctor"] {
        let out = Command::new(env!("CARGO_BIN_EXE_hoard"))
            .args([cmd, "--storage", "fs"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("not implemented yet"), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
}
