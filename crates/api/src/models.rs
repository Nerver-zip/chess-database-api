//! HTTP Request and Response Data Transfer Objects (DTOs).

use serde::{Deserialize, Serialize};

/// Query parameters for `GET /v1/explorer/position`.
#[derive(Debug, Clone, Deserialize)]
pub struct PositionQuery {
    /// Collection identifier: `masters-otb` or `elite-online`.
    pub collection: Option<String>,
    /// FEN string representing the target chess position.
    pub fen: Option<String>,
    /// Move sequence from the starting position (comma or space separated).
    pub moves: Option<String>,
    /// Unsupported parameter captured for explicit rejection.
    pub since_year: Option<serde_json::Value>,
    /// Unsupported parameter captured for explicit rejection.
    pub min_rating: Option<serde_json::Value>,
}

/// JSON payload returned by `GET /v1/explorer/position`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PositionResponse {
    /// Collection identifier (`masters-otb` or `elite-online`).
    pub collection: String,
    /// Active version identifier of the dataset (e.g. `2026.10-v1` or `v1`).
    pub dataset_version: String,
    /// ISO 8601 build timestamp of the dataset snapshot.
    pub dataset_freshness: String,
    /// Plies per game covered by the position index of this snapshot. Positions are
    /// indexed only for moves inside this window (see `docs/API_REFERENCE.md`).
    #[serde(default)]
    pub cpi_depth_plies: u32,
    /// Canonical 128-bit position key formatted as 32 lowercase hex characters.
    pub position_key: String,
    /// Normalized FEN representation of the board.
    pub fen: String,
    /// Aggregate total of games reaching this position.
    pub total_games: u64,
    /// Number of games won by White from this position.
    pub white_wins: u64,
    /// Number of drawn games from this position.
    pub draws: u64,
    /// Number of games won by Black from this position.
    pub black_wins: u64,
    /// Percentage of games won by White (rounded to 1 decimal place).
    pub white_win_pct: f64,
    /// Percentage of drawn games (rounded to 1 decimal place).
    pub draw_pct: f64,
    /// Percentage of games won by Black (rounded to 1 decimal place).
    pub black_win_pct: f64,
    /// List of candidate moves played from this position, sorted by frequency descending.
    pub moves: Vec<CandidateMoveResponse>,
}

/// Positional statistics for a single candidate move played from a position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CandidateMoveResponse {
    /// Move in Universal Chess Interface (UCI) notation (e.g. `e2e4`, `g1f3`).
    pub uci: String,
    /// Move in Standard Algebraic Notation (SAN) (e.g. `e4`, `Nf3`, `O-O`).
    pub san: String,
    /// Number of games won by White when this move was played.
    pub white_wins: u64,
    /// Number of drawn games when this move was played.
    pub draws: u64,
    /// Number of games won by Black when this move was played.
    pub black_wins: u64,
    /// Total games where this candidate move was played (`white_wins + draws + black_wins`).
    pub total_games: u64,
    /// White win rate percentage for this move.
    pub white_win_pct: f64,
    /// Draw rate percentage for this move.
    pub draw_pct: f64,
    /// Black win rate percentage for this move.
    pub black_win_pct: f64,
}

/// JSON payload returned by `GET /v1/collections`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionsResponse {
    pub collections: Vec<CollectionMetadataResponse>,
}

/// Metadata description for an active collection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionMetadataResponse {
    pub collection: String,
    pub dataset_version: String,
    pub build_timestamp: String,
    pub total_games: u64,
    pub cpi_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpi_depth_plies: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpi_total_positions: Option<u64>,
    pub format_version: String,
}

/// JSON payload returned by `GET /health`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// JSON payload returned by `GET /ready`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadyResponse {
    pub status: String,
    pub collections_loaded: Vec<String>,
}

/// JSON payload returned by `POST /v1/admin/reload`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdminReloadResponse {
    pub status: String,
    pub reloaded_collections: Vec<String>,
}
