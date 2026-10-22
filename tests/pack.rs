//! Pack names, the pack backend (fs and the GitHub cache mock) and the read path.

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::gha_mock::{self, GhaMock, MOCK_SIG};
use common::pack::{build_pack, fs_backend, names, put_bytes, put_index, put_pack};
use futures::TryStreamExt;
use hoard::envelope::Header;
use hoard::gha_twirp::GhaTwirp;
use hoard::pack::backend::{PackBackend, PackError};
use hoard::pack::index::{Loc, PackIndex};
use hoard::pack::names::ref_segment;
use hoard::pack::reader::{PackGet, PackReader};
use hoard::store::BodyStream;

const TOKEN: &str = "tok-pack";
const WAIT: Duration = Duration::from_secs(5);

async fn collect(s: BodyStream) -> std::io::Result<Vec<u8>> {
    let chunks: Vec<Bytes> = s.try_collect().await?;
    Ok(chunks.concat())
}

async fn hit(r: &PackReader, hash: &str) -> (Header, Vec<u8>) {
    match r.get(hash).await {
        PackGet::Hit(h, body) => (h, collect(body).await.unwrap()),
        other => panic!("{hash}: {other:?}"),
    }
}

async fn gha() -> (GhaMock, Arc<PackBackend>) {
    let mock = gha_mock::spawn(TOKEN).await;
    let backend = PackBackend::github(GhaTwirp::new(&mock.url, TOKEN).unwrap(), "v1");
    (mock, Arc::new(backend))
}

fn twirp_calls(mock: &GhaMock, method: &str) -> Vec<serde_json::Value> {
    mock.requests()
        .into_iter()
        .filter(|r| r.path.ends_with(&format!("/{method}")))
        .filter_map(|r| r.json)
        .collect()
}

fn lookups_of(mock: &GhaMock, key: &str) -> usize {
    twirp_calls(mock, "GetCacheEntryDownloadURL")
        .iter()
        .filter(|j| j["key"] == key)
        .count()
}

fn assert_no_leak(text: &str) {
    assert!(!text.contains(MOCK_SIG), "{text}");
    assert!(!text.contains("sig="), "{text}");
    assert!(!text.contains("http://"), "{text}");
}

// ---- names ----

#[test]
fn ref_segment_sanitises() {
    assert_eq!(ref_segment("feature/x y"), "feature_x_y");
    assert_eq!(ref_segment("v1.2-rc_3"), "v1.2-rc_3");
    assert_eq!(ref_segment(""), "_");
    assert_eq!(ref_segment(".."), "_");
    assert_eq!(ref_segment("."), "_");
    assert_eq!(ref_segment("ä"), "_");
    assert_eq!(ref_segment(&"a".repeat(300)).len(), 100);
}

#[test]
fn names_ref_precedence_and_keys() {
    let n = names(
        "p/",
        &[
            ("GITHUB_HEAD_REF", "feat/x"),
            ("GITHUB_REF_NAME", "123/merge"),
            ("GITHUB_BASE_REF", "main"),
            ("GITHUB_RUN_ID", "55"),
            ("GITHUB_RUN_ATTEMPT", "2"),
        ],
    );
    assert_eq!(n.ref_, "feat_x");
    assert_eq!(n.base.as_deref(), Some("main"));
    assert!(n.run_key.starts_with("55-2-"), "{}", n.run_key);
    assert_eq!(n.pack_key(3), format!("p/turbo-pack/v1/{}-3", n.run_key));
    assert_eq!(
        n.index_key(2),
        format!("p/turbo-index/v1/feat_x/{}-2", n.run_key)
    );
    let (r, b, any) = n.index_prefixes();
    assert_eq!(r, "p/turbo-index/v1/feat_x/");
    assert_eq!(b.as_deref(), Some("p/turbo-index/v1/main/"));
    assert_eq!(any, "p/turbo-index/v1/");

    let n = names("", &[("GITHUB_REF_NAME", "main"), ("GITHUB_HEAD_REF", "")]);
    assert_eq!(n.ref_, "main");
    assert_eq!(n.base, None);
    let n = names("", &[]);
    assert_eq!(n.ref_, "local");
    assert!(n.run_key.starts_with("local-"));
    let (r, b, any) = n.index_prefixes();
    assert!(r.ends_with('/') && any.ends_with('/') && b.is_none());
}

