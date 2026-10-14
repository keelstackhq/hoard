//! The Vite+ task-cache archive: a tar of the directory's contents (relative paths), compressed
//! with zstd level 3 (with a frame checksum). File bytes, mtimes and the unix permission bits
//! are kept. Extraction rejects absolute paths, `..` components, links whose target leaves the
//! directory, and device or fifo entries.
//!
//! tar and zstd are synchronous; they run on a blocking thread and exchange bounded chunks with
//! the async side over a channel, so the archive is never held in memory as a whole.

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// zstd level for the archive (fast; the task cache is mostly already-compressed outputs).
const ZSTD_LEVEL: i32 = 3;

/// Size of the chunks passed between the blocking tar thread and the async side.
const CHUNK: usize = 256 * 1024;

/// Chunks in flight between the two sides (bounds memory to about `CHUNK * (DEPTH + 1)`).
const DEPTH: usize = 4;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// One entry of the directory walk: path relative to the root, full path, and its own
/// metadata (symlinks are not followed).
pub(crate) struct WalkEntry {
    pub rel: PathBuf,
    pub full: PathBuf,
    pub meta: std::fs::Metadata,
}

/// Every entry under `dir`, depth first and sorted by name; empty when `dir` does not exist.
pub(crate) fn walk(dir: &Path) -> io::Result<Vec<WalkEntry>> {
    let mut out = Vec::new();
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(invalid(format!("{} is not a directory", dir.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    }
    walk_into(dir, Path::new(""), &mut out)?;
    Ok(out)
}

fn walk_into(dir: &Path, rel: &Path, out: &mut Vec<WalkEntry>) -> io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let full = e.path();
        let rel = rel.join(e.file_name());
        let meta = std::fs::symlink_metadata(&full)?;
        let is_dir = meta.is_dir();
        out.push(WalkEntry {
            rel: rel.clone(),
            full: full.clone(),
            meta,
        });
        if is_dir {
            walk_into(&full, &rel, out)?;
        }
    }
    Ok(())
}

/// Sum of the sizes of the regular files under `dir`, and the number of files and symlinks
/// (0 means the directory is empty or missing). This is the size checked against
/// `vp-max-size`, before anything is archived.
pub fn dir_size(dir: &Path) -> io::Result<(u64, usize)> {
    let mut bytes = 0;
    let mut files = 0;
    for e in walk(dir)? {
        let t = e.meta.file_type();
        if t.is_file() {
            bytes += e.meta.len();
            files += 1;
        } else if t.is_symlink() {
            files += 1;
        }
    }
    Ok((bytes, files))
}

/// A `Write` that hands `CHUNK`-sized pieces to the async side.
struct ChanWriter {
    tx: mpsc::Sender<Bytes>,
    buf: Vec<u8>,
}

impl ChanWriter {
    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.tx
            .blocking_send(chunk)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "archive writer closed"))
    }
}

impl Write for ChanWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = data.len().min(CHUNK - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() >= CHUNK {
            self.send()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

/// A `Read` over chunks sent by the async side; a sent error is returned to the reader.
struct ChanReader {
    rx: mpsc::Receiver<io::Result<Bytes>>,
    cur: Bytes,
}

impl Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.cur.is_empty() {
            match self.rx.blocking_recv() {
                None => return Ok(0),
                Some(Err(e)) => return Err(e),
                Some(Ok(b)) => self.cur = b,
            }
        }
        let n = out.len().min(self.cur.len());
        out[..n].copy_from_slice(&self.cur.split_to(n));
        Ok(n)
    }
}

