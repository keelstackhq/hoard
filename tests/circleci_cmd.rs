//! Runs the start step of `circleci/hoard.yml` offline, with fake `curl` and `uname`.
#![cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// The first `command: |` block (the start step), dedented, with parameters substituted.
fn start_script(sha256: &str) -> String {
    let yml = std::fs::read_to_string("circleci/hoard.yml").unwrap();
    let mut lines = yml.lines().skip_while(|l| l.trim() != "command: |");
    let head = lines.next().expect("no command block");
    let outer = head.len() - head.trim_start().len();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let body: Vec<_> = lines
        .take_while(|l| l.trim().is_empty() || indent(l) > outer)
        .collect();
    let inner = indent(body[0]);
    let text: Vec<_> = body.iter().map(|l| l.get(inner..).unwrap_or("")).collect();
    let (storage, params) = ("fs", "<< parameters.");
    let mut s = text.join("\n");
    for (k, v) in [
        ("version", "0.4.0"),
        ("sha256", sha256),
        ("mode", "auto"),
        ("storage", storage),
        ("default-branch", "main"),
    ] {
        s = s.replace(&format!("{params}{k} >>"), v);
    }
    s
}

fn script_file(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Run {
    code: i32,
    out: String,
    curl_log: String,
    home: tempfile::TempDir,
    tmp: tempfile::TempDir,
}

/// Builds the fakes and a fixture archive, runs the start step with the pin `sha256(hash)`.
fn run(sha256: impl Fn(&str) -> String, uname: &str) -> Run {
    let (home, tmp, bin, fix) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let fake = "case \"$1\" in serve) echo fake serve ;; env) echo \"export TURBO_API=http://127.0.0.1:1\" ;; esac";
    script_file(fix.path(), "hoard", fake);
    let tgz = fix.path().join("h.tgz");
    let (t, f) = (tgz.to_str().unwrap(), fix.path().to_str().unwrap());
    let tar = ["-czf", t, "-C", f, "hoard"];
    assert!(Command::new("tar").args(tar).status().unwrap().success());
    let sum = Command::new("sha256sum").arg(&tgz).output().unwrap();
    let sum = String::from_utf8(sum.stdout).unwrap()[..64].to_string();
    script_file(
        bin.path(),
        "curl",
        "o=; for a; do [ \"$p\" = -o ] && o=$a; p=$a; echo \"$a\" >> \"$FAKE_CURL_LOG\"; done\ncp \"$FAKE_TGZ\" \"$o\"",
    );
    script_file(
        bin.path(),
        "uname",
        "case \"$1\" in -sm) echo \"$FAKE_UNAME\" ;; -m) echo \"${FAKE_UNAME#* }\" ;; *) echo Linux ;; esac",
    );
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap()
    );
    let log = fix.path().join("curl.log");
    let o = Command::new("bash")
        .args(["-eo", "pipefail", "-c", &start_script(&sha256(&sum))])
        .env_clear()
        .env("PATH", path)
        .env("HOME", home.path())
        .env("TMPDIR", tmp.path())
        .env("BASH_ENV", home.path().join("bash_env"))
        .env("FAKE_TGZ", &tgz)
        .env("FAKE_CURL_LOG", &log)
        .env("FAKE_UNAME", uname)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&[o.stdout, o.stderr].concat()).into_owned();
    let curl_log = std::fs::read_to_string(&log).unwrap_or_default();
    let code = o.status.code().unwrap();
    Run {
        code,
        out,
        curl_log,
        home,
        tmp,
    }
}

fn installed(r: &Run) -> bool {
    r.home.path().join(".local/bin/hoard").exists()
}

/// The start step keeps one state dir (port/token/pid files) in TMPDIR; the download dir must be gone.
fn state_dirs(r: &Run) -> usize {
    std::fs::read_dir(r.tmp.path()).unwrap().count()
}

#[test]
fn correct_pin_installs_without_install_sh() {
    let r = run(|s| s.into(), "Linux x86_64");
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(installed(&r), "{}", r.out);
    assert!(
        r.curl_log.contains(".tar.gz") && !r.curl_log.contains("install.sh"),
        "{}",
        r.curl_log
    );
    assert_eq!(state_dirs(&r), 1);
}

#[test]
fn wrong_pin_continues_without_the_cache() {
    let r = run(|_| "0".repeat(64), "Linux x86_64");
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(
        r.out.contains("does not match sha256") && r.out.contains("continuing without the cache"),
        "{}",
        r.out
    );
    assert!(!installed(&r));
    assert_eq!(state_dirs(&r), 1);
}

#[test]
fn unknown_arch_never_calls_curl() {
    let r = run(|s| s.into(), "Linux riscv64");
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(
        r.out.contains("no linux release") && r.curl_log.is_empty() && !installed(&r),
        "{}",
        r.out
    );
}

#[test]
fn malformed_pin_fails_the_step() {
    let r = run(|_| "abc".into(), "Linux x86_64");
    assert_eq!(r.code, 1, "{}", r.out);
    assert!(r.out.contains("64 lowercase hex"), "{}", r.out);
}