// ---- fs ----

#[tokio::test]
async fn fs_reader_finds_hash_and_streams_exact_body() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _op) = fs_backend(dir.path());
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"first body"), ("bb", &[7u8; 5000]), ("cc", b"")]);
    let idx = put_pack(&backend, &n.pack_key(1), &built, 1000).await;
    put_index(&backend, &n.index_key(1), &idx).await;

    let r = PackReader::start(backend, n.clone(), true, WAIT);
    let e = r.lookup("bb").await.unwrap();
    assert_eq!(&*e.pack, n.pack_key(1));
    assert_eq!(e.loc.size, 5000);
    assert!(r.lookup("dd").await.is_none());

    let (h, body) = hit(&r, "aa").await;
    assert_eq!(body, b"first body");
    assert_eq!(h.size, 10);
    assert_eq!(h.hash.as_deref(), Some("aa"));
    assert_eq!(h.tag.as_deref(), Some("tag-aa"));
    assert_eq!(h.duration_ms, Some(7));
    assert_eq!(hit(&r, "bb").await.1, vec![7u8; 5000]);
    assert_eq!(hit(&r, "cc").await.1, b"");
    assert!(matches!(r.get("dd").await, PackGet::Miss));
    assert!(r.missing().is_empty() && r.corrupt().is_empty());

    assert_eq!(r.loaded().await.len(), 3);
}

#[tokio::test]
async fn hit_touches_the_pack() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _op) = fs_backend(dir.path());
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"x")]);
    let idx = put_pack(&backend, &n.pack_key(1), &built, 1000).await;
    put_index(&backend, &n.index_key(1), &idx).await;
    let r = PackReader::start(backend, n, true, WAIT);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    // Before the hit the pack was last used at 1000 ms: a 7-day cap at `now` drops it.
    let mut before = r.loaded().await;
    before.cap(now, Duration::from_secs(7 * 24 * 3600), 10, 10);
    assert_eq!(before.len(), 0);
    hit(&r, "aa").await;
    let mut after = r.loaded().await;
    after.cap(now, Duration::from_secs(7 * 24 * 3600), 10, 10);
    assert_eq!(after.len(), 1);
}

#[tokio::test]
async fn reader_without_load_has_an_empty_index() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _op) = fs_backend(dir.path());
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"x")]);
    let idx = put_pack(&backend, &n.pack_key(1), &built, 1000).await;
    put_index(&backend, &n.index_key(1), &idx).await;
    let r = PackReader::start(backend, n, false, WAIT);
    assert!(r.lookup("aa").await.is_none());
    assert!(matches!(r.get("aa").await, PackGet::Miss));
    assert!(r.loaded().await.is_empty());
}

#[tokio::test]
async fn newest_picks_the_newer_index_and_bounds_the_size() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _op) = fs_backend(dir.path());
    let n = names("", &[]);
    let (prefix, _, any) = n.index_prefixes();
    let mut a = PackIndex::default();
    a.add_pack("p-1", 1, build_pack(&[("aa", b"1")]).entries);
    let mut b = PackIndex::default();
    b.add_pack("p-2", 2, build_pack(&[("bb", b"2")]).entries);
    assert!(backend.newest(&prefix, 1 << 20).await.unwrap().is_none());
    put_index(&backend, &format!("{prefix}run-1"), &a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    put_index(&backend, &format!("{prefix}run-2"), &b).await;

    let (key, bytes) = backend.newest(&prefix, 1 << 20).await.unwrap().unwrap();
    assert_eq!(key, format!("{prefix}run-2"));
    assert_eq!(bytes, b.encode());
    // The "any ref" prefix lists one level deeper.
    let (key, _) = backend.newest(&any, 1 << 20).await.unwrap().unwrap();
    assert_eq!(key, format!("{prefix}run-2"));
    // Larger than max_bytes is an error, not a truncated read.
    assert!(matches!(
        backend.newest(&prefix, 3).await,
        Err(PackError::Other(_))
    ));
}

