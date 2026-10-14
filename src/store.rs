//! Object store over an OpenDAL `Operator`. Every object is one HRD1 blob (see `envelope`).
//!
//! Backends: fs now; S3/R2 and GitHub cache arrive in later tasks.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt};
use opendal::{ErrorKind, Operator, Reader, services};

use crate::config::{Config, Storage};
use crate::envelope::{Header, MAX_HEADER_LEN, PREFIX_LEN, decode_header, encode_header};

/// Bytes read to decode a header: fixed prefix plus the largest accepted JSON header.
const HEAD_READ_LEN: u64 = (PREFIX_LEN + MAX_HEADER_LEN) as u64;

/// Errors from the storage backend. Messages never contain secrets.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage error: {0}")]
    Storage(#[from] opendal::Error),
    #[error("body stream error: {0}")]
    Body(#[from] std::io::Error),
}

pub enum GetOutcome {
    /// Header, a reader over the whole blob, and the offset where the artifact body starts.
    Hit(Header, Reader, u64),
    Miss,
    /// The object exists but is not a valid HRD1 blob.
    Corrupt,
}

impl std::fmt::Debug for GetOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hit(h, _, off) => f.debug_tuple("Hit").field(h).field(off).finish(),
            Self::Miss => f.write_str("Miss"),
            Self::Corrupt => f.write_str("Corrupt"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Created,
    AlreadyExists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub last_modified: DateTime<Utc>,
}

/// Builds the operator for the configured storage.
pub fn build_operator(cfg: &Config) -> anyhow::Result<Operator> {
    match cfg.storage {
        Storage::Fs => {
            std::fs::create_dir_all(&cfg.fs_root)?;
            let root = cfg
                .fs_root
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("--fs-root is not valid UTF-8"))?;
            // Write to a sibling temp dir and rename on close: readers never see a partial
            // object and an aborted write leaves nothing behind.
            let tmp = format!("{}.tmp", root.trim_end_matches('/'));
            std::fs::create_dir_all(&tmp)?;
            let fs = services::Fs::default().root(root).atomic_write_dir(&tmp);
            Ok(Operator::new(fs)?)
        }
        Storage::R2 => anyhow::bail!("storage=r2 is not yet supported in this build"),
        Storage::Github => anyhow::bail!("storage=github is not yet supported in this build"),
    }
}

#[derive(Clone)]
pub struct Store {
    pub op: Operator,
}

fn is_condition_failed(e: &opendal::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::ConditionNotMatch | ErrorKind::AlreadyExists
    )
}

fn to_utc(ts: opendal::raw::Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ts.into_inner().as_millisecond()).unwrap_or_default()
}

impl Store {
    pub fn new(op: Operator) -> Self {
        Self { op }
    }

