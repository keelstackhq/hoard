//! Bearer-token check for every route except `/_hoard/health`.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

use super::AppState;

/// Rejects the request with `401` unless it carries `Authorization: Bearer <token>`.
pub async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if authorized(req.headers(), &state.token) {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Constant-time comparison of the bearer token (a length mismatch is a plain reject).
pub fn authorized(headers: &HeaderMap, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let Some(given) = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.as_bytes().strip_prefix(b"Bearer "))
    else {
        return false;
    };
    given.ct_eq(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, value.parse().unwrap());
        h
    }

    #[test]
    fn bearer_check() {
        assert!(authorized(&with("Bearer s3cret"), "s3cret"));
        assert!(!authorized(&with("Bearer s3cre"), "s3cret"));
        assert!(!authorized(&with("Bearer s3crets"), "s3cret"));
        assert!(!authorized(&with("Bearer S3CRET"), "s3cret"));
        assert!(!authorized(&with("Basic s3cret"), "s3cret"));
        assert!(!authorized(&with("s3cret"), "s3cret"));
        assert!(!authorized(&HeaderMap::new(), "s3cret"));
        assert!(!authorized(&with("Bearer "), ""));
    }
}
