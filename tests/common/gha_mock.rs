//! A local stand-in for the GitHub Actions cache service v2 (Twirp JSON) and the Azure blob
//! storage behind its signed URLs. It records every request and serves what was uploaded.
//! Signed URLs carry `sig=MOCKSIG…` so tests can check that no URL leaks into output.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use serde_json::{Value, json};

/// The signature value in every signed URL the mock hands out.
pub const MOCK_SIG: &str = "MOCKSIG-do-not-leak";

#[derive(Debug, Clone)]
pub struct MockEntry {
    pub key: String,
    pub version: String,
    pub blob: String,
    pub finalized: bool,
    pub size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct MockRequest {
    pub method: String,
    /// Path without the query (`/twirp/...` or `/blob/<id>`).
    pub path: String,
    pub query: String,
    pub authorization: Option<String>,
    pub x_ms_version: Option<String>,
    /// The `Range` header of blob GETs.
    pub range: Option<String>,
    /// JSON body of Twirp calls.
    pub json: Option<Value>,
}

#[derive(Default)]
pub struct MockState {
    pub entries: Vec<MockEntry>,
    /// Uncommitted blocks per blob.
    pub blocks: HashMap<String, HashMap<String, Vec<u8>>>,
    /// Committed blobs.
    pub blobs: HashMap<String, Vec<u8>>,
    pub requests: Vec<MockRequest>,
    /// When set, every Twirp call answers with this status.
    pub fail_status: Option<u16>,
    /// The next `n` Twirp calls answer with `status`, then calls succeed again.
    pub fail_twirp_next: Option<(u16, usize)>,
    /// The next `n` blob GETs answer with `status`, then GETs succeed again.
    pub fail_blob_get_next: Option<(u16, usize)>,
    /// Blob GETs answer 200 with the whole blob even when `Range` is sent.
    pub ignore_range: bool,
}

/// Takes one failure off a one-shot counter, if any is left.
fn take_failure(slot: &mut Option<(u16, usize)>) -> Option<u16> {
    let (status, n) = (*slot)?;
    *slot = (n > 1).then_some((status, n - 1));
    Some(status)
}

#[derive(Clone)]
struct Shared {
    base: String,
    token: String,
    state: Arc<Mutex<MockState>>,
}

pub struct GhaMock {
    /// The `ACTIONS_RESULTS_URL` to use (ends with `/`).
    pub url: String,
    pub token: String,
    pub state: Arc<Mutex<MockState>>,
}

impl GhaMock {
    pub fn requests(&self) -> Vec<MockRequest> {
        self.state.lock().unwrap().requests.clone()
    }
    pub fn entries(&self) -> Vec<MockEntry> {
        self.state.lock().unwrap().entries.clone()
    }
    pub fn fail_with(&self, status: u16) {
        self.state.lock().unwrap().fail_status = Some(status);
    }
    /// Fails only the next `n` Twirp calls with `status`.
    pub fn fail_next_twirp(&self, status: u16, n: usize) {
        self.state.lock().unwrap().fail_twirp_next = Some((status, n));
    }
    /// Fails only the next `n` blob GETs with `status`.
    pub fn fail_next_blob_get(&self, status: u16, n: usize) {
        self.state.lock().unwrap().fail_blob_get_next = Some((status, n));
    }
    /// Makes blob GETs ignore `Range` (answer 200 with the whole blob).
    pub fn ignore_range(&self, on: bool) {
        self.state.lock().unwrap().ignore_range = on;
    }
}

fn header(h: &HeaderMap, name: &str) -> Option<String> {
    h.get(name).and_then(|v| v.to_str().ok()).map(String::from)
}

fn query_param(q: &str, name: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| {
            v.replace("%2B", "+")
                .replace("%2F", "/")
                .replace("%3D", "=")
        })
    })
}

