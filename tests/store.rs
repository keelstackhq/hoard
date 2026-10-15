mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::failing_layer::FailingWrites;
use common::{fs_config, layered_fs_store, store_checks as c};
use hoard::config::Storage;
use hoard::store::{PutOutcome, Store, StoreError, build_operator};
use opendal::ErrorKind;

fn fs_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let op = build_operator(&fs_config(dir.path())).unwrap();
    (Store::new(op), dir)
}

#[tokio::test]
async fn fs_put_get_roundtrip() {
    let (s, _d) = fs_store();
    c::check_put_get_roundtrip(&s).await;
}
#[tokio::test]
async fn fs_put_existing_keeps_original() {
    let (s, _d) = fs_store();
    c::check_put_existing_keeps_original(&s).await;
}
#[tokio::test]
async fn fs_missing_and_corrupt() {
    let (s, _d) = fs_store();
    c::check_missing_and_corrupt(&s).await;
}
#[tokio::test]
async fn fs_head_does_not_read_body() {
    let (s, _d) = fs_store();
    c::check_head_does_not_read_body(&s).await;
}
#[tokio::test]
async fn fs_list_and_delete() {
    let (s, _d) = fs_store();
    c::check_list_and_delete(&s).await;
}
#[tokio::test]
async fn fs_failed_body_leaves_no_object() {
    let (s, _d) = fs_store();
    c::check_failed_body_leaves_no_object(&s).await;
}

#[test]
fn r2_without_settings_fails_clearly() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = fs_config(d.path());
    cfg.storage = Storage::R2;
    let err = build_operator(&cfg).unwrap_err().to_string();
    assert!(err.contains("r2-* settings"), "{err}");
}

fn header(size: u64) -> hoard::envelope::Header {
    hoard::envelope::Header {
        v: 1,
        duration_ms: None,
        tag: None,
        size,
    }
}

fn body(b: &'static [u8]) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    futures::stream::iter([Ok(bytes::Bytes::from_static(b))])
}

/// GitHub cache keys are immutable and a second create fails with "already exists" (409);
/// on a backend without conditional writes that is still `AlreadyExists`, not an error.
#[tokio::test]
async fn unconditional_backend_already_exists_is_already_exists() {
    for kind in [ErrorKind::AlreadyExists, ErrorKind::ConditionNotMatch] {
        let mut layer = FailingWrites::new(kind);
        layer.unconditional = true;
        let calls = layer.calls.clone();
        let (s, _d) = layered_fs_store(layer);
        assert!(!s.op.info().capability().write_with_if_not_exists);
        let out = s.put("cache/k", &header(1), body(b"x")).await.unwrap();
        assert_eq!(out, PutOutcome::AlreadyExists, "{kind:?}");
        // Stat first (nothing there), then the write that the backend rejects.
        assert_eq!(calls.stat.load(Ordering::SeqCst), 1);
        assert_eq!(calls.write.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn rate_limited_write_is_rate_limited_with_retry_after() {
    let mut layer = FailingWrites::new(ErrorKind::RateLimited);
    layer.response = Some(r#"Parts { status: 429, headers: {"retry-after": "7"} }"#.into());
    let (s, _d) = layered_fs_store(layer);
    match s.put("cache/k", &header(1), body(b"x")).await {
        Err(StoreError::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(Duration::from_secs(7)))
        }
        other => panic!("{other:?}"),
    }
    let (s, _d) = layered_fs_store(FailingWrites::new(ErrorKind::RateLimited));
    assert!(matches!(
        s.put("cache/k", &header(1), body(b"x")).await,
        Err(StoreError::RateLimited { retry_after: None })
    ));
}

#[tokio::test]
async fn fs_list_hides_scratch_files() {
    let (s, _d) = fs_store();
    c::check_list_hides_scratch_files(&s).await;
}

#[tokio::test]
async fn fs_large_body_in_small_chunks() {
    let (s, _d) = fs_store();
    c::check_large_body_in_small_chunks(&s).await;
}
#[tokio::test]
async fn fs_put_existing_large_keeps_original() {
    let (s, _d) = fs_store();
    c::check_put_existing_large_keeps_original(&s).await;
}
#[tokio::test]
async fn fs_failed_large_body_leaves_no_object() {
    let (s, _d) = fs_store();
    c::check_failed_large_body_leaves_no_object(&s).await;
}
