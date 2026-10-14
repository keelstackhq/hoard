//! Backend-agnostic Store checks; each takes a `&Store` so fs and R2 tests share them.

use bytes::Bytes;
use futures::{Stream, stream};
use hoard::envelope::{Header, encode_header};
use hoard::store::{GetOutcome, PutOutcome, Store};

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

pub async fn check_put_get_roundtrip(s: &Store) {
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

pub async fn check_put_existing_keeps_original(s: &Store) {
    s.put("k/aa", &header(1), body(b"A")).await.unwrap();
    assert_eq!(
        s.put("k/aa", &header(1), body(b"B")).await.unwrap(),
        PutOutcome::AlreadyExists
    );
    assert_eq!(body_of(s, "k/aa").await, b"A");
}

pub async fn check_missing_and_corrupt(s: &Store) {
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

pub async fn check_head_does_not_read_body(s: &Store) {
    let n = 5 * 1024 * 1024;
    let chunks = (0..5).map(|_| Ok(Bytes::from(vec![7u8; 1024 * 1024])));
    let h = header(n);
    s.put("big/aa", &h, stream::iter(chunks)).await.unwrap();
    assert_eq!(s.head("big/aa").await.unwrap(), Some(h));
}

pub async fn check_list_and_delete(s: &Store) {
    for (k, b) in [("p/a", &b"1"[..]), ("p/b", b"22"), ("p/sub/c", b"333")] {
        s.put(k, &header(b.len() as u64), body(b)).await.unwrap();
    }
    s.put("other/x", &header(1), body(b"x")).await.unwrap();
    let mut all = s.list("p/").await.unwrap();
    all.sort_by(|a, b| a.key.cmp(&b.key));
    let keys: Vec<_> = all.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(keys, ["p/a", "p/b", "p/sub/c"]);
    let expect = |b: usize| (encode_header(&header(b as u64)).len() + b) as u64;
    assert_eq!(all[0].size, expect(1));
    assert_eq!(all[2].size, expect(3));
    assert!(all[0].last_modified.timestamp() > 1_600_000_000);
    assert!(
        s.list("")
            .await
            .unwrap()
            .iter()
            .all(|o| !o.key.contains(".hoard-tmp")),
        "scratch files must not be listed"
    );
    s.delete("p/b").await.unwrap();
    assert_eq!(s.list("p/").await.unwrap().len(), 2);
    s.delete("p/b").await.unwrap(); // idempotent
}

pub async fn check_failed_body_leaves_no_object(s: &Store) {
    let bad = stream::iter(vec![
        Ok(Bytes::from_static(b"abc")),
        Err(std::io::Error::other("boom")),
    ]);
    assert!(s.put("f/aa", &header(6), bad).await.is_err());
    assert!(matches!(s.get("f/aa").await.unwrap(), GetOutcome::Miss));
    assert!(s.list("f/").await.unwrap().is_empty());
    // No scratch files from the aborted write are visible anywhere.
    assert!(s.list("").await.unwrap().is_empty());
}