#[tokio::test]
async fn ref_and_base_are_merged_and_any_is_only_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _op) = fs_backend(dir.path());
    let env = [("GITHUB_HEAD_REF", "feat"), ("GITHUB_BASE_REF", "main")];
    let n = names("", &env);
    let built = build_pack(&[("aa", b"A"), ("bb", b"B"), ("cc", b"C")]);
    let pack = n.pack_key(1);
    put_bytes(&backend, &pack, &built.bytes).await.unwrap();
    let only = |h: &str| {
        let mut i = PackIndex::default();
        let e: Vec<_> = built
            .entries
            .iter()
            .filter(|(k, _)| k == h)
            .cloned()
            .collect();
        i.add_pack(&pack, 1000, e);
        i
    };
    put_index(&backend, "turbo-index/v1/feat/r-1", &only("aa")).await;
    put_index(&backend, "turbo-index/v1/main/r-1", &only("bb")).await;
    put_index(&backend, "turbo-index/v1/other/r-1", &only("cc")).await;

    let idx = PackReader::load_newest(&backend, &n).await;
    assert!(idx.contains("aa") && idx.contains("bb") && !idx.contains("cc"));
    assert_eq!(idx.len(), 2);

    // Only a base index: used with the (absent) ref index.
    let n2 = names(
        "",
        &[("GITHUB_HEAD_REF", "new"), ("GITHUB_BASE_REF", "main")],
    );
    let idx = PackReader::load_newest(&backend, &n2).await;
    assert!(idx.contains("bb") && !idx.contains("aa") && !idx.contains("cc"));

    // Neither: the newest of any ref.
    let n3 = names("", &[("GITHUB_HEAD_REF", "new")]);
    let idx = PackReader::load_newest(&backend, &n3).await;
    assert_eq!(idx.len(), 1, "exactly the newest index of any ref");
    assert!(idx.contains("cc"), "other/ was written last");
}

#[tokio::test]
async fn undecodable_index_is_treated_as_absent() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _op) = fs_backend(dir.path());
    let n = names("", &[("GITHUB_HEAD_REF", "feat")]);
    put_bytes(&backend, "turbo-index/v1/feat/r-1", b"not zstd")
        .await
        .unwrap();
    let built = build_pack(&[("aa", b"A")]);
    let mut i = PackIndex::default();
    i.add_pack("p", 1, built.entries.clone());
    put_index(&backend, "turbo-index/v1/main/r-1", &i).await;
    // The ref index is garbage: ignored, so the fallback to any ref finds main's.
    let idx = PackReader::load_newest(&backend, &n).await;
    assert!(idx.contains("aa"));
}

#[tokio::test]
async fn wrong_envelope_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _op) = fs_backend(dir.path());
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"first"), ("bb", b"second body")]);
    let loc = |h: &str| {
        built
            .entries
            .iter()
            .find(|(k, _)| k == h)
            .unwrap()
            .1
            .clone()
    };
    put_bytes(&backend, &n.pack_key(1), &built.bytes)
        .await
        .unwrap();
    let mut idx = PackIndex::default();
    idx.add_pack(
        &n.pack_key(1),
        1000,
        vec![
            // "aa" points at bb's envelope: header.hash differs.
            ("aa".into(), loc("bb")),
            // right offset, but the index claims another size.
            (
                "bb".into(),
                Loc {
                    size: 3,
                    ..loc("bb")
                },
            ),
            // right size, but len does not add up.
            (
                "cc".into(),
                Loc {
                    offset: loc("aa").offset,
                    len: loc("aa").len + 2,
                    size: loc("aa").size,
                    duration_ms: None,
                    tag: None,
                },
            ),
            // a good one
            ("dd".into(), loc("bb")),
        ],
    );
    put_index(&backend, &n.index_key(1), &idx).await;
    let r = PackReader::start(backend, n, true, WAIT);

    for h in ["aa", "bb", "cc"] {
        assert!(matches!(r.get(h).await, PackGet::Corrupt), "{h}");
        assert!(r.corrupt().contains(h));
        assert!(r.lookup(h).await.is_none(), "{h} is a miss after corrupt");
        assert!(matches!(r.get(h).await, PackGet::Miss), "{h}");
    }
    assert!(r.missing().is_empty());
    // dd points at bb's envelope too (hash "bb"), so it is corrupt as well; nothing else is.
    assert!(matches!(r.get("dd").await, PackGet::Corrupt));
    assert_eq!(r.corrupt().len(), 4);
}

