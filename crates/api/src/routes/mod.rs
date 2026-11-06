//! Router composition, endpoint wiring, and HTTP middleware pipeline.

pub mod admin;
pub mod collections;
pub mod explorer;
pub mod games;
pub mod health;

use crate::state::AppState;
use axum::http::{header, HeaderValue, Method};
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Builds the production Axum HTTP router with all endpoints and middleware layers.
pub fn create_router(state: Arc<AppState>) -> Router {
    // 1. Configure CORS
    let cors = if state.config.allowed_origins.iter().any(|o| o == "*") {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS, Method::HEAD])
            .allow_headers([header::CONTENT_TYPE, header::ACCEPT, header::AUTHORIZATION])
    } else {
        let valid_origins: Vec<HeaderValue> = state
            .config
            .allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(valid_origins)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS, Method::HEAD])
            .allow_headers([header::CONTENT_TYPE, header::ACCEPT, header::AUTHORIZATION])
    };

    // 2. Configure Timeout
    let timeout = TimeoutLayer::with_status_code(
        axum::http::StatusCode::REQUEST_TIMEOUT,
        Duration::from_secs(state.config.request_timeout_secs),
    );

    // 3. Assemble routes
    Router::new()
        // Health and readiness probes
        .route("/health", get(health::health_check))
        .route("/ready", get(health::readiness_check))
        // Collection and game retrieval
        .route("/v1/collections", get(collections::list_collections))
        .route(
            "/v1/collections/{collection}/games/{id}",
            get(games::get_game_by_id),
        )
        // Opening explorer position queries
        .route("/v1/explorer/position", get(explorer::query_position))
        // Admin snapshot reload
        .route("/v1/admin/reload", post(admin::reload_snapshots))
        // Middleware layers
        .layer(cors)
        .layer(timeout)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
