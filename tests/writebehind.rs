//! Write-behind uploads: PUTs answer once the body is spooled, workers upload later, shutdown
//! drains the queue (fs store, slow or failing layers on top).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use common::failing_layer::FailingWrites;
use common::{TestServer, layered_fs_store, spawn_write_behind_server};
use hoard::envelope::Header;
use hoard::key::artifact_key;
use hoard::ratelimit::RateGate;
use hoard::stats::Stats;
use hoard::store::{GetOutcome, Store};
use hoard::writebehind::{DrainReport, Enqueued, SPOOL_CAP, WriteBehind};
use reqwest::{Client, Method};

const SLOW: Duration = Duration::from_millis(500);

fn key(hash: &str) -> String {
    artifact_key("cache/", "turbo", "v1", hash)
}

fn header(size: u64) -> Header {
    Header {
        v: 1,
        duration_ms: Some(7),
        tag: None,
        size,
    }
}

fn body(data: &'static [u8]) -> impl futures::Stream<Item = std::io::Result<Bytes>> + Send + Unpin {
    futures::stream::iter(vec![Ok(Bytes::from_static(data))])
}

async fn put(t: &TestServer, hash: &str, data: &'static [u8]) -> reqwest::Response {
    Client::new()
        .request(Method::PUT, format!("{}/v8/artifacts/{hash}", t.base_url))
        .bearer_auth(&t.token)
        .body(data)
        .send()
        .await
        .unwrap()
}

/// Files in `dir` (none when it does not exist).
fn files_in(dir: &Path) -> Vec<std::path::PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A queue over `store` with fresh stats and gate; the spool dir lives in `dir`.
struct Queue {
    wb: WriteBehind,
    stats: Arc<Stats>,
    gate: Arc<RateGate>,
    store: Arc<Store>,
    spool: std::path::PathBuf,
}

fn queue(store: Store, dir: &Path) -> Queue {
    let store = Arc::new(store);
    let stats = Arc::new(Stats::default());
    let gate = Arc::new(RateGate::default());
    let spool = dir.join("spool");
    let wb = WriteBehind::new(store.clone(), stats.clone(), gate.clone(), spool.clone(), 8);
    Queue {
        wb,
        stats,
        gate,
        store,
        spool,
    }
}

async fn exists(store: &Store, hash: &str) -> bool {
    store.op.exists(&key(hash)).await.unwrap()
}

