//! Store checks on the R2 backend (S3 API), run against MinIO when `HOARD_TEST_S3_ENDPOINT`
//! is set. The store is built through `build_operator`, the same path the server uses.

mod common;

use common::{MinioBucket, minio_bucket, r2_config, s3_runtime as rt, store_checks as c};
use hoard::store::{Store, build_operator};

struct S3Store {
    store: Store,
    _bucket: MinioBucket,
    _dir: tempfile::TempDir,
}

async fn s3_store() -> Option<S3Store> {
    let bucket = minio_bucket().await?;
    let dir = tempfile::tempdir().unwrap();
    let op = build_operator(&r2_config(dir.path(), &bucket)).unwrap();
    Some(S3Store {
        store: Store::new(op),
        _bucket: bucket,
        _dir: dir,
    })
}

macro_rules! s3_check {
    ($name:ident, $check:path) => {
        #[test]
        fn $name() {
            rt().block_on(async {
                let Some(s) = s3_store().await else { return };
                $check(&s.store).await;
            })
        }
    };
}

s3_check!(s3_put_get_roundtrip, c::check_put_get_roundtrip);
s3_check!(
    s3_put_existing_keeps_original,
    c::check_put_existing_keeps_original
);
s3_check!(
    s3_put_existing_large_keeps_original,
    c::check_put_existing_large_keeps_original
);
s3_check!(s3_missing_and_corrupt, c::check_missing_and_corrupt);
s3_check!(s3_head_does_not_read_body, c::check_head_does_not_read_body);
s3_check!(s3_list_and_delete, c::check_list_and_delete);
s3_check!(
    s3_failed_body_leaves_no_object,
    c::check_failed_body_leaves_no_object
);
s3_check!(
    s3_failed_large_body_leaves_no_object,
    c::check_failed_large_body_leaves_no_object
);
s3_check!(
    s3_large_body_in_small_chunks,
    c::check_large_body_in_small_chunks
);
s3_check!(
    s3_list_keeps_hoard_tmp_prefix,
    c::check_list_keeps_hoard_tmp_prefix
);

/// `put` takes the conditional-write path on S3 (no stat-then-write race), and MinIO
/// rejects a second if-not-exists write instead of overwriting.
#[test]
fn s3_reports_and_honours_conditional_write() {
    rt().block_on(async {
        let Some(s) = s3_store().await else { return };
        let op = &s.store.op;
        assert!(op.info().capability().write_with_if_not_exists);
        op.write_with("c/aa", b"one".to_vec())
            .if_not_exists(true)
            .await
            .unwrap();
        let err = op
            .write_with("c/aa", b"two".to_vec())
            .if_not_exists(true)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), opendal::ErrorKind::ConditionNotMatch, "{err}");
        assert_eq!(op.read("c/aa").await.unwrap().to_vec(), b"one");
    })
}

/// Runs `check`, then asserts the bucket holds no unfinished multipart upload.
async fn assert_no_pending_uploads(s: &S3Store) {
    let b = &s._bucket;
    let out = common::s3_client(&b.endpoint, &b.key_id, &b.secret)
        .list_multipart_uploads()
        .bucket(&b.bucket)
        .send()
        .await
        .unwrap();
    assert!(
        out.uploads().is_empty(),
        "pending multipart uploads: {:?}",
        out.uploads()
    );
}

/// A failed multi-part body aborts the upload; no parts stay behind.
#[test]
fn s3_failed_large_body_aborts_multipart() {
    rt().block_on(async {
        let Some(s) = s3_store().await else { return };
        c::check_failed_large_body_leaves_no_object(&s.store).await;
        assert_no_pending_uploads(&s).await;
    })
}

/// A multi-part write rejected because the key exists aborts the upload.
#[test]
fn s3_rejected_large_put_aborts_multipart() {
    rt().block_on(async {
        let Some(s) = s3_store().await else { return };
        c::check_put_existing_large_keeps_original(&s.store).await;
        assert_no_pending_uploads(&s).await;
    })
}
