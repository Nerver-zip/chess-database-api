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
/// Requires an administrative bearer token configured via `--admin-token` or `CODB_ADMIN_TOKEN`.
/// If no token is configured, the endpoint fails closed with HTTP 403 Forbidden.
pub async fn reload_snapshots(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AdminReloadResponse>, ApiError> {
    // 1. Verify administrative reload via HTTP is enabled
    let expected_token = state
        .config
        .admin_token
        .as_deref()
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
            token == expected_token
        }
        (_, Some(token)) => token.trim() == expected_token,
        _ => false,
    };

    if !is_authorized {
        return Err(ApiError::Unauthorized);
    }

    // 3. Execute reload with full candidate snapshot integrity verification
    let reloaded = state.reload().await?;

    Ok(Json(AdminReloadResponse {
        status: "ok".to_string(),
        reloaded_collections: reloaded,
    }))
}