async fn twirp(
    State(s): State<Shared>,
    Path(method): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut st = s.state.lock().unwrap();
    st.requests.push(MockRequest {
        method: "POST".into(),
        path: format!("/twirp/github.actions.results.api.v1.CacheService/{method}"),
        query: String::new(),
        authorization: header(&headers, "authorization"),
        x_ms_version: None,
        range: None,
        json: Some(json.clone()),
    });
    let code = take_failure(&mut st.fail_twirp_next).or(st.fail_status);
    if let Some(code) = code {
        return (StatusCode::from_u16(code).unwrap(), "{\"code\":\"x\"}").into_response();
    }
    if header(&headers, "authorization") != Some(format!("Bearer {}", s.token)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let str_of = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let key = str_of(&json["key"]);
    let version = str_of(&json["version"]);
    let signed = |blob: &str| format!("{}blob/{blob}?sv=2025-01-05&sig={MOCK_SIG}", s.base);
    let out = match method.as_str() {
        "CreateCacheEntry" => {
            if st
                .entries
                .iter()
                .any(|e| e.key == key && e.version == version)
            {
                json!({"ok": false})
            } else {
                let blob = format!("b{}", st.entries.len());
                st.entries.push(MockEntry {
                    key,
                    version,
                    blob: blob.clone(),
                    finalized: false,
                    size: None,
                });
                json!({"ok": true, "signed_upload_url": signed(&blob)})
            }
        }
        "FinalizeCacheEntryUpload" => {
            let size = match &json["size_bytes"] {
                Value::String(s) => s.parse().ok(),
                v => v.as_u64(),
            };
            let blobs = st.blobs.clone();
            match st
                .entries
                .iter_mut()
                .find(|e| e.key == key && e.version == version)
            {
                Some(e) if blobs.get(&e.blob).map(|b| b.len() as u64) == size => {
                    e.finalized = true;
                    e.size = size;
                    json!({"ok": true, "entry_id": "1"})
                }
                _ => json!({"ok": false}),
            }
        }
        "GetCacheEntryDownloadURL" => {
            let restore: Vec<String> = json["restore_keys"]
                .as_array()
                .map(|a| a.iter().map(str_of).collect())
                .unwrap_or_default();
            let live = |e: &&MockEntry| e.finalized && e.version == version;
            let exact = st.entries.iter().filter(live).find(|e| e.key == key);
            let found = exact.or_else(|| {
                restore.iter().find_map(|r| {
                    st.entries
                        .iter()
                        .filter(live)
                        .rev()
                        .find(|e| e.key.starts_with(r.as_str()))
                })
            });
            match found {
                Some(e) => json!({
                    "ok": true,
                    "signed_download_url": signed(&e.blob),
                    "matched_key": e.key,
                }),
                None => json!({"ok": false}),
            }
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    axum::Json(out).into_response()
}

fn record_blob(st: &mut MockState, method: &str, id: &str, query: &str, headers: &HeaderMap) {
    st.requests.push(MockRequest {
        method: method.into(),
        path: format!("/blob/{id}"),
        query: query.into(),
        authorization: header(headers, "authorization"),
        x_ms_version: header(headers, "x-ms-version"),
        range: header(headers, "range"),
        json: None,
    });
}

async fn blob_put(
    State(s): State<Shared>,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let q = q.unwrap_or_default();
    let mut st = s.state.lock().unwrap();
    record_blob(&mut st, "PUT", &id, &q, &headers);
    if query_param(&q, "sig").as_deref() != Some(MOCK_SIG) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match query_param(&q, "comp").as_deref() {
        Some("block") => {
            let Some(block) = query_param(&q, "blockid") else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            st.blocks
                .entry(id)
                .or_default()
                .insert(block, body.to_vec());
            StatusCode::CREATED.into_response()
        }
        Some("blocklist") => {
            let xml = String::from_utf8_lossy(&body).to_string();
            let mut data = Vec::new();
            let blocks = st.blocks.remove(&id).unwrap_or_default();
            for part in xml.split("<Latest>").skip(1) {
                let Some((bid, _)) = part.split_once("</Latest>") else {
                    return StatusCode::BAD_REQUEST.into_response();
                };
                let Some(b) = blocks.get(bid) else {
                    return StatusCode::BAD_REQUEST.into_response();
                };
                data.extend_from_slice(b);
            }
            st.blobs.insert(id, data);
            StatusCode::CREATED.into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn blob_get(
    State(s): State<Shared>,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
) -> Response {
    let q = q.unwrap_or_default();
    let mut st = s.state.lock().unwrap();
    record_blob(&mut st, "GET", &id, &q, &headers);
    if query_param(&q, "sig").as_deref() != Some(MOCK_SIG) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(code) = take_failure(&mut st.fail_blob_get_next) {
        return StatusCode::from_u16(code).unwrap().into_response();
    }
    let Some(blob) = st.blobs.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(range) = header(&headers, "range").filter(|_| !st.ignore_range) else {
        return blob.clone().into_response();
    };
    // `bytes=a-b` (inclusive); `b` is clamped to the blob like Azure does.
    let parsed = range
        .strip_prefix("bytes=")
        .and_then(|r| r.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
    let total = blob.len();
    match parsed {
        Some((a, b)) if a <= b && a < total => {
            let b = b.min(total - 1);
            (
                StatusCode::PARTIAL_CONTENT,
                [("content-range", format!("bytes {a}-{b}/{total}"))],
                blob[a..=b].to_vec(),
            )
                .into_response()
        }
        _ => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [("content-range", format!("bytes */{total}"))],
        )
            .into_response(),
    }
}

/// Starts the mock on `127.0.0.1:<random>`; requests must carry `Bearer <token>`.
pub async fn spawn(token: &str) -> GhaMock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let state = Arc::new(Mutex::new(MockState::default()));
    let shared = Shared {
        base: base.clone(),
        token: token.into(),
        state: state.clone(),
    };
    let app = Router::new()
        .route(
            "/twirp/github.actions.results.api.v1.CacheService/{method}",
            post(twirp),
        )
        .route("/blob/{id}", put(blob_put).get(blob_get))
        .layer(DefaultBodyLimit::disable())
        .with_state(shared);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    GhaMock {
        url: base,
        token: token.into(),
        state,
    }
}
