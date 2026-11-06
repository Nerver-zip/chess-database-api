//! Administration endpoint for zero-downtime snapshot refreshing (`POST /v1/admin/reload`).

use crate::error::ApiError;
use crate::models::AdminReloadResponse;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

/// Handler for `POST /v1/admin/reload`.
pub async fn reload_snapshots(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AdminReloadResponse>, ApiError> {
    let reloaded = state
        .reload()
        .map_err(|e| ApiError::Internal(format!("Failed to reload datasets: {}", e)))?;

    Ok(Json(AdminReloadResponse {
        status: "ok".to_string(),
        reloaded_collections: reloaded,
    }))
}
