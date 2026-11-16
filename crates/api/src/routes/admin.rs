//! Administration endpoint for secure, zero-downtime snapshot refreshing (`POST /v1/admin/reload`).

use crate::error::ApiError;
use crate::models::AdminReloadResponse;
use crate::state::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::Json;
use std::sync::Arc;

/// Handler for `POST /v1/admin/reload`.
///
/// Requires an administrative bearer token configured via `--admin-token` or `OPENING_EXPLORER_ADMIN_TOKEN`.
/// If no token is configured, the endpoint fails closed with HTTP 403 Forbidden.
///
/// The route is outside the read-endpoint request timeout. The reload runs in a spawned task,
/// so a long verification finishes and swaps atomically even if the caller stops waiting.
pub async fn reload_snapshots(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AdminReloadResponse>, ApiError> {
    // 1. Verify administrative reload via HTTP is enabled
    let expected_token = state
        .config
        .admin_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or(ApiError::AdminDisabled)?;

    // 2. Reject cross-origin browser triggers for administrative operations
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        tracing::warn!(
            "Rejected cross-origin attempt to trigger administrative reload from origin: {}",
            origin
        );
        return Err(ApiError::Unauthorized);
    }

    // 3. Validate token from Authorization: Bearer <token> or X-Admin-Token
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let x_admin_header = headers.get("x-admin-token").and_then(|v| v.to_str().ok());

    let is_authorized = match (auth_header, x_admin_header) {
        (Some(auth), _) if auth.starts_with("Bearer ") => {
            let token = auth.trim_start_matches("Bearer ").trim();
            !token.is_empty() && token == expected_token
        }
        (_, Some(token)) => {
            let token = token.trim();
            !token.is_empty() && token == expected_token
        }
        _ => false,
    };

    if !is_authorized {
        return Err(ApiError::Unauthorized);
    }

    // 4. Execute reload with full candidate snapshot integrity verification, in its own task
    let worker = Arc::clone(&state);
    let reloaded = tokio::spawn(async move { worker.reload().await })
        .await
        .map_err(|e| ApiError::Internal(format!("Reload task failed: {}", e)))??;

    Ok(Json(AdminReloadResponse {
        status: "ok".to_string(),
        reloaded_collections: reloaded,
    }))
}
