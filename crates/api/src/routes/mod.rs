//! Router composition, endpoint wiring, and HTTP middleware pipeline.

pub mod admin;
pub mod collections;
pub mod explorer;
pub mod games;
pub mod health;

use crate::error::ApiError;
use crate::state::AppState;
use axum::body::to_bytes;
use axum::extract::Request;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Largest framework rejection body read in order to build its error message.
const MAX_REJECTION_BODY_BYTES: usize = 1024;

/// Builds the production Axum HTTP router with all endpoints and middleware layers.
///
/// The request timeout is taken from `ServerConfig::request_timeout_secs`.
pub fn create_router(state: Arc<AppState>) -> Router {
    let request_timeout = Duration::from_secs(state.config.request_timeout_secs);
    create_router_with_request_timeout(state, request_timeout)
}

/// Builds the router with an explicit request timeout for read endpoints.
///
/// `POST /v1/admin/reload` is not subject to this timeout. See [`admin::reload_snapshots`].
pub fn create_router_with_request_timeout(
    state: Arc<AppState>,
    request_timeout: Duration,
) -> Router {
    // 1. Configure CORS
    let cors = if state.config.allowed_origins.iter().any(|o| o == "*") {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS, Method::HEAD])
            .allow_headers([
                header::CONTENT_TYPE,
                header::ACCEPT,
                header::AUTHORIZATION,
                header::IF_NONE_MATCH,
                header::IF_MATCH,
            ])
            .expose_headers([header::ETAG, header::CONTENT_TYPE, header::CACHE_CONTROL])
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
            .allow_headers([
                header::CONTENT_TYPE,
                header::ACCEPT,
                header::AUTHORIZATION,
                header::IF_NONE_MATCH,
                header::IF_MATCH,
            ])
            .expose_headers([header::ETAG, header::CONTENT_TYPE, header::CACHE_CONTROL])
    };

    // 2. Read endpoints are bounded by the request timeout.
    let timeout = TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, request_timeout);
    // Some restricted dataset licenses prohibit returning raw PGN to users.
    let enable_game_retrieval = state.config.enable_game_retrieval;
    let mut reads = Router::new()
        // Health and readiness probes
        .route("/health", get(health::health_check))
        .route("/ready", get(health::readiness_check))
        // Collection metadata
        .route("/v1/collections", get(collections::list_collections))
        // Opening explorer position queries
        .route("/v1/explorer/position", get(explorer::query_position));
    if enable_game_retrieval {
        reads = reads.route(
            "/v1/collections/{collection}/games/{id}",
            get(games::get_game_by_id),
        );
    }
    let reads = reads.layer(timeout);

    // 3. The administrative reload is exempt from the request timeout: verifying a large
    // snapshot can take longer than a client waits. The work runs in its own task.
    let admin = Router::new().route("/v1/admin/reload", post(admin::reload_snapshots));

    reads
        .merge(admin)
        .fallback(|| async { ApiError::NotFound })
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        // Outermost: framework rejections from any layer get the error envelope.
        .layer(middleware::from_fn(envelope_framework_rejections))
        .with_state(state)
}

/// Gives framework-generated rejections the standard JSON error envelope.
///
/// Axum, tower-http, and the query extractor produce these responses without a body or
/// with plain text. Each one keeps its status code and headers (such as `Allow`), and gains
/// an `error.code` and `error.message`. Responses that are already JSON pass through.
async fn envelope_framework_rejections(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let status = response.status();
    let is_rejection = matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::NOT_FOUND
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::REQUEST_TIMEOUT
    );
    if !is_rejection || is_json(&response) {
        return response;
    }

    let (parts, body) = response.into_parts();
    let detail = match to_bytes(body, MAX_REJECTION_BODY_BYTES).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).trim().to_string(),
        Err(_) => String::new(),
    };
    let error = match status {
        StatusCode::BAD_REQUEST => ApiError::InvalidQuery(if detail.is_empty() {
            "The query string could not be decoded.".to_string()
        } else {
            detail
        }),
        StatusCode::NOT_FOUND => ApiError::NotFound,
        StatusCode::METHOD_NOT_ALLOWED => ApiError::MethodNotAllowed,
        _ => ApiError::RequestTimeout,
    };

    let mut replacement = error.into_response();
    for (name, value) in parts.headers.iter() {
        if name != CONTENT_TYPE
            && name != CONTENT_LENGTH
            && !replacement.headers().contains_key(name)
        {
            replacement
                .headers_mut()
                .append(name.clone(), value.clone());
        }
    }
    replacement
}

fn is_json(response: &Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// The envelope wraps every framework rejection, whichever layer produced it.
    #[tokio::test]
    async fn framework_rejections_are_wrapped_with_status_and_headers_kept() {
        let app = Router::new()
            .route("/timeout", get(|| async { StatusCode::REQUEST_TIMEOUT }))
            .route(
                "/query",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        "Failed to deserialize query string: duplicate field `moves`\n",
                    )
                }),
            )
            .route(
                "/empty-bad-request",
                get(|| async { StatusCode::BAD_REQUEST }),
            )
            .route("/allowed", get(|| async { "ok" }))
            .layer(middleware::from_fn(envelope_framework_rejections));

        let cases = [
            ("/timeout", StatusCode::REQUEST_TIMEOUT, "request_timeout"),
            ("/query", StatusCode::BAD_REQUEST, "invalid_query"),
            (
                "/empty-bad-request",
                StatusCode::BAD_REQUEST,
                "invalid_query",
            ),
        ];
        for (uri, expected_status, expected_code) in cases {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected_status, "{}", uri);
            assert_eq!(
                response.headers()[CONTENT_TYPE],
                "application/json",
                "{}",
                uri
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let envelope: crate::error::ErrorEnvelope = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(envelope.error.code, expected_code, "{}", uri);
            assert!(!envelope.error.message.is_empty(), "{}", uri);
        }

        // Successful responses are not touched.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/allowed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"ok");
    }
}
