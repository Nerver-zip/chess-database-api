//! Collection metadata endpoint (`GET /v1/collections`).

use crate::models::CollectionsResponse;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

/// Handler for `GET /v1/collections`.
pub async fn list_collections(State(state): State<Arc<AppState>>) -> Json<CollectionsResponse> {
    let collections = state.available_collections();
    Json(CollectionsResponse { collections })
}
