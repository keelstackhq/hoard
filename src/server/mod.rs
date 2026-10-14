//! axum router: Turborepo `/v8` API behind bearer auth, plus `/_hoard/*` routes.

mod auth;
pub(crate) mod body;
mod turbo;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};

use crate::config::Mode;
use crate::ratelimit::RateGate;
use crate::stats::{Stats, StatsSnapshot};
use crate::store::Store;
use crate::writebehind::WriteBehind;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub stats: Arc<Stats>,
    pub token: String,
    pub mode: Mode,
    /// GET/HEAD and the batch query miss without reading the store (`--no-reads`).
    pub no_reads: bool,
    /// Closed for a while after the backend rate-limits an upload.
    pub rate_gate: Arc<RateGate>,
    pub prefix: String,
    pub cache_version: String,
    /// Set when PUTs are spooled and uploaded in the background (`write-behind`).
    pub write_behind: Option<Arc<WriteBehind>>,
    /// Set for `storage=r2` with `refresh-on-hit`: a GET hit may reset the object's age.
    pub refresh: Option<Arc<crate::r2::refresh::Refresher>>,
    /// `POST /_hoard/shutdown` sends a reply channel here and answers once the shutdown work
    /// (drain, prune, summary) has replied with the final stats, or the receiver is gone.
    pub shutdown: tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<StatsSnapshot>>,
}

impl AppState {
    /// Storage key of a validated Turborepo hash.
    fn turbo_key(&self, hash: &str) -> String {
        crate::key::artifact_key(&self.prefix, "turbo", &self.cache_version, hash)
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v8/artifacts/status", get(turbo::status))
        .route("/v8/artifacts/events", post(turbo::events))
        .route("/v8/artifacts", post(turbo::query))
        .route(
            "/v8/artifacts/{hash}",
            get(turbo::get_artifact)
                .head(turbo::head_artifact)
                .put(turbo::put_artifact)
                // Artifacts can be hundreds of MB; the body is streamed, never buffered.
                .layer(DefaultBodyLimit::disable()),
        )
        .route("/_hoard/stats", get(stats))
        .route("/_hoard/shutdown", post(shutdown))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        // Added after the auth layer, so it is the only unauthenticated route.
        .route("/_hoard/health", get(|| async { "ok" }))
        .with_state(state)
}

async fn stats(State(s): State<AppState>) -> Json<StatsSnapshot> {
    Json(s.stats.snapshot())
}

/// Answers with the final stats once the shutdown work is done (after the drain, so they
/// include background uploads). A closed channel or a dropped reply means shutdown is already
/// done or under way: then the current stats.
async fn shutdown(State(s): State<AppState>) -> Json<StatsSnapshot> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if s.shutdown.send(tx).await.is_ok()
        && let Ok(snapshot) = rx.await
    {
        return Json(snapshot);
    }
    Json(s.stats.snapshot())
}
