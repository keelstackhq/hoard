//! Runs the start and stop steps of `circleci/hoard.yml` offline, with fake `curl`, `uname`
//! and `hoard`.
#![cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// The start step's script (the first `command: |` block), parameters substituted.
fn start_script(sha256: &str) -> String {
    step_script(0, sha256)
}

/// The `n`th `command: |` block (0 start, 1 stop), dedented, with parameters substituted.
fn step_script(n: usize, sha256: &str) -> String {
    let yml = std::fs::read_to_string("circleci/hoard.yml").unwrap();
    let all: Vec<_> = yml.lines().collect();
    let starts: Vec<_> = (0..all.len())
        .filter(|&i| all[i].trim() == "command: |")
        .collect();
    let first = *starts
        .get(n)
        .expect("no such command block in circleci/hoard.yml");
    let mut lines = all[first..].iter().copied();
    let head = lines.next().unwrap();
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
        ("max-size", "1GB"),
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
    run_with(sha256, uname, 0)
}

/// [`run`], with the fake `hoard env` exiting `env_exit`.
fn run_with(sha256: impl Fn(&str) -> String, uname: &str, env_exit: i32) -> Run {
    let (home, tmp, bin, fix) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let fake = format!(
        "case \"$1\" in serve) echo fake serve ;; env) echo \"export TURBO_API=http://127.0.0.1:1\"; exit {env_exit} ;; esac"
    );
    script_file(fix.path(), "hoard", &fake);
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

/// `HOARD_SKIP` after sourcing the `$BASH_ENV` the start step wrote.
fn skip_after_start(r: &Run) -> String {
    let env = r.home.path().join("bash_env");
    let o = Command::new("bash")
        .args(["-c", ". \"$1\"; printf %s \"${HOARD_SKIP-unset}\"", "-"])
        .arg(&env)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn start_marks_skip_unless_the_server_runs() {
    assert_eq!(skip_after_start(&run(|s| s.into(), "Linux x86_64")), "");
    assert_eq!(
        skip_after_start(&run(|_| "0".repeat(64), "Linux x86_64")),
        "1"
    );
    assert_eq!(skip_after_start(&run(|s| s.into(), "Linux riscv64")), "1");
    // Installed and started, then `hoard env` fails: stop must still run.
    let r = run_with(|s| s.into(), "Linux x86_64", 1);
    assert_eq!(
        (r.code, skip_after_start(&r).as_str()),
        (1, ""),
        "{}",
        r.out
    );
}

/// Runs the stop step with a fake `hoard` (exit `code`) in PATH; returns (exit, hoard calls).
fn run_stop(skip: Option<&str>, code: i32) -> (i32, String) {
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("calls");
    let fake = format!("echo \"$*\" >> \"{}\"; exit {code}", log.display());
    script_file(bin.path(), "hoard", &fake);
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap()
    );
    let mut cmd = Command::new("bash");
    cmd.args(["-eo", "pipefail", "-c", &step_script(1, "")])
        .env_clear()
        .env("PATH", path);
    if let Some(skip) = skip {
        cmd.env("HOARD_SKIP", skip);
    }
    let o = cmd.output().unwrap();
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    (o.status.code().unwrap(), calls)
}

#[test]
fn stop_step_skips_when_start_did_not_run_hoard() {
    assert_eq!(run_stop(Some("1"), 0), (0, String::new()));
    // The start step never ran in this job: a `hoard` on PATH is not this job's.
    assert_eq!(run_stop(None, 0), (0, String::new()));
}

#[test]
fn stop_step_stops_and_prunes_and_never_fails() {
    for code in [0, 1] {
        let (exit, calls) = run_stop(Some(""), code);
        assert_eq!(exit, 0, "{calls}");
        let lines: Vec<_> = calls.lines().collect();
        assert_eq!(lines.len(), 2, "{calls}");
        assert!(lines[0].starts_with("stop --summary markdown"), "{calls}");
        assert!(
            lines[1].starts_with("prune --force --storage fs"),
            "{calls}"
        );
        assert!(lines[1].contains("--max-size 1GB"), "{calls}");
    }
}

/// `restore: false` (hull passes it on the default branch) skips only the fs `restore_cache`:
/// CircleCI caches are not branch-scoped, so a PR's edited config could save a poisoned
/// `hoard-v1-main~…` that main would restore. The default (`true`) keeps the restore.
#[test]
fn restore_parameter_guards_only_the_restore_cache_step() {
    let yml = std::fs::read_to_string("circleci/hoard.yml").unwrap();
    assert!(
        yml.contains("  restore:\n    type: boolean\n    default: true\n"),
        "restore parameter (boolean, default true) missing"
    );
    assert_eq!(yml.matches("<< parameters.restore >>").count(), 1, "{yml}");
    let at = yml.find("restore_cache:").expect("restore_cache step");
    let block = &yml[yml[..at].rfind("- when:").unwrap()..at];
    for guard in [
        "- equal: [start, << parameters.step >>]",
        "- equal: [fs, << parameters.storage >>]",
        "- << parameters.restore >>",
    ] {
        assert!(
            block.contains(guard),
            "restore_cache `when` lacks {guard}: {block}"
        );
    }
}
