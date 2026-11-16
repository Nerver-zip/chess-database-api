//! Unified API error domain models, HTTP status code mapping, and JSON serialization.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// High-level API domain errors mapped to stable, machine-readable JSON error contracts.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error(
        "Missing required query parameter 'collection'. Expected 'masters-otb' or 'elite-online'."
    )]
    MissingCollection,

    #[error("Invalid collection identifier '{0}'. Expected 'masters-otb' or 'elite-online'.")]
    InvalidCollection(String),

    #[error("Collection '{0}' is not configured or loaded on this server.")]
    CollectionUnavailable(String),

    #[error("Dataset snapshot for collection '{0}' does not contain a published position index (positions.cpi).")]
    IndexUnavailable(String),

    #[error(
        "Missing position representation. Exactly one of 'fen' or 'moves' parameter is required."
    )]
    MissingPosition,

    #[error("Ambiguous position representation. Specify either 'fen' or 'moves', not both.")]
    AmbiguousPosition,

    #[error("Invalid FEN syntax: {0}")]
    InvalidFen(String),

    #[error("Invalid or illegal move sequence: {0}")]
    InvalidMoves(String),

    #[error("Move sequence exceeds maximum allowed plies ({0} > {1}).")]
    MoveSequenceTooLong(usize, usize),

    #[error("Query parameter '{0}' is not supported. Year/rating filtering is not supported by the CPI index.")]
    UnsupportedParameter(String),

    /// The request could not be decoded (for example, a repeated query parameter or
    /// invalid percent-encoding). The text is the decoder's description of the problem.
    #[error("{0}")]
    InvalidQuery(String),

    #[error("Invalid game ID '{0}'. Expected a 64-bit unsigned integer.")]
    InvalidGameId(String),

    #[error("Game ID {0} was not found in collection '{1}'.")]
    GameNotFound(u64, String),

    #[error("No resource exists at this path.")]
    NotFound,

    #[error("The HTTP method is not allowed for this path.")]
    MethodNotAllowed,

    #[error("The request did not complete within the server's request timeout.")]
    RequestTimeout,

    #[error("Unauthorized: valid administrative authorization token required.")]
    Unauthorized,

    #[error("Administrative reload via HTTP is disabled. Configure --admin-token or use local SIGHUP signal.")]
    AdminDisabled,

    #[error("Reader pool timeout: server is busy and cannot allocate a reader handle.")]
    PoolTimeout(String),

    #[error("Administrative reload failed: {0}")]
    ReloadFailed(String),

    #[error("Storage engine error: {0}")]
    Storage(String),

    #[error("Internal server error: {0}")]
    Internal(String),
}

/// Standard error container payload returned by all error responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorEnvelope {
    pub error: ErrorDetails,
}

/// Machine-readable error details.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorDetails {
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// Returns the stable, machine-readable error code.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::MissingCollection | Self::InvalidCollection(_) => "invalid_collection",
            Self::CollectionUnavailable(_) => "collection_unavailable",
            Self::IndexUnavailable(_) => "index_unavailable",
            Self::MissingPosition => "missing_position",
            Self::AmbiguousPosition => "ambiguous_position",
            Self::InvalidFen(_) => "invalid_fen",
            Self::InvalidMoves(_) => "invalid_moves",
            Self::MoveSequenceTooLong(_, _) => "move_sequence_too_long",
            Self::UnsupportedParameter(_) => "unsupported_parameter",
            Self::InvalidQuery(_) => "invalid_query",
            Self::InvalidGameId(_) => "invalid_game_id",
            Self::GameNotFound(_, _) => "game_not_found",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::RequestTimeout => "request_timeout",
            Self::Unauthorized => "unauthorized",
            Self::AdminDisabled => "admin_disabled",
            Self::PoolTimeout(_) => "reader_pool_timeout",
            Self::ReloadFailed(_) => "reload_failed",
            Self::Storage(_) => "storage_error",
            Self::Internal(_) => "internal_error",
        }
    }

    /// Returns the associated HTTP status code.
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::MissingCollection
            | Self::InvalidCollection(_)
            | Self::MissingPosition
            | Self::AmbiguousPosition
            | Self::InvalidFen(_)
            | Self::InvalidMoves(_)
            | Self::MoveSequenceTooLong(_, _)
            | Self::UnsupportedParameter(_)
            | Self::InvalidQuery(_)
            | Self::InvalidGameId(_) => StatusCode::BAD_REQUEST,

            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::AdminDisabled => StatusCode::FORBIDDEN,
            Self::GameNotFound(_, _) | Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::RequestTimeout => StatusCode::REQUEST_TIMEOUT,

            Self::CollectionUnavailable(_) | Self::IndexUnavailable(_) | Self::PoolTimeout(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }

            Self::ReloadFailed(_) | Self::Storage(_) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        if status.is_server_error() {
            // Log the full diagnostic on the server side.
            tracing::error!(
                target: "opening_explorer_api::error",
                code = %self.error_code(),
                diagnostic = %self,
                "Internal server error"
            );
        }

        // Only 500 responses hide their detail: the diagnostic may contain filesystem
        // paths or engine internals. Other errors, including 503 availability messages,
        // describe the client-visible condition and are returned as written.
        let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
            match &self {
                Self::ReloadFailed(_) => {
                    "Administrative reload failed: replacement snapshot integrity check was rejected."
                }
                Self::Storage(_) => "A storage engine error occurred.",
                _ => "An internal server error occurred.",
            }
            .to_string()
        } else {
            self.to_string()
        };

        let payload = ErrorEnvelope {
            error: ErrorDetails {
                code: self.error_code().to_string(),
                message,
            },
        };
        (status, Json(payload)).into_response()
    }
}
