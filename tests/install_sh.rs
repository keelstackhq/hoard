//! scripts/install.sh offline: a fake `curl` first in PATH serves fixtures by the URL's last segment.
#![cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const FAKE_CURL: &str = r#"#!/bin/sh
case "$*" in *"--proto =https"*"--proto-redir =https"*"-fsSL"*"--retry 3"*) ;; *) echo "bad curl flags: $*" >&2; exit 2 ;; esac
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift 2 ;; *) url="$1"; shift ;; esac; done
echo "${url##*/}" >> "$FIX/curl.log"
cp "$FIX/${url##*/}" "$out"
"#;

fn target() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64-unknown-linux-musl",
        _ => "x86_64-unknown-linux-musl",
    }
}

fn sha256(f: &Path) -> String {
    let o = Command::new("sha256sum").arg(f).output().unwrap();
    String::from_utf8(o.stdout).unwrap()[..64].to_string()
}

struct Fix {
    dir: tempfile::TempDir,
    hash: String,
}

impl Fix {
    fn new() -> Fix {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir(d.join("bin")).unwrap();
        std::fs::write(d.join("bin/curl"), FAKE_CURL).unwrap();
        let tgz = format!("hoard-{}.tar.gz", target());
        let script = format!(
            "printf '#!/bin/sh\necho fake hoard\n' > hoard && chmod +x hoard bin/curl && tar -czf {tgz} hoard"
        );
        let sh = Command::new("sh")
            .args(["-c", &script])
            .current_dir(d)
            .status();
        assert!(sh.unwrap().success());
        let hash = sha256(&d.join(tgz));
        Fix { dir, hash }
    }

    fn sums(&self, version: &str, hash: &str) {
        let t = |t| format!("  \"{t}-unknown-linux-musl\": \"{hash}\",\n");
        let body =
            format!("export const VERSION = \"{version}\";\n\nexport const CHECKSUMS = {{\n");
        let sums = body + &t("x86_64") + &t("aarch64") + "};\n";
        std::fs::write(self.dir.path().join("checksums.generated.ts"), sums).unwrap();
    }

    /// (exit code, stderr, install dir) of `sh install.sh`.
    fn run(&self, env: &[(&str, &str)]) -> (i32, String, tempfile::TempDir) {
        let d = self.dir.path();
        let out = tempfile::tempdir().unwrap();
        let path = format!(
            "{}:{}",
            d.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh");
        let o = Command::new("sh")
            .arg(script)
            .env("PATH", path)
            .env("FIX", d)
            .env("HOARD_INSTALL_DIR", out.path())
            .envs(env.iter().copied())
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        (o.status.code().unwrap(), err, out)
    }

    fn curl_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("curl.log")).unwrap_or_default()
    }
}

#[test]
fn installs_a_verified_release() {
    let f = Fix::new();
    f.sums("0.4.0", &f.hash);
    let (code, err, out) = f.run(&[("HOARD_VERSION", "v0.4.0")]);
    assert_eq!(code, 0, "{err}");
    let bin = out.path().join("hoard");
    assert!(std::fs::metadata(&bin).unwrap().permissions().mode() & 0o111 != 0);
    let o = Command::new(&bin).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "fake hoard\n");
}

#[test]
fn rejects_a_checksum_mismatch() {
    let f = Fix::new();
    f.sums("0.4.0", &"0".repeat(64));
    let (code, err, out) = f.run(&[("HOARD_VERSION", "0.4.0")]);
    assert_eq!(code, 1);
    assert!(err.contains("checksum mismatch"), "{err}");
    assert!(!out.path().join("hoard").exists());
}

#[test]
fn rejects_a_target_missing_from_the_sums() {
    let f = Fix::new();
    std::fs::write(
        f.dir.path().join("checksums.generated.ts"),
        "export const VERSION = \"0.4.0\";\n",
    )
    .unwrap();
    let (code, err, _) = f.run(&[("HOARD_VERSION", "0.4.0")]);
    assert_eq!(code, 1);
    assert!(err.contains("no checksum for"), "{err}");
}

#[test]
fn rejects_a_sums_file_of_another_version() {
    let f = Fix::new();
    f.sums("0.3.0", &f.hash);
    let (code, err, _) = f.run(&[("HOARD_VERSION", "0.4.0")]);
    assert_eq!(code, 1);
    assert!(
        err.contains("checksums file at v0.4.0 is for another version"),
        "{err}"
    );
}

#[test]
fn pinned_sha256_skips_the_sums_file() {
    let f = Fix::new();
    f.sums("9.9.9", &"0".repeat(64));
    let (code, err, _) = f.run(&[("HOARD_VERSION", "0.4.0"), ("HOARD_SHA256", &f.hash)]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !f.curl_log().contains("checksums.generated.ts"),
        "{}",
        f.curl_log()
    );
}

#[test]
fn wrong_or_malformed_pinned_sha256_is_rejected() {
    let f = Fix::new();
    for (pin, want) in [
        ("a".repeat(64), "checksum mismatch"),
        ("ABC".into(), "HOARD_SHA256"),
    ] {
        let (code, err, _) = f.run(&[("HOARD_VERSION", "0.4.0"), ("HOARD_SHA256", &pin)]);
        assert!(code == 1 && err.contains(want), "{code} {err}");
    }
}

#[test]
fn bad_versions_are_rejected_before_any_download() {
    let f = Fix::new();
    for v in ["1.2", "1.2.3/../x", "1.2.3;id", "latest", "1.2.3\n/../x"] {
        let (code, err, _) = f.run(&[("HOARD_VERSION", v)]);
        assert_eq!(code, 1, "{v}");
        assert!(err.contains("HOARD_VERSION"), "{err}");
    }
    assert_eq!(f.curl_log(), "");
}

#[test]
fn the_real_checksums_file_parses() {
    let f = Fix::new();
    let real = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/action/src/checksums.generated.ts"
    );
    std::fs::copy(real, f.dir.path().join("checksums.generated.ts")).unwrap();
    let text = std::fs::read_to_string(real).unwrap();
    let version = text
        .split("VERSION = \"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let (code, err, _) = f.run(&[("HOARD_VERSION", version)]);
    assert_eq!(code, 1);
    assert!(err.contains("checksum mismatch"), "{err}");
}