#[tokio::test]
async fn offset_into_the_middle_of_an_envelope_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _op) = fs_backend(dir.path());
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"first body here")]);
    put_bytes(&backend, &n.pack_key(1), &built.bytes)
        .await
        .unwrap();
    let l = built.entries[0].1.clone();
    let mut idx = PackIndex::default();
    idx.add_pack(
        &n.pack_key(1),
        1,
        vec![(
            "aa".into(),
            Loc {
                offset: l.offset + 3,
                len: l.len - 3,
                ..l.clone()
            },
        )],
    );
    put_index(&backend, &n.index_key(1), &idx).await;
    let r = PackReader::start(backend, n, true, WAIT);
    assert!(matches!(r.get("aa").await, PackGet::Corrupt));
}

#[tokio::test]
async fn truncated_pack_is_a_miss_or_corrupt_never_a_short_hit() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, op) = fs_backend(dir.path());
    let n = names("", &[]);
    let built = build_pack(&[("aa", &[1u8; 100])]);
    let idx = put_pack(&backend, &n.pack_key(1), &built, 1).await;
    put_index(&backend, &n.index_key(1), &idx).await;
    // The pack lost its tail.
    op.write(
        &n.pack_key(1),
        built.bytes[..built.bytes.len() - 40].to_vec(),
    )
    .await
    .unwrap();
    let r = PackReader::start(backend, n, true, WAIT);
    match r.get("aa").await {
        PackGet::Hit(_, body) => assert!(collect(body).await.is_err(), "short body must error"),
        PackGet::Miss | PackGet::Corrupt => {}
    }
}

#[tokio::test]
async fn deleted_pack_is_missing_once() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, op) = fs_backend(dir.path());
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"A"), ("bb", b"B")]);
    let pack = n.pack_key(1);
    let idx = put_pack(&backend, &pack, &built, 1000).await;
    put_index(&backend, &n.index_key(1), &idx).await;
    op.delete(&pack).await.unwrap();

    let r = PackReader::start(backend.clone(), n, true, WAIT);
    assert!(
        r.lookup("aa").await.is_some(),
        "HEAD answers from the index"
    );
    assert!(matches!(r.get("aa").await, PackGet::Miss));
    assert!(r.missing().contains(&pack));
    assert!(r.corrupt().is_empty());
    assert!(r.lookup("aa").await.is_none());
    assert!(r.lookup("bb").await.is_none());
    // Even when the pack is back, this run does not ask again.
    put_bytes(&backend, &pack, &built.bytes).await.unwrap();
    assert!(matches!(r.get("bb").await, PackGet::Miss));
}

#[tokio::test]
async fn fs_upload_is_if_not_exists() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, op) = fs_backend(dir.path());
    let n = put_bytes(&backend, "turbo-pack/v1/k-1", b"hello")
        .await
        .unwrap();
    assert_eq!(n, 5);
    let second = put_bytes(&backend, "turbo-pack/v1/k-1", b"other").await;
    assert!(matches!(second, Err(PackError::Exists)), "{second:?}");
    assert_eq!(
        op.read("turbo-pack/v1/k-1").await.unwrap().to_vec(),
        b"hello"
    );
    // read_range on a missing pack: not found.
    assert!(
        backend
            .read_range("turbo-pack/v1/zz", 0, 3)
            .await
            .unwrap()
            .is_none()
    );
    let s = backend
        .read_range("turbo-pack/v1/k-1", 1, 3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(collect(s).await.unwrap(), b"ell");
}

// ---- GitHub cache mock ----

