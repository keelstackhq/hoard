//! Turborepo HTTP API against a real server on 127.0.0.1 (fs store).

mod common;

use std::time::Duration;

use bytes::Bytes;
use common::{TestServer, spawn_test_server};
use hoard::config::Mode;
use hoard::key::artifact_key;
use reqwest::{Client, Method, RequestBuilder, Response};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn req(t: &TestServer, method: Method, path: &str, token: Option<&str>) -> RequestBuilder {
    let r = Client::new().request(method, format!("{}{path}", t.base_url));
    match token {
        Some(tok) => r.bearer_auth(tok),
        None => r,
    }
}

fn authed(t: &TestServer, method: Method, path: &str) -> RequestBuilder {
    req(t, method, path, Some(&t.token))
}

async fn get(t: &TestServer, path: &str, token: Option<&str>) -> Response {
    req(t, Method::GET, path, token).send().await.unwrap()
}

async fn get_noauth(t: &TestServer, path: &str) -> Response {
    get(t, path, None).await
}

async fn get_artifact(t: &TestServer, hash: &str) -> Response {
    authed(t, Method::GET, &format!("/v8/artifacts/{hash}"))
        .send()
        .await
        .unwrap()
}

async fn head(t: &TestServer, hash: &str) -> Response {
    authed(t, Method::HEAD, &format!("/v8/artifacts/{hash}"))
        .send()
        .await
        .unwrap()
}

async fn put(
    t: &TestServer,
    hash: &str,
    body: &'static [u8],
    headers: &[(&str, &str)],
) -> Response {
    let mut r = authed(
        t,
        Method::PUT,
        &format!("/v8/artifacts/{hash}?teamId=t&slug=s"),
    )
    .header("content-type", "application/octet-stream")
    .body(body);
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    r.send().await.unwrap()
}

fn key(hash: &str) -> String {
    artifact_key("cache/", "turbo", "v1", hash)
}

async fn exists(t: &TestServer, hash: &str) -> bool {
    t.store.op.exists(&key(hash)).await.unwrap()
}

#[tokio::test]
async fn rejects_missing_or_wrong_token() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    assert_eq!(get(&t, "/v8/artifacts/status", None).await.status(), 401);
    assert_eq!(
        get(&t, "/v8/artifacts/status", Some("nope")).await.status(),
        401
    );
    assert_eq!(get_noauth(&t, "/_hoard/health").await.status(), 200);
    assert_eq!(
        get(&t, "/v8/artifacts/status", Some(&t.token))
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn auth_variants_and_routes() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    // Token prefix / longer token / wrong scheme are all rejected.
    let short = &t.token[..t.token.len() - 1];
    let long = format!("{}x", t.token);
    assert_eq!(
        get(&t, "/v8/artifacts/status", Some(short)).await.status(),
        401
    );
    assert_eq!(
        get(&t, "/v8/artifacts/status", Some(&long)).await.status(),
        401
    );
    let basic = req(&t, Method::GET, "/v8/artifacts/status", None)
        .header("authorization", format!("Basic {}", t.token))
        .send()
        .await
        .unwrap();
    assert_eq!(basic.status(), 401);
    // Every other route needs auth too, and auth runs before hash validation.
    for (m, p) in [
        (Method::GET, "/v8/artifacts/abc"),
        (Method::HEAD, "/v8/artifacts/abc"),
        (Method::PUT, "/v8/artifacts/abc"),
        (Method::GET, "/v8/artifacts/zz"),
        (Method::POST, "/v8/artifacts"),
        (Method::POST, "/v8/artifacts/events"),
        (Method::GET, "/_hoard/stats"),
        (Method::POST, "/_hoard/shutdown"),
    ] {
        let r = req(&t, m.clone(), p, None).send().await.unwrap();
        assert_eq!(r.status(), 401, "{m} {p}");
    }
    let health = get_noauth(&t, "/_hoard/health").await;
    assert_eq!(health.text().await.unwrap(), "ok");
    let status: Value = get(&t, "/v8/artifacts/status", Some(&t.token))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status, json!({"status": "enabled"}));
}

#[tokio::test]
async fn put_then_get_with_artifact_headers() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    let r = put(
        &t,
        "abc123",
        b"zstd-bytes",
        &[("x-artifact-duration", "1500"), ("x-artifact-tag", "c2ln")],
    )
    .await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap(), json!({"urls": []}));
    let g = get_artifact(&t, "abc123").await;
    assert_eq!(g.status(), 200);
    assert_eq!(g.headers()["x-artifact-duration"], "1500");
    assert_eq!(g.headers()["x-artifact-tag"], "c2ln");
    assert_eq!(g.headers()["content-type"], "application/octet-stream");
    assert_eq!(g.headers()["content-length"], "10");
    assert_eq!(g.bytes().await.unwrap().as_ref(), b"zstd-bytes");
    let s = t.stats.snapshot();
    assert_eq!((s.hits, s.puts, s.time_saved_ms), (1, 1, 1500));
    assert_eq!((s.bytes_up, s.bytes_down), (10, 10));
}

