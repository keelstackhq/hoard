//! The shared GitHub cache Twirp client against the local mock: upload, exact-key lookup,
//! Range reads and error redaction.

mod common;

use common::gha_mock::{self, GhaMock};
use futures::TryStreamExt;
use hoard::gha_twirp::{GhaError, GhaTwirp, pack_version, vp_version};

const TOKEN: &str = "tok-gha-twirp";
const KEY: &str = "pack/one";

async fn collect(
    s: impl futures::Stream<Item = std::io::Result<bytes::Bytes>>,
) -> std::io::Result<Vec<u8>> {
    let chunks: Vec<_> = s.try_collect().await?;
    Ok(chunks.concat())
}

/// A mock with `data` uploaded as `KEY`; returns the client and the signed download URL.
async fn uploaded(data: &[u8]) -> (GhaMock, GhaTwirp, String) {
    let mock = gha_mock::spawn(TOKEN).await;
    let client = GhaTwirp::new(&mock.url, TOKEN).unwrap();
    let version = pack_version("v1");
    let url = client
        .create_entry(KEY, &version)
        .await
        .unwrap()
        .expect("new key");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob");
    std::fs::write(&path, data).unwrap();
    let n = client.upload_blocks(&url, &path).await.unwrap();
    assert_eq!(n, data.len() as u64);
    client.finalize(KEY, &version, n).await.unwrap();
    let (matched, dl) = client
        .get_download_url(KEY, &[], &version)
        .await
        .unwrap()
        .expect("finalized entry");
    assert_eq!(matched, KEY);
    (mock, client, dl)
}

#[tokio::test]
async fn range_read_yields_exactly_the_slice() {
    let (mock, client, url) = uploaded(b"0123456789abcdef").await;
    let body = client.download_range(&url, 3, 5).await.unwrap();
    assert_eq!(collect(body).await.unwrap(), b"34567");
    let get = mock
        .requests()
        .into_iter()
        .rfind(|r| r.method == "GET")
        .unwrap();
    assert_eq!(get.range.as_deref(), Some("bytes=3-7"));
    assert_eq!(get.x_ms_version.as_deref(), Some("2025-01-05"));
    assert_eq!(get.authorization, None);
}

#[tokio::test]
async fn range_past_the_end_is_a_status_error() {
    let (_mock, client, url) = uploaded(b"0123456789").await;
    let err = client.download_range(&url, 50, 5).await.err().unwrap();
    assert!(matches!(err, GhaError::Status { status: 416, .. }), "{err}");
}

#[tokio::test]
async fn zero_length_is_rejected_without_a_request() {
    let (mock, client, url) = uploaded(b"0123456789").await;
    let before = mock.requests().len();
    let err = client.download_range(&url, 2, 0).await.err().unwrap();
    assert!(matches!(err, GhaError::Protocol { .. }), "{err}");
    assert_eq!(mock.requests().len(), before);
}

#[tokio::test]
async fn a_200_means_range_was_ignored() {
    let (mock, client, url) = uploaded(b"0123456789").await;
    mock.ignore_range(true);
    let err = client.download_range(&url, 2, 3).await.err().unwrap();
    assert!(matches!(err, GhaError::Protocol { .. }), "{err}");
}

#[tokio::test]
async fn wrong_sig_is_403_and_the_error_has_no_url() {
    let (_mock, client, url) = uploaded(b"0123456789").await;
    let bad = url.replace(gha_mock::MOCK_SIG, "wrong");
    let err = client.download_range(&bad, 3, 5).await.err().unwrap();
    assert!(matches!(err, GhaError::Status { status: 403, .. }), "{err}");
    let text = format!("{err} {err:?}");
    assert!(
        !text.contains("MOCKSIG") && !text.contains("sig="),
        "{text}"
    );
}

#[tokio::test]
async fn throttling_statuses_map_to_rate_limited() {
    let (mock, client, url) = uploaded(b"0123456789").await;
    for status in [429, 503] {
        mock.fail_next_blob_get(status, 1);
        let err = client.download_range(&url, 0, 2).await.err().unwrap();
        assert!(matches!(err, GhaError::RateLimited(_)), "{status}: {err}");
    }
    // One-shot: the next read succeeds.
    let body = client.download_range(&url, 0, 2).await.unwrap();
    assert_eq!(collect(body).await.unwrap(), b"01");
}

#[tokio::test]
async fn fail_next_twirp_is_one_shot_per_call() {
    let mock = gha_mock::spawn(TOKEN).await;
    let client = GhaTwirp::new(&mock.url, TOKEN).unwrap();
    mock.fail_next_twirp(403, 2);
    for _ in 0..2 {
        let err = client.create_entry("k", "v").await.unwrap_err();
        assert!(matches!(err, GhaError::Status { status: 403, .. }), "{err}");
    }
    assert!(client.create_entry("k", "v").await.unwrap().is_some());
}

#[test]
fn pack_version_is_a_distinct_sha256_hex() {
    let v = pack_version("v1");
    assert_eq!(v.len(), 64);
    assert!(v.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_ne!(v, vp_version("v1"));
    assert_ne!(v, pack_version("v2"));
}
