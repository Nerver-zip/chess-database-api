//! Game retrieval endpoint (`GET /v1/collections/{collection}/games/{id}`).

use crate::chess_util::if_none_match_matches;
use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use opening_explorer_core::Collection;
use std::sync::Arc;
use std::time::Duration;

/// Handler for `GET /v1/collections/{collection}/games/{id}`.
pub async fn get_game_by_id(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
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

    // 4. Verify existence in games.idx FIRST per RFC 7232!
    // Never return 304 Not Modified for a resource that does not exist.
    if !snapshot.contains_game(game_id) {
        return Err(ApiError::GameNotFound(
            game_id,
            collection.as_str().to_string(),
        ));
    }

    let etag = format!(
        "\"{}-{}-{}\"",
        collection.as_str(),
        snapshot.manifest.dataset_build_id,
        game_id
    );

    // 5. Evaluate conditional request (If-None-Match) against existing resource
    if let Some(if_none_match) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        if if_none_match_matches(if_none_match, &etag) {
            return Ok((
                StatusCode::NOT_MODIFIED,
                [
                    (header::ETAG, etag),
                    (header::CACHE_CONTROL, "public, max-age=3600".to_string()),
                ],
            )
                .into_response());
        }
    }

    // 6. Retrieve game via bounded reader pool
    let timeout = Duration::from_secs(state.config.reader_pool_timeout_secs);
    let pgn = snapshot.reader_pool.get_game_pgn(game_id, timeout).await?;

    let response = (
        [
            (
                header::CONTENT_TYPE,
                "application/x-chess-pgn; charset=utf-8".to_string(),
            ),
            (header::CACHE_CONTROL, "public, max-age=3600".to_string()),
            (header::ETAG, etag),
        ],
        pgn,
    )
        .into_response();
    Ok(response)
}
