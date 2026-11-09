//! Liveness and readiness endpoints (`GET /health`, `GET /ready`).

use crate::models::{HealthResponse, ReadyResponse};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::sync::Arc;

/// Lightweight liveness probe (`GET /health`).
pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Readiness probe (`GET /ready`).
///
/// Returns 200 OK if at least one dataset collection is loaded and ready to serve queries,
/// or 503 Service Unavailable if no collections are currently active.
pub async fn readiness_check(State(state): State<Arc<AppState>>) -> Response {
    let ready = state.ready_collections();
    if ready.is_empty() {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyResponse {
                status: "not_ready".to_string(),
                collections_loaded: vec![],
            }),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            Json(ReadyResponse {
                status: "ready".to_string(),
                collections_loaded: ready,
            }),
        )
            .into_response()
    }
}