#[tokio::test]
async fn put_without_artifact_headers() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    let r = put(&t, "ab", b"x", &[("x-artifact-duration", "soon")]).await;
    assert_eq!(r.status(), 200);
    let g = get_artifact(&t, "ab").await;
    assert_eq!(g.status(), 200);
    assert!(g.headers().get("x-artifact-duration").is_none());
    assert!(g.headers().get("x-artifact-tag").is_none());
    assert_eq!(g.bytes().await.unwrap().as_ref(), b"x");
    assert_eq!(t.stats.snapshot().time_saved_ms, 0);
}

#[tokio::test]
async fn head_and_miss() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    assert_eq!(head(&t, "ffff").await.status(), 404);
    assert_eq!(t.stats.snapshot().misses, 1);
    assert_eq!(get_artifact(&t, "ffff").await.status(), 404);
    assert_eq!(t.stats.snapshot().misses, 2);
}

#[tokio::test]
async fn head_hit_returns_headers() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    put(
        &t,
        "beef",
        b"0123456789abcdef",
        &[("x-artifact-duration", "42"), ("x-artifact-tag", "dGFn")],
    )
    .await;
    let h = head(&t, "beef").await;
    assert_eq!(h.status(), 200);
    assert_eq!(h.headers()["x-artifact-duration"], "42");
    assert_eq!(h.headers()["x-artifact-tag"], "dGFn");
    assert_eq!(h.headers()["content-length"], "16");
    // HEAD neither counts a hit nor downloads anything.
    let s = t.stats.snapshot();
    assert_eq!(
        (s.hits, s.misses, s.bytes_down, s.time_saved_ms),
        (0, 0, 0, 0)
    );
}

#[tokio::test]
async fn invalid_hash_is_400() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    let long = "a".repeat(129);
    for h in ["..%2Fx", "zz", "abc.def", long.as_str()] {
        assert_eq!(get_artifact(&t, h).await.status(), 400, "GET {h}");
        assert_eq!(head(&t, h).await.status(), 400, "HEAD {h}");
        assert_eq!(put(&t, h, b"x", &[]).await.status(), 400, "PUT {h}");
    }
    let s = t.stats.snapshot();
    assert_eq!((s.misses, s.puts, s.put_errors), (0, 0, 0));
    assert!(t.store.list("").await.unwrap().is_empty());
}

#[tokio::test]
async fn duplicate_put_is_200_and_counted() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    assert_eq!(put(&t, "dd", b"first", &[]).await.status(), 200);
    let r = put(&t, "dd", b"second", &[]).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap(), json!({"urls": []}));
    let s = t.stats.snapshot();
    assert_eq!((s.puts, s.put_skipped_existing, s.put_errors), (1, 1, 0));
    assert_eq!(
        get_artifact(&t, "dd").await.bytes().await.unwrap().as_ref(),
        b"first"
    );
}

#[tokio::test]
async fn corrupt_object_is_a_miss() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    t.store
        .op
        .write(&key("cc"), b"PK\x03\x04 not hoard".to_vec())
        .await
        .unwrap();
    assert_eq!(get_artifact(&t, "cc").await.status(), 404);
    let s = t.stats.snapshot();
    assert_eq!((s.corrupt, s.misses, s.hits), (1, 1, 0));
    assert_eq!(head(&t, "cc").await.status(), 404);
}

#[tokio::test]
async fn read_mode_rejects_put() {
    let t = spawn_test_server(Mode::Read).await;
    assert_eq!(put(&t, "aa", b"x", &[]).await.status(), 403);
    assert!(!exists(&t, "aa").await);
    // Seed directly; GET still works.
    let rw = hoard::envelope::Header {
        v: 1,
        duration_ms: Some(7),
        tag: None,
        size: 3,
    };
    t.store
        .put(
            &key("aa"),
            &rw,
            futures::stream::iter([Ok(Bytes::from_static(b"abc"))]),
        )
        .await
        .unwrap();
    let g = get_artifact(&t, "aa").await;
    assert_eq!(g.status(), 200);
    assert_eq!(g.bytes().await.unwrap().as_ref(), b"abc");
    assert_eq!(t.stats.snapshot().puts, 0);
}

