//! Turborepo remote-cache API (`/v8/artifacts`). Storage errors never fail the build:
//! GET/HEAD become misses, PUT answers 200 and counts `put_errors` (or `rate_limited`, when
//! the backend rate-limits uploads; uploads then pause, reads continue).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use super::AppState;
use super::body::ExactLength;
use crate::config::Mode;
use crate::envelope::{Header, MAX_HEADER_LEN, PREFIX_LEN, encode_header};
use crate::key::validate_hash;
use crate::pack::reader::PackGet;
use crate::pack::writer::Accepted;
use crate::redact::redacted;
use crate::store::{GetOutcome, PutOutcome, StoreError};
use crate::writebehind::Enqueued;

const DURATION: &str = "x-artifact-duration";
const TAG: &str = "x-artifact-tag";
/// Parallel HEADs for one batch query.
const QUERY_CONCURRENCY: usize = 16;

fn inc(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

fn put_ok() -> Response {
    Json(json!({"urls": []})).into_response()
}

/// `403` with the error shape Turborepo parses (`{"error":{"code","message"}}`), so it prints
/// `Unknown status read_only: hoard is in read-only mode` instead of a JSON-parse warning.
/// The code must not be `forbidden` or start with `remote_caching_`: Turborepo answers those by
/// disabling the remote cache for the rest of the run, reads included.
fn forbidden(mode: Mode) -> Response {
    let (code, message) = match mode {
        Mode::Off => ("off", "hoard is off (mode=off)"),
        _ => ("read_only", "hoard is in read-only mode"),
    };
    let body = json!({"error": {"code": code, "message": message}});
    (StatusCode::FORBIDDEN, Json(body)).into_response()
}

pub async fn status(State(s): State<AppState>) -> Json<serde_json::Value> {
    let status = if s.mode == Mode::Off {
        "disabled"
    } else {
        "enabled"
    };
    Json(json!({ "status": status }))
}

pub async fn events(_body: Bytes) -> StatusCode {
    StatusCode::OK
}

#[derive(Deserialize)]
pub struct Query {
    #[serde(default)]
    hashes: Vec<String>,
}

#[derive(Serialize)]
pub struct Found {
    size: u64,
    #[serde(rename = "taskDurationMs")]
    task_duration_ms: Option<u64>,
}

/// Batch existence check. Invalid hashes and misses are left out; stats are unchanged.
pub async fn query(State(s): State<AppState>, body: Bytes) -> Json<HashMap<String, Found>> {
    if s.mode == Mode::Off || s.no_reads {
        return Json(HashMap::new());
    }
    let hashes = serde_json::from_slice::<Query>(&body)
        .map(|q| q.hashes)
        .unwrap_or_default();
    if let Some(packs) = &s.packs {
        // Answered from the index: no storage request per hash.
        let mut found = HashMap::new();
        for hash in hashes.into_iter().filter(|h| validate_hash(h)) {
            if let Some(e) = packs.reader.lookup(&hash).await {
                found.insert(
                    hash,
                    Found {
                        size: e.loc.size,
                        task_duration_ms: e.loc.duration_ms,
                    },
                );
            }
        }
        return Json(found);
    }
    let found = futures::stream::iter(hashes.into_iter().filter(|h| validate_hash(h)))
        .map(|hash| {
            let s = &s;
            async move {
                match s.store.head(&s.turbo_key(&hash)).await {
                    Ok(Some(h)) => Some((
                        hash,
                        Found {
                            size: h.size,
                            task_duration_ms: h.duration_ms,
                        },
                    )),
                    Ok(None) => None,
                    Err(e) => {
                        warn!(%hash, error = %redacted(&e), "batch query: storage error, treating as miss");
                        None
                    }
                }
            }
        })
        .buffer_unordered(QUERY_CONCURRENCY)
        .filter_map(futures::future::ready)
        .collect()
        .await;
    Json(found)
}

/// `Content-Length` plus the artifact headers that are present.
fn artifact_headers(h: &Header) -> HeaderMap {
    let mut out = HeaderMap::new();
    out.insert(header::CONTENT_LENGTH, HeaderValue::from(h.size));
    if let Some(d) = h.duration_ms {
        out.insert(DURATION, HeaderValue::from(d));
    }
    if let Some(v) = h.tag.as_deref().and_then(|t| HeaderValue::from_str(t).ok()) {
        out.insert(TAG, v);
    }
    out
}

pub async fn head_artifact(State(s): State<AppState>, Path(hash): Path<String>) -> Response {
    if !validate_hash(&hash) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if s.mode == Mode::Off {
        return StatusCode::NOT_FOUND.into_response();
    }
    if s.no_reads {
        inc(&s.stats.misses, 1);
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(packs) = &s.packs {
        return match packs.reader.lookup(&hash).await {
            Some(e) => {
                let h = Header {
                    v: 1,
                    duration_ms: e.loc.duration_ms,
                    tag: e.loc.tag,
                    size: e.loc.size,
                    hash: None,
                };
                (StatusCode::OK, artifact_headers(&h)).into_response()
            }
            None => {
                inc(&s.stats.misses, 1);
                StatusCode::NOT_FOUND.into_response()
            }
        };
    }
    match s.store.head(&s.turbo_key(&hash)).await {
        Ok(Some(h)) => (StatusCode::OK, artifact_headers(&h)).into_response(),
        Ok(None) => {
            inc(&s.stats.misses, 1);
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            warn!(%hash, error = %redacted(&e), "HEAD: storage error, treating as miss");
            inc(&s.stats.misses, 1);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

pub async fn get_artifact(State(s): State<AppState>, Path(hash): Path<String>) -> Response {
    if !validate_hash(&hash) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if s.mode == Mode::Off {
        return StatusCode::NOT_FOUND.into_response();
    }
    let miss = || {
        inc(&s.stats.misses, 1);
        StatusCode::NOT_FOUND.into_response()
    };
    if s.no_reads {
        return miss();
    }
    let key = s.turbo_key(&hash);
    let (h, body) = if let Some(packs) = &s.packs {
        match packs.reader.get(&hash).await {
            PackGet::Hit(h, body) => (h, body),
            PackGet::Miss => return miss(),
            PackGet::Corrupt => {
                warn!(%hash, "GET: packed artifact is corrupt, treating as miss");
                inc(&s.stats.corrupt, 1);
                return miss();
            }
        }
    } else {
        match s.store.get(&key).await {
            Ok(GetOutcome::Hit(h, body)) => (h, body),
            Ok(GetOutcome::Miss) => return miss(),
            Ok(GetOutcome::Corrupt) => {
                warn!(%hash, "GET: stored object is not a valid HRD1 blob, treating as miss");
                inc(&s.stats.corrupt, 1);
                return miss();
            }
            Err(e) => {
                warn!(%hash, error = %redacted(&e), "GET: storage error, treating as miss");
                return miss();
            }
        }
    };
    // The header's size is the body length: a stored body of another length ends the response
    // with an error instead of sending bytes that do not match Content-Length.
    let stream = ExactLength::new(body, h.size);
    inc(&s.stats.hits, 1);
    inc(&s.stats.bytes_down, h.size);
    inc(&s.stats.time_saved_ms, h.duration_ms.unwrap_or(0));
    let mut headers = artifact_headers(&h);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    let response = (StatusCode::OK, headers, Body::from_stream(stream)).into_response();
    // Background and non-blocking: the response above never waits for it.
    if let Some(refresh) = &s.refresh {
        refresh.on_hit(&key);
    }
    response
}

pub async fn put_artifact(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !validate_hash(&hash) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if s.mode != Mode::ReadWrite {
        return forbidden(s.mode);
    }
    // Paused after a rate limit: skip without touching the store (the body is dropped unread).
    if !s.rate_gate.allow_upload(Instant::now()) {
        inc(&s.stats.rate_limited, 1);
        return put_ok();
    }
    let Some(size) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    else {
        warn!(%hash, "PUT: missing or invalid Content-Length, not caching");
        inc(&s.stats.put_errors, 1);
        return put_ok();
    };
    let header = Header {
        v: 1,
        duration_ms: headers
            .get(DURATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok()),
        tag: headers
            .get(TAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        size,
        hash: None,
    };
    if encode_header(&header).len() > PREFIX_LEN + MAX_HEADER_LEN {
        warn!(%hash, "PUT: artifact headers too large for the envelope, not caching");
        inc(&s.stats.put_errors, 1);
        return put_ok();
    }
    // Packing: spool the body into the next pack; it is uploaded in the background and at
    // shutdown. Nothing is written to the store per artifact.
    if let Some(packs) = &s.packs {
        let Some(writer) = &packs.writer else {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                warn!("PUT: packing has no writer (spool dir unusable); artifacts are not cached");
            }
            inc(&s.stats.put_errors, 1);
            return put_ok();
        };
        let stream = Box::pin(body.into_data_stream().map_err(std::io::Error::other));
        match writer.put(&hash, header, stream).await {
            Ok(Accepted::Spooled) => {}
            Ok(Accepted::Duplicate) => inc(&s.stats.put_skipped_existing, 1),
            Ok(Accepted::NotCached(why)) => {
                warn!(%hash, why, "PUT: not cached");
                inc(&s.stats.put_errors, 1);
            }
            Err(e) => {
                warn!(%hash, error = %e, "PUT: cannot spool the artifact into a pack, not cached");
                inc(&s.stats.put_errors, 1);
            }
        }
        return put_ok();
    }
    let key = s.turbo_key(&hash);
    let mut stream = ExactLength::new(body.into_data_stream().map_err(std::io::Error::other), size);
    // Write-behind: answer once the body is spooled; a worker uploads it later. A full spool
    // or a closed (draining) queue hands the body back for the synchronous upload below.
    if let Some(wb) = &s.write_behind {
        match wb.enqueue(key.clone(), header.clone(), stream).await {
            Ok(Enqueued::Spooled) => return put_ok(),
            Ok(Enqueued::Duplicate) => {
                inc(&s.stats.put_skipped_existing, 1);
                return put_ok();
            }
            Ok(Enqueued::Declined(back)) => stream = back,
            Err(e) => {
                warn!(%hash, error = %e, "PUT: cannot spool the artifact for upload, not cached");
                inc(&s.stats.put_errors, 1);
                return put_ok();
            }
        }
    }
    match s.store.put(&key, &header, stream).await {
        Ok(PutOutcome::Created) => {
            inc(&s.stats.puts, 1);
            inc(&s.stats.bytes_up, size);
        }
        Ok(PutOutcome::AlreadyExists) => inc(&s.stats.put_skipped_existing, 1),
        Err(StoreError::RateLimited { retry_after }) => {
            s.rate_gate.on_rate_limited(Instant::now(), retry_after);
            inc(&s.stats.rate_limited, 1);
            inc(&s.stats.throttled, 1);
        }
        Err(e) => {
            warn!(%hash, error = %redacted(&e), "PUT: storage error, not cached");
            inc(&s.stats.put_errors, 1);
        }
    }
    put_ok()
}