    /// Reads the first `PREFIX_LEN + MAX_HEADER_LEN` bytes (never the body). Objects shorter
    /// than that are fine: a range past EOF is clamped using the object's size.
    async fn read_prefix(&self, reader: &Reader, key: &str) -> Result<Bytes, StoreError> {
        match reader.read(0..HEAD_READ_LEN).await {
            Ok(b) => Ok(b.to_bytes()),
            Err(e) if e.kind() == ErrorKind::RangeNotSatisfied => {
                let size = self.op.stat(key).await?.content_length().min(HEAD_READ_LEN);
                if size == 0 {
                    return Ok(Bytes::new());
                }
                Ok(reader.read(0..size).await?.to_bytes())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Header of `key` via a bounded range read. Missing or corrupt objects both yield `None`
    /// (a corrupt object is a miss); use [`Store::get`] to tell them apart.
    pub async fn head(&self, key: &str) -> Result<Option<Header>, StoreError> {
        match self.get(key).await? {
            GetOutcome::Hit(h, _, _) => Ok(Some(h)),
            GetOutcome::Miss | GetOutcome::Corrupt => Ok(None),
        }
    }

    /// Reads and validates the header only; the caller streams the body from the returned offset.
    pub async fn get(&self, key: &str) -> Result<GetOutcome, StoreError> {
        let reader = match self.op.reader(key).await {
            Ok(r) => r,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(GetOutcome::Miss),
            Err(e) => return Err(e.into()),
        };
        let prefix = match self.read_prefix(&reader, key).await {
            Ok(p) => p,
            Err(StoreError::Storage(e)) if e.kind() == ErrorKind::NotFound => {
                return Ok(GetOutcome::Miss);
            }
            Err(e) => return Err(e),
        };
        match decode_header(&prefix) {
            Ok((h, off)) => Ok(GetOutcome::Hit(h, reader, off as u64)),
            Err(_) => Ok(GetOutcome::Corrupt),
        }
    }

    /// Writes header then streams the body. Keys are immutable: an existing key is
    /// `AlreadyExists`. A failing body stream aborts the write so no partial object remains.
    pub async fn put(
        &self,
        key: &str,
        header: &Header,
        body: impl Stream<Item = Result<Bytes, std::io::Error>> + Send,
    ) -> Result<PutOutcome, StoreError> {
        let conditional = self.op.info().capability().write_with_if_not_exists;
        if !conditional {
            match self.op.stat(key).await {
                Ok(_) => return Ok(PutOutcome::AlreadyExists),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let mut writer = match self.op.writer_with(key).if_not_exists(conditional).await {
            Ok(w) => w,
            Err(e) if conditional && is_condition_failed(&e) => {
                return Ok(PutOutcome::AlreadyExists);
            }
            Err(e) => return Err(e.into()),
        };
        let streamed: Result<(), StoreError> = async {
            writer.write(Bytes::from(encode_header(header))).await?;
            let mut body = std::pin::pin!(body);
            while let Some(chunk) = body.next().await {
                writer.write(chunk?).await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = streamed {
            if conditional && matches!(&e, StoreError::Storage(o) if is_condition_failed(o)) {
                let _ = writer.abort().await;
                return Ok(PutOutcome::AlreadyExists);
            }
            let _ = writer.abort().await;
            return Err(e);
        }
        match writer.close().await {
            Ok(_) => Ok(PutOutcome::Created),
            Err(e) if conditional && is_condition_failed(&e) => Ok(PutOutcome::AlreadyExists),
            Err(e) => Err(e.into()),
        }
    }

    /// Recursive listing of files under `prefix`; keys are relative to the operator root.
    pub async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
        let entries = match self.op.list_with(prefix).recursive(true).await {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            if entry.metadata().is_dir() {
                continue;
            }
            let key = entry.path().to_string();
            let mut meta = entry.metadata().clone();
            if meta.last_modified().is_none() || !meta.is_file() {
                meta = self.op.stat(&key).await?;
            }
            out.push(ObjectInfo {
                key,
                size: meta.content_length(),
                last_modified: meta.last_modified().map(to_utc).unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Deletes `key`; deleting a missing key succeeds.
    pub async fn delete(&self, key: &str) -> Result<(), StoreError> {
        Ok(self.op.delete(key).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, LifecycleMode, Mode, Storage};
    use futures::stream;

    fn header(size: u64) -> Header {
        Header {
            v: 1,
            duration_ms: Some(7),
            tag: Some("t".into()),
            size,
        }
    }

    fn body(b: &[u8]) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + use<> {
        stream::iter(vec![Ok(Bytes::copy_from_slice(b))])
    }

    async fn read_from(reader: opendal::Reader, off: u64) -> Vec<u8> {
        reader.read(off..).await.unwrap().to_vec()
    }

    async fn body_of(s: &Store, key: &str) -> Vec<u8> {
        match s.get(key).await.unwrap() {
            GetOutcome::Hit(_, r, off) => read_from(r, off).await,
            other => panic!("{other:?}"),
        }
    }

    fn fs_config(root: &std::path::Path) -> Config {
        Config {
            storage: Storage::Fs,
            mode: Mode::ReadWrite,
            cache_version: "v1".into(),
            port: 0,
            port_file: root.join("port"),
            token_file: root.join("token"),
            token: None,
            summary_file: None,
            fs_root: root.join("store"),
            r2: None,
            r2_lifecycle: LifecycleMode::Auto,
            max_age_days: 7,
            max_size: 1 << 30,
        }
    }

    async fn fs_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let op = build_operator(&fs_config(dir.path())).unwrap();
        (Store::new(op), dir)
    }

    pub(crate) async fn check_put_get_roundtrip(s: &Store) {
        let h = header(10);
        assert_eq!(
            s.put("cache/turbo/v1/aa", &h, body(b"0123456789"))
                .await
                .unwrap(),
            PutOutcome::Created
        );
        match s.get("cache/turbo/v1/aa").await.unwrap() {
            GetOutcome::Hit(back, reader, off) => {
                assert_eq!(back, h);
                assert_eq!(read_from(reader, off).await, b"0123456789");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(s.head("cache/turbo/v1/aa").await.unwrap(), Some(h));
    }

    pub(crate) async fn check_put_existing_keeps_original(s: &Store) {
        s.put("k/aa", &header(1), body(b"A")).await.unwrap();
        assert_eq!(
            s.put("k/aa", &header(1), body(b"B")).await.unwrap(),
            PutOutcome::AlreadyExists
        );
        assert_eq!(body_of(s, "k/aa").await, b"A");
    }

    pub(crate) async fn check_missing_and_corrupt(s: &Store) {
        assert!(matches!(s.get("k/none").await.unwrap(), GetOutcome::Miss));
        assert_eq!(s.head("k/none").await.unwrap(), None);
        s.op.write("k/bad", b"not an envelope".to_vec())
            .await
            .unwrap();
        assert!(matches!(s.get("k/bad").await.unwrap(), GetOutcome::Corrupt));
        assert_eq!(s.head("k/bad").await.unwrap(), None);
        s.op.write("k/tiny", b"HR".to_vec()).await.unwrap();
        assert!(matches!(
            s.get("k/tiny").await.unwrap(),
            GetOutcome::Corrupt
        ));
    }

    pub(crate) async fn check_head_does_not_read_body(s: &Store) {
        let n = 5 * 1024 * 1024;
        let chunks = (0..5).map(|_| Ok(Bytes::from(vec![7u8; 1024 * 1024])));
        let h = header(n);
        s.put("big/aa", &h, stream::iter(chunks)).await.unwrap();
        assert_eq!(s.head("big/aa").await.unwrap(), Some(h));
    }

    pub(crate) async fn check_list_and_delete(s: &Store) {
        for (k, b) in [("p/a", &b"1"[..]), ("p/b", b"22"), ("p/sub/c", b"333")] {
            s.put(k, &header(b.len() as u64), body(b)).await.unwrap();
        }
        s.put("other/x", &header(1), body(b"x")).await.unwrap();
        let mut all = s.list("p/").await.unwrap();
        all.sort_by(|a, b| a.key.cmp(&b.key));
        let keys: Vec<_> = all.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, ["p/a", "p/b", "p/sub/c"]);
        let expect =
            |b: usize| (crate::envelope::encode_header(&header(b as u64)).len() + b) as u64;
        assert_eq!(all[0].size, expect(1));
        assert_eq!(all[2].size, expect(3));
        assert!(all[0].last_modified.timestamp() > 1_600_000_000);
        s.delete("p/b").await.unwrap();
        assert_eq!(s.list("p/").await.unwrap().len(), 2);
        s.delete("p/b").await.unwrap(); // idempotent
    }

    pub(crate) async fn check_failed_body_leaves_no_object(s: &Store) {
        let bad = stream::iter(vec![
            Ok(Bytes::from_static(b"abc")),
            Err(std::io::Error::other("boom")),
        ]);
        assert!(s.put("f/aa", &header(6), bad).await.is_err());
        assert!(matches!(s.get("f/aa").await.unwrap(), GetOutcome::Miss));
        assert!(s.list("f/").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fs_put_get_roundtrip() {
        let (s, _d) = fs_store().await;
        check_put_get_roundtrip(&s).await;
    }
    #[tokio::test]
    async fn fs_put_existing_is_already_exists_and_keeps_original() {
        let (s, _d) = fs_store().await;
        check_put_existing_keeps_original(&s).await;
    }
    #[tokio::test]
    async fn fs_missing_and_corrupt() {
        let (s, _d) = fs_store().await;
        check_missing_and_corrupt(&s).await;
    }
    #[tokio::test]
    async fn fs_head_does_not_read_body() {
        let (s, _d) = fs_store().await;
        check_head_does_not_read_body(&s).await;
    }
    #[tokio::test]
    async fn fs_list_and_delete() {
        let (s, _d) = fs_store().await;
        check_list_and_delete(&s).await;
    }
    #[tokio::test]
    async fn fs_failed_body_leaves_no_object() {
        let (s, _d) = fs_store().await;
        check_failed_body_leaves_no_object(&s).await;
    }
    #[test]
    fn unsupported_backends_fail_clearly() {
        let d = tempfile::tempdir().unwrap();
        let mut c = fs_config(d.path());
        c.storage = Storage::R2;
        assert!(
            build_operator(&c)
                .unwrap_err()
                .to_string()
                .contains("not yet supported")
        );
        c.storage = Storage::Github;
        assert!(
            build_operator(&c)
                .unwrap_err()
                .to_string()
                .contains("not yet supported")
        );
    }
}
