mod common;

use common::{fs_config, store_checks as c};
use hoard::config::Storage;
use hoard::store::{Store, build_operator};

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
fn unsupported_backends_fail_clearly() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = fs_config(d.path());
    cfg.storage = Storage::R2;
    assert!(
        build_operator(&cfg)
            .unwrap_err()
            .to_string()
            .contains("not yet supported")
    );
    cfg.storage = Storage::Github;
    assert!(
        build_operator(&cfg)
            .unwrap_err()
            .to_string()
            .contains("not yet supported")
    );
}
