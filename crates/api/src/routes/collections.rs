//! Collection metadata endpoint (`GET /v1/collections`).

use crate::models::CollectionsResponse;
use crate::state::AppState;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::sync::Arc;

/// Handler for `GET /v1/collections`.
pub async fn list_collections(State(state): State<Arc<AppState>>) -> Response {
    let collections = state.available_collections();
    (
        [(header::CACHE_CONTROL, "public, max-age=60")],
        Json(CollectionsResponse { collections }),
    )
        .into_response()
}
