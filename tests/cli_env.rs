use std::process::Command;

#[test]
fn empty_hoard_env_vars_behave_as_unset() {
    let out = Command::new(env!("CARGO_BIN_EXE_hoard"))
        .args(["serve", "--storage", "fs"])
        .env("HOARD_PORT", "")
        .env("HOARD_CACHE_VERSION", "")
        .env("HOARD_R2_BUCKET", "")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("invalid value"), "{stderr}");
    assert!(!stderr.contains("cache-version"), "{stderr}");
}