fn write_archive(dir: &Path, w: impl Write) -> io::Result<()> {
    let mut enc = zstd::Encoder::new(w, ZSTD_LEVEL)?;
    enc.include_checksum(true)?;
    let mut tar = tar::Builder::new(enc);
    tar.mode(tar::HeaderMode::Complete);
    tar.follow_symlinks(false);
    for e in walk(dir)? {
        let t = e.meta.file_type();
        if t.is_dir() {
            tar.append_dir(&e.rel, &e.full)?;
        } else if t.is_symlink() {
            // A link restore would refuse (absolute or with `..`) would fail the whole restore.
            if std::fs::read_link(&e.full).is_ok_and(|t| is_safe_link_target(&t)) {
                tar.append_path_with_name(&e.full, &e.rel)?;
            } else {
                tracing::warn!(path = %e.rel.display(), "not archiving a symlink that points outside the cache directory");
            }
        } else if t.is_file() {
            tar.append_path_with_name(&e.full, &e.rel)?;
        }
        // Sockets, fifos and devices are not cache content; leave them out.
    }
    let mut w = tar.into_inner()?.finish()?;
    w.flush()
}

/// Writes the tar.zst of `dir`'s contents to `out`; returns the number of bytes written.
pub async fn archive_dir(dir: &Path, out: impl AsyncWrite) -> io::Result<u64> {
    let (tx, mut rx) = mpsc::channel::<Bytes>(DEPTH);
    let dir = dir.to_path_buf();
    let job = tokio::task::spawn_blocking(move || {
        write_archive(
            &dir,
            ChanWriter {
                tx,
                buf: Vec::with_capacity(CHUNK),
            },
        )
    });
    let mut out = std::pin::pin!(out);
    let mut written = 0u64;
    let mut write_err = None;
    while let Some(chunk) = rx.recv().await {
        if let Err(e) = out.write_all(&chunk).await {
            write_err = Some(e);
            break;
        }
        written += chunk.len() as u64;
    }
    drop(rx); // a failed write stops the tar thread at its next send
    let built = job.await.map_err(io::Error::other)?;
    if let Some(e) = write_err {
        return Err(e);
    }
    built?;
    out.flush().await?;
    Ok(written)
}

