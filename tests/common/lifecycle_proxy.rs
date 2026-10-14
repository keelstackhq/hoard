//! A test-only S3 proxy in front of MinIO that keeps bucket lifecycle configurations verbatim.
//!
//! MinIO accepts `AbortIncompleteMultipartUpload` in a lifecycle rule but silently drops it,
//! so a rule read back from MinIO never matches hoard's rule. R2 stores it. This proxy answers
//! `GET`/`PUT /{bucket}?lifecycle` itself (storing the body as sent, without auth checks) and
//! forwards every other request to MinIO unchanged. The original `Host` header is kept, so
//! MinIO accepts the SigV4 signature made for the proxy's address.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// Largest lifecycle configuration accepted (S3 allows 1,000 rules; this is plenty).
const MAX_LIFECYCLE_BODY: usize = 1 << 20;

/// The stored lifecycle configurations, by bucket: raw XML exactly as last PUT (or seeded).
#[derive(Clone, Default)]
pub struct Lifecycles(Arc<Mutex<HashMap<String, Bytes>>>);

impl Lifecycles {
    pub fn get(&self, bucket: &str) -> Option<String> {
        let map = self.0.lock().unwrap();
        map.get(bucket)
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
    }
    pub fn set(&self, bucket: &str, xml: &str) {
        let mut map = self.0.lock().unwrap();
        map.insert(bucket.to_string(), Bytes::from(xml.to_string()));
    }
}

#[derive(Clone)]
struct Proxy {
    upstream: String,
    client: reqwest::Client,
    lifecycles: Lifecycles,
}

/// Starts the proxy for `upstream` (e.g. `http://127.0.0.1:9000`) on the current runtime and
/// returns its base URL and its lifecycle store. Call it inside `s3_runtime().block_on(..)`.
pub async fn spawn(upstream: &str) -> (String, Lifecycles) {
    let lifecycles = Lifecycles::default();
    let proxy = Proxy {
        upstream: upstream.trim_end_matches('/').to_string(),
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        lifecycles: lifecycles.clone(),
    };
    let app = axum::Router::new().fallback(handle).with_state(proxy);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), lifecycles)
}

/// Hop-by-hop and length headers the HTTP stacks set themselves.
fn skip(name: &header::HeaderName) -> bool {
    name == header::CONTENT_LENGTH
        || name == header::TRANSFER_ENCODING
        || name == header::CONNECTION
}

fn is_lifecycle(req: &Request) -> Option<String> {
    let query = req.uri().query()?;
    let lifecycle = query
        .split('&')
        .any(|p| p == "lifecycle" || p.starts_with("lifecycle="));
    let bucket = req.uri().path().trim_matches('/');
    (lifecycle && !bucket.is_empty() && !bucket.contains('/')).then(|| bucket.to_string())
}

async fn handle(State(p): State<Proxy>, req: Request) -> Response {
    if let Some(bucket) = is_lifecycle(&req) {
        return lifecycle(&p, &bucket, req).await;
    }
    forward(&p, req).await
}

async fn lifecycle(p: &Proxy, bucket: &str, req: Request) -> Response {
    match *req.method() {
        Method::GET => match p.lifecycles.0.lock().unwrap().get(bucket) {
            Some(xml) => (StatusCode::OK, [(header::CONTENT_TYPE, "application/xml")], xml.clone())
                .into_response(),
            None => (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "application/xml")],
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>NoSuchLifecycleConfiguration</Code>\
                     <Message>The lifecycle configuration does not exist</Message><BucketName>{bucket}</BucketName></Error>"
                ),
            )
                .into_response(),
        },
        Method::PUT => {
            let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_LIFECYCLE_BODY).await else {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            };
            let mut map = p.lifecycles.0.lock().unwrap();
            map.insert(bucket.to_string(), body);
            StatusCode::OK.into_response()
        }
        Method::DELETE => {
            p.lifecycles.0.lock().unwrap().remove(bucket);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

async fn forward(p: &Proxy, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let path_q = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let mut headers = HeaderMap::new();
    for (k, v) in &parts.headers {
        if !skip(k) {
            headers.append(k, v.clone());
        }
    }
    // Test proxy: buffering is fine here (small objects only).
    let Ok(body) = axum::body::to_bytes(body, usize::MAX).await else {
        return (StatusCode::BAD_REQUEST, "cannot read request body").into_response();
    };
    let mut up = p
        .client
        .request(parts.method.clone(), format!("{}{path_q}", p.upstream))
        .headers(headers);
    if !body.is_empty() || parts.headers.contains_key(header::CONTENT_LENGTH) {
        up = up.body(body);
    }
    let res = match up.send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    let mut out = Response::builder().status(res.status());
    for (k, v) in res.headers() {
        if !skip(k) {
            out = out.header(k, v);
        }
    }
    let is_head = parts.method == Method::HEAD;
    let len = res.headers().get(header::CONTENT_LENGTH).cloned();
    let bytes = match res.bytes().await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    if is_head {
        // HEAD keeps the upstream length; there is no body to measure.
        if let Some(len) = len {
            out = out.header(header::CONTENT_LENGTH, len);
        }
        return out.body(Body::empty()).unwrap();
    }
    out.body(Body::from(bytes)).unwrap()
}
