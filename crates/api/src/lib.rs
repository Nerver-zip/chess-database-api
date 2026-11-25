//! Read-only HTTP service for Chess Database API.
//!
//! Serves Opening Explorer statistics from CPI v2 and canonical PGNs from
//! CGB3 archives. Enriched snapshots add game, player, and event discovery
//! and filtered Opening Explorer statistics.

pub mod chess_util;
pub mod config;
pub mod error;
pub mod models;
pub mod routes;
mod search_runtime;
pub mod server;
pub mod state;

pub use config::ServerConfig;
pub use error::{ApiError, ErrorDetails, ErrorEnvelope};
pub use models::*;
pub use routes::create_router;
pub use search_runtime::SearchOptions;
pub use server::{run_server, run_server_with_search};
pub use state::AppState;

/// Returns the current crate package version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