#[tokio::test]
async fn gha_round_trip_resolves_the_url_once_per_pack() {
    let (mock, backend) = gha().await;
    let n = names("", &[("GITHUB_HEAD_REF", "feat")]);
    let built = build_pack(&[("aa", b"alpha"), ("bb", &[9u8; 3000])]);
    let pack = n.pack_key(1);
    let idx = put_pack(&backend, &pack, &built, 1000).await;
    put_index(&backend, &n.index_key(1), &idx).await;

    let r = PackReader::start(backend.clone(), n.clone(), true, WAIT);
    assert_eq!(hit(&r, "aa").await.1, b"alpha");
    assert_eq!(hit(&r, "bb").await.1, vec![9u8; 3000]);
    assert_eq!(lookups_of(&mock, &pack), 1, "one URL lookup for two GETs");
    // The index load used key+restore_keys with the same prefix.
    let (prefix, _, _) = n.index_prefixes();
    let idx_calls: Vec<_> = twirp_calls(&mock, "GetCacheEntryDownloadURL")
        .into_iter()
        .filter(|j| j["key"] == prefix.as_str())
        .collect();
    assert_eq!(idx_calls.len(), 1);
    assert_eq!(idx_calls[0]["restore_keys"][0], prefix.as_str());
    // Pack lookups use the exact key, no restore keys, and the pack version.
    let lk = twirp_calls(&mock, "GetCacheEntryDownloadURL")
        .into_iter()
        .find(|j| j["key"] == pack.as_str())
        .unwrap();
    assert!(lk["restore_keys"].as_array().is_none_or(|a| a.is_empty()));
    assert_eq!(lk["version"], hoard::gha_twirp::pack_version("v1").as_str());
    // The reads were Range requests.
    let ranges: Vec<_> = mock
        .requests()
        .into_iter()
        .filter(|q| q.method == "GET" && q.range.is_some())
        .collect();
    assert_eq!(ranges.len(), 2);
}

#[tokio::test]
async fn gha_403_resolves_the_url_once_more() {
    let (mock, backend) = gha().await;
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"alpha")]);
    let pack = n.pack_key(1);
    let idx = put_pack(&backend, &pack, &built, 1000).await;
    put_index(&backend, &n.index_key(1), &idx).await;
    let r = PackReader::start(backend.clone(), n, true, WAIT);
    assert_eq!(hit(&r, "aa").await.1, b"alpha");
    assert_eq!(lookups_of(&mock, &pack), 1);
    mock.fail_next_blob_get(403, 1);
    assert_eq!(hit(&r, "aa").await.1, b"alpha");
    assert_eq!(lookups_of(&mock, &pack), 2);
    assert!(r.missing().is_empty() && r.corrupt().is_empty());

    // Two 403s in a row: one retry only, then a plain miss.
    mock.fail_next_blob_get(403, 2);
    assert!(matches!(r.get("aa").await, PackGet::Miss));
    assert_eq!(lookups_of(&mock, &pack), 3);
    assert!(r.missing().is_empty());
}

#[tokio::test]
async fn gha_429_on_lookup_is_a_miss_but_not_missing() {
    let (mock, backend) = gha().await;
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"alpha")]);
    let pack = n.pack_key(1);
    let idx = put_pack(&backend, &pack, &built, 1000).await;
    put_index(&backend, &n.index_key(1), &idx).await;
    let r = PackReader::start(backend.clone(), n, true, WAIT);
    r.wait_loaded().await;
    mock.fail_next_twirp(429, 1);
    assert!(matches!(r.get("aa").await, PackGet::Miss));
    assert!(r.missing().is_empty() && r.corrupt().is_empty());
    assert!(r.lookup("aa").await.is_some());
    assert_eq!(
        hit(&r, "aa").await.1,
        b"alpha",
        "the next GET retries the lookup"
    );
}

#[tokio::test]
async fn gha_evicted_pack_is_missing() {
    let (_mock, backend) = gha().await;
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"alpha")]);
    let mut idx = PackIndex::default();
    idx.add_pack("turbo-pack/v1/gone-1", 1, built.entries.clone());
    put_index(&backend, &n.index_key(1), &idx).await;
    let r = PackReader::start(backend, n, true, WAIT);
    assert!(matches!(r.get("aa").await, PackGet::Miss));
    assert!(r.missing().contains("turbo-pack/v1/gone-1"));
    assert!(r.lookup("aa").await.is_none());
}