#[tokio::test]
async fn put_answers_before_the_slow_upload_and_drain_waits_for_it() {
    let (store, dir) = layered_fs_store(FailingWrites::slow(SLOW));
    let t = spawn_write_behind_server(store, SPOOL_CAP, dir).await;
    let start = Instant::now();
    let r = put(&t, "aa11", b"artifact-bytes").await;
    let took = start.elapsed();
    assert_eq!(r.status(), 200);
    assert!(took < Duration::from_millis(100), "PUT took {took:?}");
    assert_eq!(t.stats.snapshot().puts, 0, "not uploaded yet");

    let report = t
        .write_behind
        .as_ref()
        .unwrap()
        .drain(Duration::from_secs(10))
        .await;
    assert_eq!(
        report,
        DrainReport {
            uploaded: 1,
            dropped: 0
        }
    );
    let s = t.stats.snapshot();
    assert_eq!((s.puts, s.bytes_up, s.put_errors), (1, 14, 0));
    match t.store.get(&key("aa11")).await.unwrap() {
        GetOutcome::Hit(h, _) => assert_eq!(h.size, 14),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn drain_waits_until_every_item_is_uploaded() {
    let (store, dir) = layered_fs_store(FailingWrites::slow(SLOW));
    let q = queue(store, dir.path());
    for hash in ["a1", "a2", "a3"] {
        let r =
            q.wb.enqueue(key(hash), header(3), body(b"abc"))
                .await
                .unwrap();
        assert!(matches!(r, Enqueued::Spooled), "{r:?}");
    }
    let start = Instant::now();
    let report = q.wb.drain(Duration::from_secs(10)).await;
    assert!(start.elapsed() >= Duration::from_millis(400));
    assert_eq!(
        report,
        DrainReport {
            uploaded: 3,
            dropped: 0
        }
    );
    for hash in ["a1", "a2", "a3"] {
        assert!(exists(&q.store, hash).await, "{hash}");
    }
    let s = q.stats.snapshot();
    assert_eq!((s.puts, s.bytes_up), (3, 9));
}

/// Review Focus 3: a drain that times out aborts the uploads in flight (the job is being
/// cancelled); they count as put_errors and leave no object, complete or partial.
#[tokio::test]
async fn drain_timeout_drops_in_flight_uploads_and_leaves_no_object() {
    let layer = FailingWrites::slow(Duration::from_secs(30));
    let calls = layer.calls.clone();
    let (store, dir) = layered_fs_store(layer);
    let q = queue(store, dir.path());
    for hash in ["b1", "b2"] {
        q.wb.enqueue(key(hash), header(3), body(b"xyz"))
            .await
            .unwrap();
    }
    // Both uploads are mid-way (writer open, body written, close pending).
    wait_until("both uploads to start", || {
        calls.write.load(Ordering::SeqCst) == 2
    })
    .await;
    let start = Instant::now();
    let report = q.wb.drain(Duration::from_millis(50)).await;
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(
        report,
        DrainReport {
            uploaded: 0,
            dropped: 2
        }
    );
    let s = q.stats.snapshot();
    assert_eq!((s.puts, s.put_errors), (0, 2));
    assert!(!exists(&q.store, "b1").await);
    assert!(!exists(&q.store, "b2").await);
    assert!(q.store.list("cache/").await.unwrap().is_empty());
    assert!(!q.spool.exists(), "spool dir removed");
}

#[tokio::test]
async fn spool_file_is_deleted_after_upload() {
    let (store, dir) = layered_fs_store(FailingWrites::slow(Duration::from_millis(200)));
    let q = queue(store, dir.path());
    q.wb.enqueue(key("c1"), header(3), body(b"abc"))
        .await
        .unwrap();
    assert_eq!(files_in(&q.spool).len(), 1, "spooled while uploading");
    let stats = q.stats.clone();
    wait_until("upload", || stats.snapshot().puts == 1).await;
    wait_until("spool file removal", || files_in(&q.spool).is_empty()).await;
    assert!(q.spool.exists(), "the dir stays until drain");
    q.wb.drain(Duration::from_secs(5)).await;
    assert!(!q.spool.exists(), "drain removes the spool dir");
}

#[tokio::test]
async fn short_body_is_a_spool_error_and_leaves_no_file() {
    let (store, dir) = layered_fs_store(FailingWrites::slow(SLOW));
    let q = queue(store, dir.path());
    let r = q.wb.enqueue(key("d1"), header(10), body(b"abc")).await;
    assert!(r.is_err(), "{r:?}");
    assert!(files_in(&q.spool).is_empty());
    let report = q.wb.drain(Duration::from_secs(5)).await;
    assert_eq!(report, DrainReport::default());
    assert!(!exists(&q.store, "d1").await);
}

#[tokio::test]
async fn same_key_queued_twice_is_spooled_once() {
    let layer = FailingWrites::slow(SLOW);
    let calls = layer.calls.clone();
    let (store, dir) = layered_fs_store(layer);
    let t = spawn_write_behind_server(store, SPOOL_CAP, dir).await;
    assert_eq!(put(&t, "e1", b"one").await.status(), 200);
    assert_eq!(put(&t, "e1", b"one").await.status(), 200);
    let s = t.stats.snapshot();
    assert_eq!(s.put_skipped_existing, 1);
    let report = t
        .write_behind
        .as_ref()
        .unwrap()
        .drain(Duration::from_secs(10))
        .await;
    assert_eq!(report.uploaded, 1);
    assert_eq!(calls.write.load(Ordering::SeqCst), 1);
    let s = t.stats.snapshot();
    assert_eq!((s.puts, s.put_skipped_existing, s.put_errors), (1, 1, 0));
}

/// A PUT that would push the spool over its cap is uploaded synchronously instead.
#[tokio::test]
async fn full_spool_falls_back_to_a_synchronous_upload() {
    let (store, dir) = layered_fs_store(FailingWrites::slow(Duration::from_millis(300)));
    let t = spawn_write_behind_server(store, 10, dir).await;
    let start = Instant::now();
    assert_eq!(put(&t, "f1", b"0123456789abcdef").await.status(), 200);
    assert!(start.elapsed() >= Duration::from_millis(300), "synchronous");
    assert_eq!(t.stats.snapshot().puts, 1, "uploaded before the response");
    // A body that fits is queued.
    let start = Instant::now();
    assert_eq!(put(&t, "f2", b"small").await.status(), 200);
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(t.stats.snapshot().puts, 1);
    t.write_behind
        .as_ref()
        .unwrap()
        .drain(Duration::from_secs(10))
        .await;
    assert_eq!(t.stats.snapshot().puts, 2);
}

#[tokio::test]
async fn enqueue_after_drain_is_declined() {
    let (store, dir) = layered_fs_store(FailingWrites::slow(Duration::from_millis(10)));
    let q = queue(store, dir.path());
    q.wb.drain(Duration::from_secs(1)).await;
    let r =
        q.wb.enqueue(key("g1"), header(3), body(b"abc"))
            .await
            .unwrap();
    assert!(matches!(r, Enqueued::Declined(_)), "{r:?}");
}

/// While the rate gate is closed the worker waits; it uploads once the gate opens.
#[tokio::test]
async fn closed_gate_delays_the_upload_until_it_opens() {
    let layer = FailingWrites::slow(Duration::from_millis(1));
    let calls = layer.calls.clone();
    let (store, dir) = layered_fs_store(layer);
    let q = queue(store, dir.path());
    q.gate
        .on_rate_limited(Instant::now(), Some(Duration::from_secs(1)));
    let start = Instant::now();
    q.wb.enqueue(key("h1"), header(3), body(b"abc"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(calls.write.load(Ordering::SeqCst), 0, "paused");
    let report = q.wb.drain(Duration::from_secs(10)).await;
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert_eq!(
        report,
        DrainReport {
            uploaded: 1,
            dropped: 0
        }
    );
    assert!(exists(&q.store, "h1").await);
}

/// A 429 during a background upload closes the gate, is counted, and the item is retried
/// once the gate opens.
#[tokio::test]
async fn rate_limited_upload_is_retried_after_the_pause() {
    let mut layer = FailingWrites::new(opendal::ErrorKind::RateLimited);
    layer.response =
        Some(r#"Parts { status: 429, version: HTTP/1.1, headers: {"retry-after": "1"} }"#.into());
    layer.fail_times = Some(Arc::new(AtomicU64::new(1)));
    let calls = layer.calls.clone();
    let (store, dir) = layered_fs_store(layer);
    let q = queue(store, dir.path());
    let start = Instant::now();
    q.wb.enqueue(key("i1"), header(3), body(b"abc"))
        .await
        .unwrap();
    let report = q.wb.drain(Duration::from_secs(10)).await;
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert_eq!(
        report,
        DrainReport {
            uploaded: 1,
            dropped: 0
        }
    );
    assert_eq!(calls.write.load(Ordering::SeqCst), 2);
    let s = q.stats.snapshot();
    assert_eq!((s.rate_limited, s.puts, s.put_errors), (1, 1, 0));
    assert!(exists(&q.store, "i1").await);
}

/// A permanent storage error in a worker is a put_error; the spool file is still removed.
#[tokio::test]
async fn failed_upload_counts_put_errors_and_removes_the_spool_file() {
    let (store, dir) = layered_fs_store(FailingWrites::new(opendal::ErrorKind::PermissionDenied));
    let q = queue(store, dir.path());
    q.wb.enqueue(key("j1"), header(3), body(b"abc"))
        .await
        .unwrap();
    let report = q.wb.drain(Duration::from_secs(10)).await;
    assert_eq!(
        report,
        DrainReport {
            uploaded: 0,
            dropped: 0
        }
    );
    assert_eq!(q.stats.snapshot().put_errors, 1);
    assert!(!q.spool.exists());
}

/// An existing key found by the worker is success (put_skipped_existing), as without the queue.
#[tokio::test]
async fn existing_key_is_skipped_by_the_worker() {
    let (store, dir) = layered_fs_store(FailingWrites::slow(Duration::from_millis(1)));
    let q = queue(store, dir.path());
    q.store
        .put(&key("k1"), &header(3), body(b"abc"))
        .await
        .unwrap();
    q.wb.enqueue(key("k1"), header(3), body(b"abc"))
        .await
        .unwrap();
    let report = q.wb.drain(Duration::from_secs(10)).await;
    assert_eq!(report.uploaded, 0);
    let s = q.stats.snapshot();
    assert_eq!((s.puts, s.put_skipped_existing), (0, 1));
}

/// A truncated PUT body is a put_error with write-behind too, and nothing is queued.
#[tokio::test]
async fn truncated_body_is_a_put_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (store, dir) = layered_fs_store(FailingWrites::slow(Duration::from_millis(1)));
    let t = spawn_write_behind_server(store, SPOOL_CAP, dir).await;
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
    let stats = t.stats.clone();
    wait_until("put_errors", || stats.snapshot().put_errors == 1).await;
    let report = t
        .write_behind
        .as_ref()
        .unwrap()
        .drain(Duration::from_secs(5))
        .await;
    assert_eq!(report, DrainReport::default());
    assert_eq!(t.stats.snapshot().puts, 0);
    assert!(t.store.list("").await.unwrap().is_empty());
}

/// `hoard serve --write-behind on` (fs): the shutdown handshake drains the queue before the
/// summary is written and before it answers.
#[tokio::test]
async fn serve_drains_the_queue_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = common::fs_config(dir.path());
    cfg.write_behind = true;
    let h = tokio::spawn(hoard::serve::run_serve(cfg.clone()));
    let port: u16 = common::wait_for_file(&cfg.port_file)
        .await
        .trim()
        .parse()
        .unwrap();
    let token = std::fs::read_to_string(&cfg.token_file).unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let r = Client::new()
        .put(format!("{base}/v8/artifacts/ff01"))
        .bearer_auth(&token)
        .body("payload")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = Client::new()
        .post(format!("{base}/_hoard/shutdown"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let summary = std::fs::read_to_string(cfg.summary_file.as_ref().unwrap()).unwrap();
    assert!(summary.contains("| 0 | 0 | 7 B |"), "{summary}");
    h.await.unwrap().unwrap();
    let path = cfg.fs_root.join(key("ff01"));
    assert!(path.exists(), "{}", path.display());
}

/// A GET of a key that is still queued is a miss (spec §5); after the upload it is a hit.
#[tokio::test]
async fn get_of_a_queued_key_is_a_miss() {
    let (store, dir) = layered_fs_store(FailingWrites::slow(SLOW));
    let t = spawn_write_behind_server(store, SPOOL_CAP, dir).await;
    assert_eq!(put(&t, "ab12", b"queued").await.status(), 200);
    let get = || {
        Client::new()
            .get(format!("{}/v8/artifacts/ab12", t.base_url))
            .bearer_auth(&t.token)
            .send()
    };
    assert_eq!(get().await.unwrap().status(), 404);
    assert_eq!(t.stats.snapshot().misses, 1);
    t.write_behind
        .as_ref()
        .unwrap()
        .drain(Duration::from_secs(10))
        .await;
    assert_eq!(get().await.unwrap().status(), 200);
}
