//! Storage errors are logged redacted. Its own test binary: tracing caches per-callsite
//! interest process-wide, so a test running in parallel without a subscriber could hide this
//! test's events from its scoped subscriber.

mod common;

use hoard::config::Mode;
use reqwest::{Client, Method};
use serde_json::json;

fn authed(t: &common::TestServer, method: Method, path: &str) -> reqwest::RequestBuilder {
    Client::new()
        .request(method, format!("{}{path}", t.base_url))
        .bearer_auth(&t.token)
}

/// A `Write` handle onto a shared buffer, for capturing log output.
#[derive(Clone, Default)]
struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An S3 endpoint that rejects every request with a body holding a signed Azure URL, the way
/// a ghac error carries its SAS URL.
async fn sas_leaking_endpoint(secret: &'static str) -> String {
    let body = format!(
        "denied for https://acct.blob.core.windows.net/c/k?sv=2024-08-04&se=2026-10-14&sp=r&sig={secret}"
    );
    let app = axum::Router::new().fallback(move || {
        let body = body.clone();
        async move { (axum::http::StatusCode::FORBIDDEN, body) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Storage errors are logged redacted: the SAS signature in an OpenDAL error never reaches the
/// log, on GET, HEAD, the batch query or PUT.
#[tokio::test]
async fn storage_error_logs_are_redacted() {
    const SECRET: &str = "s3cr3tSiGnAtUrE";
    let logs = LogBuf::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    // Current-thread runtime: the server's tasks run on this thread and see the subscriber.
    let _guard = tracing::subscriber::set_default(subscriber);

    let endpoint = sas_leaking_endpoint(SECRET).await;
    let dir = tempfile::tempdir().unwrap();
    let op = hoard::r2::s3_operator(&endpoint, "auto", "b", "k", "s").unwrap();
    let t =
        common::spawn_test_server_with(hoard::store::Store::new(op), Mode::ReadWrite, dir).await;
    assert_eq!(
        authed(&t, Method::GET, "/v8/artifacts/aa")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        authed(&t, Method::HEAD, "/v8/artifacts/aa")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let q = authed(&t, Method::POST, "/v8/artifacts")
        .json(&json!({"hashes": ["aa"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(q.status(), 200);
    let p = authed(&t, Method::PUT, "/v8/artifacts/aa")
        .body(&b"abc"[..])
        .send()
        .await
        .unwrap();
    assert_eq!(p.status(), 200);
    assert_eq!(t.stats.snapshot().put_errors, 1);

    let out = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    let warns: Vec<&str> = out.lines().filter(|l| l.contains("WARN")).collect();
    assert!(warns.len() >= 4, "{out}");
    for line in &warns {
        assert!(line.contains("storage error"), "{line}");
        assert!(line.contains("sig=REDACTED"), "{line}");
    }
    assert!(!out.contains(SECRET), "secret leaked: {out}");
    assert!(!out.contains("sig=s"), "{out}");
}