/// True when `p` is a non-empty relative path of plain names (`.` allowed, no `..`, no root
/// or drive prefix).
fn is_safe_relative(p: &Path) -> bool {
    let mut any = false;
    for c in p.components() {
        match c {
            Component::Normal(_) => any = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    any
}

/// A symlink target is allowed only when it is relative and has no `..` component: it can then
/// only point at or below the link's own directory, however links are chained. (A lexical
/// check that allows `..` is not enough: `a/b -> ..` plus `a/b/c -> ..` escapes.)
fn is_safe_link_target(target: &Path) -> bool {
    target.components().next().is_some()
        && target
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Defence in depth after extraction: every symlink under `dir` that resolves must resolve
/// inside `dir`. A link that does not resolve (dangling, or a loop) is harmless.
pub(crate) fn check_links_inside(dir: &Path) -> io::Result<()> {
    let root = dir.canonicalize()?;
    for e in walk(dir)? {
        if !e.meta.file_type().is_symlink() {
            continue;
        }
        match e.full.canonicalize() {
            Ok(real) if real.starts_with(&root) => {}
            Ok(_) => {
                return Err(invalid(format!(
                    "archive symlink {} points outside the cache directory",
                    e.rel.display()
                )));
            }
            // Dangling, a loop (ELOOP) or otherwise unresolvable: there is nothing it points
            // at, and its target passed the lexical check, so it cannot lead outside.
            Err(_) => {}
        }
    }
    Ok(())
}

/// Refuses an entry that could write outside the root or is not plain cache content.
fn check_entry<R: Read>(e: &tar::Entry<'_, R>) -> io::Result<bool> {
    let path = e.path()?.into_owned();
    if !is_safe_relative(&path) {
        return Err(invalid(format!(
            "archive entry {} is absolute or contains ..",
            path.display()
        )));
    }
    use tar::EntryType as T;
    match e.header().entry_type() {
        T::Regular | T::Continuous | T::Directory => Ok(true),
        T::Symlink => {
            let target = e
                .link_name()?
                .ok_or_else(|| invalid("symlink without a target"))?;
            if !is_safe_link_target(&target) {
                return Err(invalid(format!(
                    "archive symlink {} points outside the cache directory",
                    path.display()
                )));
            }
            Ok(true)
        }
        T::Link => {
            let target = e
                .link_name()?
                .ok_or_else(|| invalid("hardlink without a target"))?;
            if !is_safe_relative(&target) {
                return Err(invalid(format!(
                    "archive hardlink {} points outside the cache directory",
                    path.display()
                )));
            }
            Ok(true)
        }
        // Metadata records with nothing to unpack.
        T::XGlobalHeader => Ok(false),
        other => Err(invalid(format!(
            "archive entry {} has unsupported type {other:?}",
            path.display()
        ))),
    }
}

fn unpack_into(r: impl Read, dir: &Path) -> io::Result<()> {
    let mut archive = tar::Archive::new(zstd::Decoder::new(r)?);
    archive.set_preserve_mtime(true);
    archive.set_overwrite(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !check_entry(&entry)? {
            continue;
        }
        if !entry.unpack_in(dir)? {
            return Err(invalid("archive entry outside the cache directory"));
        }
    }
    // Read to the end so a truncated or corrupt zstd frame (checksum) is an error.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    check_links_inside(dir)
}

/// Extracts a tar.zst from `body` into `dir` (which must exist).
pub async fn extract_stream(
    body: impl Stream<Item = io::Result<Bytes>>,
    dir: &Path,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(DEPTH);
    let dir = dir.to_path_buf();
    let job = tokio::task::spawn_blocking(move || {
        unpack_into(
            ChanReader {
                rx,
                cur: Bytes::new(),
            },
            &dir,
        )
    });
    let mut body = std::pin::pin!(body);
    while let Some(item) = body.next().await {
        let failed = item.is_err();
        // A closed channel means the tar thread stopped (done or failed); its result says which.
        if tx.send(item).await.is_err() || failed {
            break;
        }
    }
    drop(tx);
    job.await.map_err(io::Error::other)?
}

/// Extracts a tar.zst read from `input` into `dir` (which must exist).
pub async fn extract_archive(input: impl AsyncRead, dir: &Path) -> io::Result<()> {
    let input = Box::pin(input);
    let chunks = futures::stream::try_unfold(input, |mut r| async move {
        let mut buf = vec![0u8; CHUNK];
        let n = r.read(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.truncate(n);
        Ok(Some((Bytes::from(buf), r)))
    });
    extract_stream(chunks, dir).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn mtime(p: &Path) -> SystemTime {
        std::fs::metadata(p).unwrap().modified().unwrap()
    }

    fn set_mtime(p: &Path, t: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    async fn archive_bytes(dir: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        let n = archive_dir(dir, &mut out).await.unwrap();
        assert_eq!(n, out.len() as u64);
        out
    }

    /// A tar.zst with a single regular file whose raw name is `name` (bypassing the tar
    /// crate's path checks, as a hostile archive would).
    fn raw_archive(name: &[u8], kind: tar::EntryType, link: Option<&[u8]>) -> Vec<u8> {
        let mut h = tar::Header::new_gnu();
        h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
        if let Some(l) = link {
            h.as_gnu_mut().unwrap().linkname[..l.len()].copy_from_slice(l);
        }
        h.set_entry_type(kind);
        h.set_mode(0o644);
        let body: &[u8] = if kind == tar::EntryType::Regular {
            b"evil"
        } else {
            b""
        };
        h.set_size(body.len() as u64);
        h.set_cksum();
        let mut tar = tar::Builder::new(Vec::new());
        tar.append(&h, body).unwrap();
        zstd::encode_all(&tar.into_inner().unwrap()[..], 3).unwrap()
    }

    /// One raw entry: name, type, link target.
    type RawEntry<'a> = (&'a [u8], tar::EntryType, Option<&'a [u8]>);

    /// A tar.zst of raw entries; regular files hold `evil`.
    fn raw_entries(entries: &[RawEntry<'_>]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, kind, link) in entries {
            let mut h = tar::Header::new_gnu();
            h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
            if let Some(l) = link {
                h.as_gnu_mut().unwrap().linkname[..l.len()].copy_from_slice(l);
            }
            h.set_entry_type(*kind);
            h.set_mode(0o755);
            let body: &[u8] = if *kind == tar::EntryType::Regular {
                b"evil"
            } else {
                b""
            };
            h.set_size(body.len() as u64);
            h.set_cksum();
            tar.append(&h, body).unwrap();
        }
        zstd::encode_all(&tar.into_inner().unwrap()[..], 3).unwrap()
    }

    /// Each symlink alone stays inside, but `a/b/c` resolves through `a/b` (the root) to the
    /// root's parent.
    #[tokio::test]
    async fn chained_symlinks_are_rejected() {
        use tar::EntryType as T;
        let root = tempfile::tempdir().unwrap();
        let dst = root.path().join("dst");
        std::fs::create_dir(&dst).unwrap();
        let bytes = raw_entries(&[
            (b"a", T::Directory, None),
            (b"a/b", T::Symlink, Some(b"..")),
            (b"a/b/c", T::Symlink, Some(b"..")),
        ]);
        let err = extract_archive(&bytes[..], &dst).await.unwrap_err();
        assert!(err.to_string().contains("outside"), "{err}");
    }

    #[tokio::test]
    async fn absolute_symlink_then_file_through_it_is_rejected() {
        use tar::EntryType as T;
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("etc");
        std::fs::create_dir(&outside).unwrap();
        let dst = root.path().join("dst");
        std::fs::create_dir(&dst).unwrap();
        let target = outside.to_str().unwrap().as_bytes().to_vec();
        let bytes = raw_entries(&[
            (b"a", T::Symlink, Some(&target)),
            (b"a/passwd", T::Regular, None),
        ]);
        assert!(extract_archive(&bytes[..], &dst).await.is_err());
        assert!(!outside.join("passwd").exists());
    }

    #[cfg(unix)]
    #[test]
    fn links_escaping_after_extraction_are_found() {
        let root = tempfile::tempdir().unwrap();
        let dst = root.path().join("dst");
        std::fs::create_dir_all(dst.join("d")).unwrap();
        std::fs::write(dst.join("d/f"), b"x").unwrap();
        std::os::unix::fs::symlink("d/f", dst.join("ok")).unwrap();
        std::os::unix::fs::symlink("missing", dst.join("dangling")).unwrap();
        assert!(check_links_inside(&dst).is_ok());
        std::os::unix::fs::symlink(root.path(), dst.join("d/up")).unwrap();
        let err = check_links_inside(&dst).unwrap_err();
        assert!(err.to_string().contains("outside"), "{err}");
    }

    /// A link loop cannot leave the directory (its targets passed the lexical check), so it
    /// is as harmless as a dangling link and must not fail every restore of the archive.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_loops_round_trip() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("f"), b"x").unwrap();
        std::os::unix::fs::symlink("self", src.path().join("self")).unwrap();
        std::os::unix::fs::symlink("b", src.path().join("a")).unwrap();
        std::os::unix::fs::symlink("a", src.path().join("b")).unwrap();
        assert!(check_links_inside(src.path()).is_ok());
        let bytes = archive_bytes(src.path()).await;
        let dst = tempfile::tempdir().unwrap();
        extract_archive(&bytes[..], dst.path()).await.unwrap();
        assert_eq!(
            std::fs::read_link(dst.path().join("self")).unwrap(),
            Path::new("self")
        );
        assert_eq!(std::fs::read(dst.path().join("f")).unwrap(), b"x");
    }

    #[tokio::test]
    async fn round_trip_keeps_bytes_and_mtimes() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("a/b")).unwrap();
        let files = [
            ("cache.db", b"sqlite bytes".to_vec()),
            ("a/out.tar", vec![7u8; 300_000]),
            ("a/b/empty", Vec::new()),
        ];
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (i, (name, data)) in files.iter().enumerate() {
            let p = src.path().join(name);
            std::fs::write(&p, data).unwrap();
            set_mtime(&p, t + Duration::from_secs(i as u64 * 60));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = src.path().join("cache.db");
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o751)).unwrap();
        }
        let bytes = archive_bytes(src.path()).await;
        assert_eq!(&bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic");

        let dst = tempfile::tempdir().unwrap();
        extract_archive(&bytes[..], dst.path()).await.unwrap();
        for (i, (name, data)) in files.iter().enumerate() {
            let p = dst.path().join(name);
            assert_eq!(&std::fs::read(&p).unwrap(), data, "{name}");
            assert_eq!(mtime(&p), t + Duration::from_secs(i as u64 * 60), "{name}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dst.path().join("cache.db"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o751);
        }
        // Relative paths only: nothing named after the source directory.
        let names: Vec<_> = std::fs::read_dir(dst.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[tokio::test]
    async fn parent_dir_entry_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let dst = root.path().join("dst");
        std::fs::create_dir(&dst).unwrap();
        let bytes = raw_archive(b"../evil", tar::EntryType::Regular, None);
        let err = extract_archive(&bytes[..], &dst).await.unwrap_err();
        assert!(err.to_string().contains(".."), "{err}");
        assert!(!root.path().join("evil").exists());
    }

    #[tokio::test]
    async fn absolute_entry_is_rejected() {
        let dst = tempfile::tempdir().unwrap();
        let bytes = raw_archive(b"/tmp/hoard-evil", tar::EntryType::Regular, None);
        let err = extract_archive(&bytes[..], dst.path()).await.unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[tokio::test]
    async fn escaping_links_and_special_files_are_rejected() {
        for (name, kind, link) in [
            (&b"up"[..], tar::EntryType::Symlink, Some(&b"../../etc"[..])),
            (b"a/up", tar::EntryType::Symlink, Some(b"../..")),
            (b"abs", tar::EntryType::Symlink, Some(b"/etc/passwd")),
            (b"hard", tar::EntryType::Link, Some(b"../outside")),
            (b"fifo", tar::EntryType::Fifo, None),
            (b"dev", tar::EntryType::Char, None),
            (b"blk", tar::EntryType::Block, None),
        ] {
            let dst = tempfile::tempdir().unwrap();
            let bytes = raw_archive(name, kind, link);
            let err = extract_archive(&bytes[..], dst.path())
                .await
                .expect_err(&String::from_utf8_lossy(name));
            let msg = err.to_string();
            assert!(
                msg.contains("outside") || msg.contains("unsupported type"),
                "{msg}"
            );
            assert_eq!(std::fs::read_dir(dst.path()).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inner_symlink_round_trips() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("d")).unwrap();
        std::fs::write(src.path().join("d/f"), b"x").unwrap();
        std::os::unix::fs::symlink("d/f", src.path().join("link")).unwrap();
        let bytes = archive_bytes(src.path()).await;
        let dst = tempfile::tempdir().unwrap();
        extract_archive(&bytes[..], dst.path()).await.unwrap();
        let link = dst.path().join("link");
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("d/f"));
        assert_eq!(std::fs::read(&link).unwrap(), b"x");
    }

    #[tokio::test]
    async fn truncated_archive_is_an_error() {
        let src = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..400_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(src.path().join("f"), &data).unwrap();
        let bytes = archive_bytes(src.path()).await;
        let dst = tempfile::tempdir().unwrap();
        let cut = &bytes[..bytes.len() - 8];
        assert!(extract_archive(cut, dst.path()).await.is_err());
    }

    #[test]
    fn dir_size_counts_regular_files() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(dir_size(&d.path().join("missing")).unwrap(), (0, 0));
        std::fs::create_dir_all(d.path().join("x/y")).unwrap();
        assert_eq!(dir_size(d.path()).unwrap(), (0, 0));
        std::fs::write(d.path().join("x/a"), [0u8; 10]).unwrap();
        std::fs::write(d.path().join("x/y/b"), [0u8; 5]).unwrap();
        assert_eq!(dir_size(d.path()).unwrap(), (15, 2));
    }
}