#[tokio::test]
async fn gha_slow_load_leaves_the_index_empty() {
    let (mock, backend) = gha().await;
    let n = names("", &[]);
    let built = build_pack(&[("aa", b"alpha")]);
    let idx = put_pack(&backend, &n.pack_key(1), &built, 1000).await;
    put_index(&backend, &n.index_key(1), &idx).await;
    mock.delay_twirp(Duration::from_millis(400));
    let r = PackReader::start(backend, n, true, Duration::from_millis(60));
    let t = std::time::Instant::now();
    assert!(r.lookup("aa").await.is_none());
    assert!(
        t.elapsed() < Duration::from_millis(350),
        "waits only the deadline"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        r.lookup("aa").await.is_none(),
        "a late load is not installed"
    );
    assert!(r.loaded().await.is_empty());
}

#[tokio::test]
async fn gha_backend_errors_carry_no_url() {
    let (mock, backend) = gha().await;
    let built = build_pack(&[("aa", b"alpha")]);
    put_bytes(&backend, "turbo-pack/v1/k-1", &built.bytes)
        .await
        .unwrap();

    // 500 from the blob.
    mock.fail_next_blob_get(500, 1);
    let Err(PackError::Other(msg)) = backend.read_range("turbo-pack/v1/k-1", 0, 5).await else {
        panic!("expected Other")
    };
    assert!(msg.contains("500"), "{msg}");
    assert_no_leak(&msg);
    // 429 from the blob.
    mock.fail_next_blob_get(429, 1);
    assert!(matches!(
        backend.read_range("turbo-pack/v1/k-1", 0, 5).await,
        Err(PackError::RateLimited { retry_after: None })
    ));
    // A server that ignores Range.
    mock.ignore_range(true);
    let Err(PackError::Other(msg)) = backend.read_range("turbo-pack/v1/k-1", 0, 5).await else {
        panic!("expected Other")
    };
    assert_no_leak(&msg);
    mock.ignore_range(false);
    // Persistent 403 (after the one re-resolve).
    mock.fail_next_blob_get(403, 2);
    let Err(PackError::Other(msg)) = backend.read_range("turbo-pack/v1/k-1", 0, 5).await else {
        panic!("expected Other")
    };
    assert_no_leak(&msg);
    // Service errors.
    mock.fail_with(500);
    let Err(PackError::Other(msg)) = put_bytes(&backend, "turbo-pack/v1/k-2", b"x").await else {
        panic!("expected Other")
    };
    assert_no_leak(&msg);
    let Err(PackError::Other(msg)) = backend.newest("turbo-index/v1/", 1000).await else {
        panic!("expected Other")
    };
    assert_no_leak(&msg);
    mock.fail_with(429);
    assert!(matches!(
        put_bytes(&backend, "turbo-pack/v1/k-3", b"x").await,
        Err(PackError::RateLimited { retry_after: None })
    ));
    assert!(matches!(
        backend.newest("turbo-index/v1/", 1000).await,
        Err(PackError::RateLimited { .. })
    ));
}

#[tokio::test]
async fn gha_upload_exists_and_newest() {
    let (mock, backend) = gha().await;
    put_bytes(&backend, "turbo-pack/v1/k-1", b"one")
        .await
        .unwrap();
    assert!(matches!(
        put_bytes(&backend, "turbo-pack/v1/k-1", b"two").await,
        Err(PackError::Exists)
    ));
    // A reservation without a finalized upload also reads as Exists.
    put_bytes(&backend, "turbo-index/v1/main/a-1", b"first")
        .await
        .unwrap();
    put_bytes(&backend, "turbo-index/v1/main/a-2", b"second")
        .await
        .unwrap();
    put_bytes(&backend, "turbo-index/v1/dev/b-1", b"third")
        .await
        .unwrap();
    let (key, bytes) = backend
        .newest("turbo-index/v1/main/", 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(key, "turbo-index/v1/main/a-2");
    assert_eq!(bytes, b"second");
    assert!(
        backend
            .newest("turbo-index/v1/none/", 100)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        backend.newest("turbo-index/v1/main/", 3).await,
        Err(PackError::Other(_))
    ));
    // Entries are versioned for packs.
    let v = hoard::gha_twirp::pack_version("v1");
    assert!(mock.entries().iter().all(|e| e.version == v));
}
