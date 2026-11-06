//! Game retrieval endpoint (`GET /v1/collections/{collection}/games/{id}`).

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use codb_core::Collection;
use codb_storage::StorageError;
use std::sync::Arc;

/// Handler for `GET /v1/collections/{collection}/games/{id}`.
pub async fn get_game_by_id(
    State(state): State<Arc<AppState>>,
    Path((collection_str, id_str)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    // 1. Validate collection identifier
    let collection = match collection_str.as_str() {
        "masters-otb" => Collection::MastersOtb,
        "elite-online" => Collection::EliteOnline,
        other => return Err(ApiError::InvalidCollection(other.to_string())),
    };

    // 2. Validate 64-bit unsigned game ID
    let game_id = id_str
        .parse::<u64>()
        .map_err(|_| ApiError::InvalidGameId(id_str.clone()))?;

    // 3. Acquire pinned snapshot
    let snapshot = state.get_snapshot(collection)?;
    let pool = Arc::clone(&snapshot.reader_pool);

    // 4. Offload blocking shard seek and block decompression to Tokio blocking threadpool
    let result = tokio::task::spawn_blocking(move || pool.get_game_pgn(game_id))
        .await
        .map_err(|e| ApiError::Internal(format!("Worker thread failure: {}", e)))?;

    // 5. Handle retrieval outcome
    match result {
        Ok(pgn) => {
            let response = (
                [
                    (
                        header::CONTENT_TYPE,
                        "application/x-chess-pgn; charset=utf-8",
                    ),
                    (header::CACHE_CONTROL, "public, max-age=3600"),
                ],
                pgn,
            )
                .into_response();
            Ok(response)
        }
        Err(StorageError::GameNotFound(_)) => Err(ApiError::GameNotFound(
            game_id,
            collection.as_str().to_string(),
        )),
        Err(e) => Err(ApiError::Storage(format!("Game retrieval failed: {}", e))),
    }
}
