//! Configuration for the Opening Explorer HTTP API server.

use std::path::PathBuf;

/// Complete configuration for the `codb` HTTP server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Filesystem path to the Masters OTB dataset root directory (containing `current` or version snapshot).
    pub masters_dataset: Option<PathBuf>,
    /// Filesystem path to the Elite Online dataset root directory (containing `current` or version snapshot).
    pub elite_dataset: Option<PathBuf>,
    /// Network host/interface to bind the HTTP listener (default: `127.0.0.1`).
    pub host: String,
    /// Network port to listen on (default: `8080`).
    pub port: u16,
    /// Allowed CORS origins (e.g. `["*"]` or `["http://localhost:3000"]`).
    pub allowed_origins: Vec<String>,
    /// Global request timeout in seconds (default: 15 seconds).
    pub request_timeout_secs: u64,
    /// Maximum allowed plies in a move sequence query parameter (default: 200, maximum: 512).
    pub max_move_plies: usize,
    /// Maximum capacity of the reader pool per dataset snapshot (default: 16, maximum: 64).
    pub reader_pool_size: usize,
    /// Maximum time in seconds to wait for an available reader from the pool (default: 5, maximum: 60).
    pub reader_pool_timeout_secs: u64,
    /// Whether `GET /v1/collections/{collection}/games/{id}` may return stored PGN.
    pub enable_game_retrieval: bool,
    /// Deeply verifies every stored block before the service becomes ready.
    pub deep_validate_on_startup: bool,
    /// Secret administrative authorization token required for `POST /v1/admin/reload`.
    /// When None, HTTP reload is disabled (403 Forbidden).
    pub admin_token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            masters_dataset: None,
            elite_dataset: None,
            host: "127.0.0.1".to_string(),
            port: 8080,
            allowed_origins: Vec::new(), // Restrictive by default (no wildcard)
            request_timeout_secs: 15,
            max_move_plies: 200,
            reader_pool_size: 16,
            reader_pool_timeout_secs: 5,
            enable_game_retrieval: true,
            deep_validate_on_startup: false,
            admin_token: None,
        }
    }
}

impl ServerConfig {
    /// Validates configuration parameters.
    pub fn validate(&self) -> Result<(), String> {
        if self.masters_dataset.is_none() && self.elite_dataset.is_none() {
            return Err(
                "At least one dataset (--masters-dataset or --elite-dataset) must be configured"
                    .to_string(),
            );
        }
        if self.port == 0 {
            return Err("Server port cannot be 0".to_string());
        }
        if self.request_timeout_secs == 0 || self.request_timeout_secs > 300 {
            return Err("Request timeout must be between 1 and 300 seconds".to_string());
        }
        if self.max_move_plies == 0 || self.max_move_plies > 512 {
            return Err("Maximum move plies must be between 1 and 512".to_string());
        }
        if self.reader_pool_size == 0 || self.reader_pool_size > 64 {
            return Err("Reader pool size must be between 1 and 64".to_string());
        }
        if self.reader_pool_timeout_secs == 0 || self.reader_pool_timeout_secs > 60 {
            return Err("Reader pool timeout must be between 1 and 60 seconds".to_string());
        }
        if let Some(token) = &self.admin_token {
            if token.trim().is_empty() {
                return Err(
                    "Administrative token (--admin-token / CODB_ADMIN_TOKEN) cannot be empty or whitespace-only"
                        .to_string(),
                );
            }
        }
        Ok(())
    }
}
