//! Position query endpoint (`GET /v1/explorer/position`).

use crate::chess_util::{
    compute_key, compute_position_etag, derive_san, format_fen, if_none_match_matches,
    parse_and_apply_moves, parse_fen,
};
use crate::error::ApiError;
use crate::models::{CandidateMoveResponse, PositionQuery, PositionResponse};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use opening_explorer_core::Collection;
use shakmaty::Chess;
use std::sync::Arc;

#[inline]
fn calc_pct(part: u64, total: u64) -> f64 {
    if total > 0 {
        ((part as f64 * 1000.0 / total as f64).round()) / 10.0
    } else {
        0.0
    }
}

/// Handler for `GET /v1/explorer/position`.
pub async fn query_position(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<PositionQuery>,
) -> Result<Response, ApiError> {
    // 1. Explicitly reject unsupported query parameters
    if query.since_year.is_some() {
        return Err(ApiError::UnsupportedParameter("since_year".to_string()));
    }
    if query.min_rating.is_some() {
        return Err(ApiError::UnsupportedParameter("min_rating".to_string()));
    }

    // 2. Validate collection parameter
    let col_str = query
        .collection
        .as_deref()
        .ok_or(ApiError::MissingCollection)?;

    let collection = match col_str {
        "masters-otb" => Collection::MastersOtb,
        "elite-online" => Collection::EliteOnline,
        other => return Err(ApiError::InvalidCollection(other.to_string())),
    };

    // 3. Validate and parse position representation (mutually exclusive)
    let board = match (query.fen.as_deref(), query.moves.as_deref()) {
        (None, None) => return Err(ApiError::MissingPosition),
        (Some(_), Some(_)) => return Err(ApiError::AmbiguousPosition),
        (Some(f), None) => parse_fen(f)?,
        (None, Some(m_str)) => {
            let mut b = Chess::default();
            parse_and_apply_moves(&mut b, m_str, state.config.max_move_plies)?;
            b
        }
    };

    let key = compute_key(&board);
    let fen_str = format_fen(&board);

    // 4. Pin immutable dataset snapshot
    let snapshot = state.get_snapshot(collection)?;
    let cpi_index = snapshot
        .cpi_index
        .as_ref()
        .ok_or_else(|| ApiError::IndexUnavailable(collection.as_str().to_string()))?;
    let cpi_depth_plies = snapshot
        .manifest
        .position_index
        .as_ref()
        .map(|meta| meta.indexed_depth_plies as u32)
        .ok_or_else(|| ApiError::IndexUnavailable(collection.as_str().to_string()))?;

    // 5. Query CPI v2 position index (memory-mapped, lock-free)
    let maybe_stats = cpi_index
        .query(key)
        .map_err(|e| ApiError::Storage(format!("CPI query failure: {}", e)))?;

    // 6. Build consistent response
    let (total_games, white_wins, draws, black_wins, candidate_moves) = match maybe_stats {
        Some(stats) => {
            let mut moves_out = Vec::with_capacity(stats.moves.len());
            for m in stats.moves {
                let total = (m.white_wins + m.draws + m.black_wins) as u64;
                let w = m.white_wins as u64;
                let d = m.draws as u64;
                let b = m.black_wins as u64;
                let san = derive_san(&board, &m.uci);

                moves_out.push(CandidateMoveResponse {
                    uci: m.uci,
                    san,
                    white_wins: w,
                    draws: d,
                    black_wins: b,
                    total_games: total,
                    white_win_pct: calc_pct(w, total),
                    draw_pct: calc_pct(d, total),
                    black_win_pct: calc_pct(b, total),
                });
            }

            // Sort candidate moves: frequency descending, then UCI ascending
            moves_out.sort_by(|a, b| {
                b.total_games
                    .cmp(&a.total_games)
                    .then_with(|| a.uci.cmp(&b.uci))
            });

            (
                stats.total_games as u64,
                stats.white_wins as u64,
                stats.draws as u64,
                stats.black_wins as u64,
                moves_out,
            )
        }
        None => (0, 0, 0, 0, Vec::new()),
    };

    let etag = compute_position_etag(
        collection.as_str(),
        &snapshot.manifest.dataset_build_id,
        &key.to_hex(),
        &fen_str,
    );

    // Check conditional request (If-None-Match)
    if let Some(if_none_match) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        if if_none_match_matches(if_none_match, &etag) {
            return Ok((
                StatusCode::NOT_MODIFIED,
                [
                    (header::ETAG, etag),
                    (header::CACHE_CONTROL, "public, max-age=60".to_string()),
                ],
            )
                .into_response());
        }
    }

    let resp = PositionResponse {
        collection: collection.as_str().to_string(),
        dataset_version: snapshot.manifest.dataset_build_id.clone(),
        dataset_freshness: snapshot.manifest.build_timestamp.clone(),
        cpi_depth_plies,
        position_key: key.to_hex(),
        fen: fen_str,
        total_games,
        white_wins,
        draws,
        black_wins,
        white_win_pct: calc_pct(white_wins, total_games),
        draw_pct: calc_pct(draws, total_games),
        black_win_pct: calc_pct(black_wins, total_games),
        moves: candidate_moves,
    };

    Ok((
        StatusCode::OK,
        [
            (header::ETAG, etag),
            (header::CACHE_CONTROL, "public, max-age=60".to_string()),
        ],
        Json(resp),
    )
        .into_response())
}
