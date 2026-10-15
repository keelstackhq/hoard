//! A fingerprint of the task-cache directory, so `vp save` can skip a cache that is unchanged
//! since `vp restore` (spec §6.1): SHA-256 (hex) over the sorted (relative path, size, mtime)
//! of the regular files. File contents are not read.

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use super::archive::walk;

/// The fingerprint of `dir`'s regular files; a missing directory has the fingerprint of no files.
pub fn fingerprint(dir: &Path) -> io::Result<String> {
    let mut files: Vec<(String, u64, u128)> = Vec::new();
    for e in walk(dir)? {
        if !e.meta.file_type().is_file() {
            continue;
        }
        let mtime = e
            .meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let rel = e.rel.to_string_lossy().replace('\\', "/");
        files.push((rel, e.meta.len(), mtime));
    }
    files.sort();
    let mut h = Sha256::new();
    for (rel, size, mtime) in &files {
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(format!("{size}\0{mtime}\n").as_bytes());
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Where `vp restore` leaves the fingerprint for `vp save`:
/// `<RUNNER_TEMP, else ./.hoard>/hoard-vp-<os_arch>.fingerprint`.
pub fn fingerprint_file(runner_temp: Option<&str>, os_arch: &str) -> PathBuf {
    let dir = match runner_temp {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(".hoard"),
    };
    dir.join(format!("hoard-vp-{os_arch}.fingerprint"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn set_mtime(p: &Path, t: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn changes_with_path_size_and_mtime_only() {
        let d = tempfile::tempdir().unwrap();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        std::fs::create_dir_all(d.path().join("a")).unwrap();
        std::fs::write(d.path().join("a/f"), b"one").unwrap();
        set_mtime(&d.path().join("a/f"), t);
        let base = fingerprint(d.path()).unwrap();
        assert_eq!(base.len(), 64);
        assert_eq!(fingerprint(d.path()).unwrap(), base, "stable");

        // Same size and mtime, other bytes: not detected (contents are not read).
        std::fs::write(d.path().join("a/f"), b"two").unwrap();
        set_mtime(&d.path().join("a/f"), t);
        assert_eq!(fingerprint(d.path()).unwrap(), base);

        set_mtime(&d.path().join("a/f"), t + Duration::from_secs(1));
        let newer = fingerprint(d.path()).unwrap();
        assert_ne!(newer, base, "mtime");
        set_mtime(&d.path().join("a/f"), t);

        std::fs::write(d.path().join("a/f"), b"three").unwrap();
        set_mtime(&d.path().join("a/f"), t);
        assert_ne!(fingerprint(d.path()).unwrap(), base, "size");
        std::fs::write(d.path().join("a/f"), b"one").unwrap();
        set_mtime(&d.path().join("a/f"), t);

        std::fs::rename(d.path().join("a/f"), d.path().join("a/g")).unwrap();
        assert_ne!(fingerprint(d.path()).unwrap(), base, "path");
        std::fs::rename(d.path().join("a/g"), d.path().join("a/f")).unwrap();

        // Empty directories are not files.
        std::fs::create_dir(d.path().join("empty")).unwrap();
        assert_eq!(fingerprint(d.path()).unwrap(), base);
        std::fs::write(d.path().join("new"), b"").unwrap();
        assert_ne!(fingerprint(d.path()).unwrap(), base, "added file");
    }

    #[test]
    fn missing_dir_is_no_files() {
        let d = tempfile::tempdir().unwrap();
        let empty = fingerprint(d.path()).unwrap();
        assert_eq!(fingerprint(&d.path().join("missing")).unwrap(), empty);
    }

    #[test]
    fn file_lives_in_runner_temp_else_dot_hoard() {
        assert_eq!(
            fingerprint_file(Some("/tmp/rt"), "linux-x64"),
            Path::new("/tmp/rt/hoard-vp-linux-x64.fingerprint")
        );
        assert_eq!(
            fingerprint_file(None, "darwin-arm64"),
            Path::new(".hoard/hoard-vp-darwin-arm64.fingerprint")
        );
        assert_eq!(
            fingerprint_file(Some(""), "x"),
            Path::new(".hoard/hoard-vp-x.fingerprint")
        );
    }
}
