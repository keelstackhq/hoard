//! Turborepo HTTP API against a real server on 127.0.0.1 (fs store).

mod common;

use std::time::Duration;

use bytes::Bytes;
use common::{TestServer, fs_config, spawn_test_server, wait_for_file};
use hoard::config::Mode;
use hoard::key::artifact_key;
use hoard::serve::{env_lines, run_serve};
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
    let r = put(&t, "aa", b"x", &[]).await;
    assert_eq!(r.status(), 403);
    // Turborepo parses a 403 body as {"error":{"code","message"}} and prints both. The code is
    // not "forbidden": that makes Turborepo disable the remote cache (reads too) for the run.
    assert_eq!(
        r.headers()["content-type"].to_str().unwrap(),
        "application/json"
    );
    let body: Value = r.json().await.unwrap();
    assert_eq!(
        body,
        json!({"error": {"code": "read_only", "message": "hoard is in read-only mode"}})
    );
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
    let r = put(&t, "aa", b"x", &[]).await;
    assert_eq!(r.status(), 403);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["code"], "off");
    assert_eq!(body["error"]["message"], "hoard is off (mode=off)");
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
async fn shutdown_waits_for_reply() {
    let mut t = spawn_test_server(Mode::ReadWrite).await;
    let req = authed(&t, Method::POST, "/_hoard/shutdown").send();
    let resp = tokio::spawn(req);
    let reply = tokio::time::timeout(Duration::from_secs(5), t.shutdown.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !resp.is_finished(),
        "responded before the shutdown work finished"
    );
    reply.send(()).unwrap();
    let r = resp.await.unwrap().unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn shutdown_answers_when_nobody_listens() {
    let mut t = spawn_test_server(Mode::ReadWrite).await;
    t.shutdown.close();
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        authed(&t, Method::POST, "/_hoard/shutdown").send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r.status(), 200);
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

// ---- `hoard serve` (run_serve) and `hoard env` ----

fn client() -> Client {
    Client::new()
}

async fn post_shutdown(port: u16, token: &str) -> reqwest::Result<Response> {
    client()
        .post(format!("http://127.0.0.1:{port}/_hoard/shutdown"))
        .bearer_auth(token)
        .send()
        .await
}

#[tokio::test]
async fn serve_writes_port_and_token_files_and_summary_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fs_config(&dir); // port 0, port_file/token_file/summary_file inside dir
    let h = tokio::spawn(run_serve(cfg.clone()));
    let port: u16 = wait_for_file(&cfg.port_file).await.trim().parse().unwrap();
    let token = std::fs::read_to_string(&cfg.token_file).unwrap();
    assert_eq!(token.len(), 64);
    let r = client()
        .post(format!("http://127.0.0.1:{port}/_hoard/shutdown"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    h.await.unwrap().unwrap();
    assert!(
        std::fs::read_to_string(cfg.summary_file.unwrap())
            .unwrap()
            .contains("### hoard")
    );
}

#[test]
fn env_output() {
    assert_eq!(
        env_lines(4123, "t0k"),
        "export TURBO_API=http://127.0.0.1:4123\nexport TURBO_TOKEN=t0k\nexport TURBO_TEAM=hoard\n"
    );
}

#[tokio::test]
async fn serve_summary_is_written_before_shutdown_responds() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fs_config(&dir);
    let h = tokio::spawn(run_serve(cfg.clone()));
    let port: u16 = wait_for_file(&cfg.port_file).await.trim().parse().unwrap();
    let token = std::fs::read_to_string(&cfg.token_file).unwrap();
    // The random token is 64 lowercase hex chars.
    assert!(token.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    let r = post_shutdown(port, &token).await.unwrap();
    assert_eq!(r.status(), 200);
    let summary = std::fs::read_to_string(cfg.summary_file.as_ref().unwrap()).unwrap();
    assert!(
        summary.contains(&format!("Storage: fs ({})", cfg.fs_root.display())),
        "{summary}"
    );
    tokio::time::timeout(Duration::from_secs(10), h)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn serve_removes_port_and_token_files_after_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fs_config(&dir);
    let h = tokio::spawn(run_serve(cfg.clone()));
    let port: u16 = wait_for_file(&cfg.port_file).await.trim().parse().unwrap();
    let token = std::fs::read_to_string(&cfg.token_file).unwrap();
    post_shutdown(port, &token).await.unwrap();
    h.await.unwrap().unwrap();
    assert!(!cfg.port_file.exists());
    assert!(!cfg.token_file.exists());
}

#[tokio::test]
async fn serve_second_shutdown_does_not_hang() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fs_config(&dir);
    let h = tokio::spawn(run_serve(cfg.clone()));
    let port: u16 = wait_for_file(&cfg.port_file).await.trim().parse().unwrap();
    let token = std::fs::read_to_string(&cfg.token_file).unwrap();
    let (a, b) = tokio::time::timeout(
        Duration::from_secs(10),
        futures::future::join(post_shutdown(port, &token), post_shutdown(port, &token)),
    )
    .await
    .expect("a shutdown request hung");
    // A request that reaches the server gets 200; one that arrives after the listener
    // closed may fail to connect, which is fine.
    let ok = [a, b]
        .into_iter()
        .filter_map(|r| r.ok())
        .inspect(|r| assert_eq!(r.status(), 200))
        .count();
    assert!(ok >= 1);
    tokio::time::timeout(Duration::from_secs(10), h)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn serve_uses_given_token_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fs_config(&dir);
    cfg.token = Some("my-fixed-token".into());
    cfg.port_file = dir.path().join("nested/run/port");
    cfg.token_file = dir.path().join("nested/run/token");
    let h = tokio::spawn(run_serve(cfg.clone()));
    let port: u16 = wait_for_file(&cfg.port_file).await.trim().parse().unwrap();
    assert_eq!(
        std::fs::read_to_string(&cfg.token_file).unwrap(),
        "my-fixed-token"
    );
    let r = client()
        .get(format!("http://127.0.0.1:{port}/v8/artifacts/status"))
        .bearer_auth("my-fixed-token")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    post_shutdown(port, "my-fixed-token").await.unwrap();
    h.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn serve_token_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let cfg = fs_config(&dir);
    let h = tokio::spawn(run_serve(cfg.clone()));
    let port: u16 = wait_for_file(&cfg.port_file).await.trim().parse().unwrap();
    let mode = std::fs::metadata(&cfg.token_file)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let token = std::fs::read_to_string(&cfg.token_file).unwrap();
    post_shutdown(port, &token).await.unwrap();
    h.await.unwrap().unwrap();
}

#[tokio::test]
async fn serve_github_storage_fails_with_hint() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fs_config(&dir);
    cfg.storage = hoard::config::Storage::Github;
    let err = format!("{:#}", run_serve(cfg.clone()).await.unwrap_err());
    assert!(err.contains("GitHub Action"), "{err}");
    assert!(err.contains("--storage fs"), "{err}");
    assert!(!cfg.port_file.exists());
}

#[tokio::test]
async fn serve_summary_write_failure_does_not_fail_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fs_config(&dir);
    let blocker = dir.path().join("a-file");
    std::fs::write(&blocker, "x").unwrap();
    cfg.summary_file = Some(blocker.join("summary.md")); // parent is a regular file
    let h = tokio::spawn(run_serve(cfg.clone()));
    let port: u16 = wait_for_file(&cfg.port_file).await.trim().parse().unwrap();
    let token = std::fs::read_to_string(&cfg.token_file).unwrap();
    let r = post_shutdown(port, &token).await.unwrap();
    assert_eq!(r.status(), 200);
    h.await.unwrap().unwrap();
    assert!(!cfg.port_file.exists() && !cfg.token_file.exists());
}

#[tokio::test]
async fn serve_port_file_failure_removes_token_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fs_config(&dir);
    let blocker = dir.path().join("a-file");
    std::fs::write(&blocker, "x").unwrap();
    cfg.port_file = blocker.join("port");
    assert!(run_serve(cfg.clone()).await.is_err());
    assert!(!cfg.token_file.exists());
}
