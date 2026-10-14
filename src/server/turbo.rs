//! Turborepo remote-cache API (`/v8/artifacts`). Storage errors never fail the build:
//! GET/HEAD become misses, PUT answers 200 and counts `put_errors`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

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
use crate::redact::redacted;
use crate::store::{GetOutcome, PutOutcome};

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
    if s.mode == Mode::Off {
        return Json(HashMap::new());
    }
    let hashes = serde_json::from_slice::<Query>(&body)
        .map(|q| q.hashes)
        .unwrap_or_default();
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
    let (h, body) = match s.store.get(&s.turbo_key(&hash)).await {
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
    (StatusCode::OK, headers, Body::from_stream(stream)).into_response()
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
        return StatusCode::FORBIDDEN.into_response();
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
    };
    if encode_header(&header).len() > PREFIX_LEN + MAX_HEADER_LEN {
        warn!(%hash, "PUT: artifact headers too large for the envelope, not caching");
        inc(&s.stats.put_errors, 1);
        return put_ok();
    }
    let stream = ExactLength::new(body.into_data_stream().map_err(std::io::Error::other), size);
    match s.store.put(&s.turbo_key(&hash), &header, stream).await {
        Ok(PutOutcome::Created) => {
            inc(&s.stats.puts, 1);
            inc(&s.stats.bytes_up, size);
        }
        Ok(PutOutcome::AlreadyExists) => inc(&s.stats.put_skipped_existing, 1),
        Err(e) => {
            warn!(%hash, error = %redacted(&e), "PUT: storage error, not cached");
            inc(&s.stats.put_errors, 1);
        }
    }
    put_ok()
}
