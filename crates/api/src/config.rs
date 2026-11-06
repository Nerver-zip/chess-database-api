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
    /// Maximum allowed plies in a move sequence query parameter (default: 200).
    pub max_move_plies: usize,
    /// Maximum capacity of the reader pool per dataset snapshot (default: 16).
    pub reader_pool_size: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            masters_dataset: None,
            elite_dataset: None,
            host: "127.0.0.1".to_string(),
            port: 8080,
            allowed_origins: vec!["*".to_string()],
            request_timeout_secs: 15,
            max_move_plies: 200,
            reader_pool_size: 16,
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
        if self.max_move_plies == 0 {
            return Err("Maximum move plies must be greater than 0".to_string());
        }
        if self.reader_pool_size == 0 {
            return Err("Reader pool size must be greater than 0".to_string());
        }
        Ok(())
    }
}
