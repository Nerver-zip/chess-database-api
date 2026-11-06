//! `codb-api`: Production Opening Explorer HTTP API service.
//!
//! Provides high-throughput, low-latency, read-only HTTP endpoints exposing
//! persistent CGB3 game archives and CPI v2 position opening indexes.

pub mod chess_util;
pub mod config;
pub mod error;
pub mod models;
pub mod routes;
pub mod server;
pub mod state;

pub use config::ServerConfig;
pub use error::{ApiError, ErrorDetails, ErrorEnvelope};
pub use models::*;
pub use routes::create_router;
pub use server::run_server;
pub use state::AppState;

/// Returns the current crate package version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
