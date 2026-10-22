//! Turborepo HTTP API with `pack` on (fs store): PUTs become packs at shutdown, a later server
//! (the next CI job) reads them through the index.

mod common;

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use common::{TestServer, spawn_pack_server};
use hoard::config::Mode;
use reqwest::{Client, Method, Response};
use serde_json::{Value, json};

const A: &str = "aaaa01";
const B: &str = "bbbb02";
const C: &str = "cccc03";
const D: &str = "dddd04";

fn authed(t: &TestServer, method: Method, path: &str) -> reqwest::RequestBuilder {
    Client::new()
        .request(method, format!("{}{path}", t.base_url))
        .bearer_auth(&t.token)
}

async fn get(t: &TestServer, hash: &str) -> Response {
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

async fn put(t: &TestServer, hash: &str, body: &'static [u8]) -> Response {
    authed(t, Method::PUT, &format!("/v8/artifacts/{hash}"))
        .header("x-artifact-duration", "250")
        .header("x-artifact-tag", format!("tag-{hash}"))
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn query(t: &TestServer, hashes: &[&str]) -> Value {
    authed(t, Method::POST, "/v8/artifacts")
        .json(&json!({ "hashes": hashes }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// A server for "job" `run`: a distinct run key, the same fs root.
async fn job(root: &Path, mode: Mode, no_reads: bool, run: &str) -> TestServer {
    spawn_pack_server(
        root,
        mode,
        no_reads,
        &[("GITHUB_RUN_ID", run), ("GITHUB_RUN_ATTEMPT", "1")],
    )
    .await
}

/// What `run_serve` does at shutdown for packs.
async fn finish(t: &TestServer) -> hoard::pack::writer::PackDrain {
    t.packs
        .as_ref()
        .unwrap()
        .writer
        .as_ref()
        .unwrap()
        .drain(Duration::from_secs(30))
        .await
}

async fn first_job(root: &Path) {
    let t = job(root, Mode::ReadWrite, false, "1").await;
    assert_eq!(put(&t, A, b"alpha").await.status(), 200);
    assert_eq!(put(&t, B, b"bravo-bravo").await.status(), 200);
    assert_eq!(put(&t, C, b"charlie").await.status(), 200);
    // Packs only exist after shutdown: nothing is readable before.
    assert_eq!(head(&t, A).await.status(), 404);
    assert_eq!(get(&t, A).await.status(), 404);
    assert!(query(&t, &[A, B, C]).await.as_object().unwrap().is_empty());
    let drain = finish(&t).await;
    assert_eq!(
        (drain.packs, drain.dropped, drain.index_written),
        (1, 0, true)
    );
    let s = t.stats.snapshot();
    assert_eq!((s.puts, s.packs, s.put_errors), (3, 1, 0));
    assert_eq!(s.misses, 2);
}

#[tokio::test]
async fn packed_artifacts_are_served_by_the_next_job() {
    let dir = tempfile::tempdir().unwrap();
    first_job(dir.path()).await;

    let t = job(dir.path(), Mode::ReadWrite, false, "2").await;
    let h = head(&t, B).await;
    assert_eq!(h.status(), 200);
    assert_eq!(h.headers()["content-length"], "11");
    assert_eq!(h.headers()["x-artifact-duration"], "250");
    assert_eq!(h.headers()["x-artifact-tag"], "tag-bbbb02");
    let g = get(&t, B).await;
    assert_eq!(g.status(), 200);
    assert_eq!(g.headers()["x-artifact-duration"], "250");
    assert_eq!(g.bytes().await.unwrap().as_ref(), b"bravo-bravo");
    assert_eq!(get(&t, A).await.bytes().await.unwrap().as_ref(), b"alpha");
    assert_eq!(head(&t, D).await.status(), 404);
    assert_eq!(get(&t, D).await.status(), 404);

    let q = query(&t, &[A, B, C, D, "zz"]).await;
    let keys: HashSet<&str> = q.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, HashSet::from([A, B, C]));
    assert_eq!(q[B], json!({"size": 11, "taskDurationMs": 250}));

    let s = t.stats.snapshot();
    assert_eq!((s.hits, s.misses), (2, 2));
    assert_eq!(s.bytes_down, 5 + 11);
    assert_eq!(s.time_saved_ms, 500);

    // A duplicate PUT of an indexed hash is skipped.
    assert_eq!(put(&t, A, b"alpha").await.status(), 200);
    assert_eq!(t.stats.snapshot().put_skipped_existing, 1);
}

#[tokio::test]
async fn read_only_job_reads_packs_and_refuses_puts() {
    let dir = tempfile::tempdir().unwrap();
    first_job(dir.path()).await;
    let t = job(dir.path(), Mode::Read, false, "2").await;
    assert!(t.packs.as_ref().unwrap().writer.is_none());
    assert_eq!(get(&t, C).await.bytes().await.unwrap().as_ref(), b"charlie");
    assert_eq!(put(&t, D, b"delta").await.status(), 403);
    assert_eq!(t.stats.snapshot().put_errors, 0);
}

#[tokio::test]
async fn no_reads_misses_everything() {
    let dir = tempfile::tempdir().unwrap();
    first_job(dir.path()).await;
    let t = job(dir.path(), Mode::ReadWrite, true, "2").await;
    assert_eq!(head(&t, A).await.status(), 404);
    assert_eq!(get(&t, A).await.status(), 404);
    assert!(query(&t, &[A, B]).await.as_object().unwrap().is_empty());
    assert_eq!(t.stats.snapshot().hits, 0);
}

#[tokio::test]
async fn second_job_adds_to_the_index() {
    let dir = tempfile::tempdir().unwrap();
    first_job(dir.path()).await;

    let t = job(dir.path(), Mode::ReadWrite, false, "2").await;
    assert_eq!(put(&t, D, b"delta").await.status(), 200);
    let drain = finish(&t).await;
    assert_eq!((drain.packs, drain.index_written), (1, true));
    assert_eq!(t.stats.snapshot().puts, 1);

    let t3 = job(dir.path(), Mode::Read, false, "3").await;
    let q = query(&t3, &[A, B, C, D]).await;
    assert_eq!(q.as_object().unwrap().len(), 4);
    assert_eq!(get(&t3, D).await.bytes().await.unwrap().as_ref(), b"delta");
    assert_eq!(get(&t3, A).await.bytes().await.unwrap().as_ref(), b"alpha");
}