#[tokio::test]
async fn off_mode_status_disabled() {
    let t = spawn_test_server(Mode::Off).await;
    let s: Value = get(&t, "/v8/artifacts/status", Some(&t.token))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(s, json!({"status": "disabled"}));
    assert_eq!(put(&t, "aa", b"x", &[]).await.status(), 403);
    assert!(!exists(&t, "aa").await);
    let rw = hoard::envelope::Header {
        v: 1,
        duration_ms: None,
        tag: None,
        size: 1,
    };
    t.store
        .put(
            &key("aa"),
            &rw,
            futures::stream::iter([Ok(Bytes::from_static(b"x"))]),
        )
        .await
        .unwrap();
    assert_eq!(get_artifact(&t, "aa").await.status(), 404);
    assert_eq!(head(&t, "aa").await.status(), 404);
    let b: Value = authed(&t, Method::POST, "/v8/artifacts")
        .json(&json!({"hashes": ["aa"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(b, json!({}));
    assert_eq!(t.stats.snapshot(), Default::default());
}

#[tokio::test]
async fn events_and_stats() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    let r = authed(&t, Method::POST, "/v8/artifacts/events")
        .json(&json!([{"source":"LOCAL","event":"HIT","hash":"aa"}]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = authed(&t, Method::POST, "/v8/artifacts/events")
        .body("not json")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    get_artifact(&t, "aa").await;
    let s: Value = authed(&t, Method::GET, "/_hoard/stats")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for k in ["hits", "misses", "time_saved_ms"] {
        assert!(s.get(k).is_some(), "missing {k} in {s}");
    }
    assert_eq!(s["misses"], 1);
}

#[tokio::test]
async fn batch_query() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    put(&t, "aa", b"hello", &[("x-artifact-duration", "250")]).await;
    put(&t, "cd", b"hi", &[]).await;
    let before = t.stats.snapshot();
    let b: Value = authed(&t, Method::POST, "/v8/artifacts?teamId=x")
        .json(&json!({"hashes": ["aa", "bb", "cd", "../x"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        b,
        json!({
            "aa": {"size": 5, "taskDurationMs": 250},
            "cd": {"size": 2, "taskDurationMs": null},
        })
    );
    assert_eq!(t.stats.snapshot(), before);
}

#[tokio::test]
async fn shutdown_signals_channel() {
    let mut t = spawn_test_server(Mode::ReadWrite).await;
    let r = authed(&t, Method::POST, "/_hoard/shutdown")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "ok");
    let got = tokio::time::timeout(Duration::from_secs(5), t.shutdown.recv()).await;
    assert_eq!(got.unwrap(), Some(()));
}

#[tokio::test]
async fn put_without_content_length_is_skipped() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    let stream = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))]);
    let r = authed(&t, Method::PUT, "/v8/artifacts/ee")
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap(), json!({"urls": []}));
    let s = t.stats.snapshot();
    assert_eq!((s.puts, s.put_errors), (0, 1));
    assert!(!exists(&t, "ee").await);
}

#[tokio::test]
async fn huge_tag_is_a_put_error() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    let tag = "a".repeat(5000);
    let r = put(&t, "ef", b"x", &[("x-artifact-tag", tag.as_str())]).await;
    assert_eq!(r.status(), 200);
    let s = t.stats.snapshot();
    assert_eq!((s.puts, s.put_errors), (0, 1));
    assert!(!exists(&t, "ef").await);
}

#[tokio::test]
async fn truncated_body_leaves_no_object() {
    let t = spawn_test_server(Mode::ReadWrite).await;
    let mut sock = tokio::net::TcpStream::connect(t.addr).await.unwrap();
    let req = format!(
        "PUT /v8/artifacts/abcd HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {}\r\n\
         content-length: 100\r\n\r\nonly-ten-b",
        t.token
    );
    sock.write_all(req.as_bytes()).await.unwrap();
    sock.shutdown().await.unwrap();
    let mut resp = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut resp)).await;
    for _ in 0..100 {
        if t.stats.snapshot().put_errors == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let s = t.stats.snapshot();
    assert_eq!((s.puts, s.put_errors), (0, 1));
    assert!(!exists(&t, "abcd").await);
    assert!(t.store.list("").await.unwrap().is_empty());
}

/// xorshift64 chunks: deterministic per seed, cheap to generate twice.
fn chunks(seed: u64, total: usize, chunk: usize) -> impl Iterator<Item = Bytes> {
    let mut x = seed | 1;
    let mut left = total;
    std::iter::from_fn(move || {
        if left == 0 {
            return None;
        }
        let n = left.min(chunk);
        left -= n;
        let mut v = Vec::with_capacity(n + 8);
        while v.len() < n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.truncate(n);
        Some(Bytes::from(v))
    })
}

#[tokio::test]
#[ignore]
async fn large_artifact_streams() {
    use sha2::{Digest, Sha256};
    const TOTAL: usize = 150 * 1024 * 1024;
    let t = spawn_test_server(Mode::ReadWrite).await;
    let seed: u64 = rand::random();
    let mut sent = Sha256::new();
    for c in chunks(seed, TOTAL, 1 << 20) {
        sent.update(&c);
    }
    let stream = futures::stream::iter(chunks(seed, TOTAL, 1 << 20).map(Ok::<_, std::io::Error>));
    let r = authed(&t, Method::PUT, "/v8/artifacts/1a2b3c")
        .header("content-length", TOTAL.to_string())
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let s = t.stats.snapshot();
    assert_eq!((s.puts, s.put_errors, s.bytes_up), (1, 0, TOTAL as u64));

    let mut g = get_artifact(&t, "1a2b3c").await;
    assert_eq!(g.status(), 200);
    assert_eq!(g.content_length(), Some(TOTAL as u64));
    let mut recv = Sha256::new();
    let mut n = 0usize;
    while let Some(c) = g.chunk().await.unwrap() {
        n += c.len();
        recv.update(&c);
    }
    assert_eq!(n, TOTAL);
    assert_eq!(sent.finalize(), recv.finalize());
    assert_eq!(t.stats.snapshot().bytes_down, TOTAL as u64);
}
