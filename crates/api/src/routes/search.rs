//! Game discovery and individual metadata from a pinned search snapshot.

use crate::chess_util::{compute_key, parse_and_apply_moves, parse_fen};
use crate::error::ApiError;
use crate::state::{AppState, DatasetSnapshot};
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{header, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chess_database_core::{Collection, CompactMove};
use chess_database_index::search::{
    metadata::{normalize_name_search, parse_pgn_date_bounds},
    query::{
        game_metadata, search_games, source_attributions, GameFilters, GamePageRequest, GameSort,
        PageKey, PlayerSelector, PlayerSide, PositionFilter, QueryError, Range,
    },
};
use sha2::{Digest, Sha256};
use shakmaty::{san::SanPlus, uci::UciMove, CastlingMode, Chess, Role};
use std::sync::Arc;

const MAX_QUERY_BYTES: usize = 8 * 1024;
const CURSOR_BYTES: usize = 1 + 16 + 16 + 8 + 4;
const CURSOR_VERSION: u8 = 1;

#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GameParams {
    collection: Option<String>,
    fen: Option<String>,
    moves: Option<String>,
    continuation: Option<String>,
    player: Option<String>,
    player_id: Option<String>,
    player_side: PlayerSide,
    opponent: Option<String>,
    opponent_id: Option<String>,
    white: Option<String>,
    white_id: Option<String>,
    black: Option<String>,
    black_id: Option<String>,
    min_white_rating: Option<u16>,
    max_white_rating: Option<u16>,
    min_black_rating: Option<u16>,
    max_black_rating: Option<u16>,
    min_player_rating: Option<u16>,
    max_player_rating: Option<u16>,
    min_both_ratings: Option<u16>,
    max_both_ratings: Option<u16>,
    min_either_rating: Option<u16>,
    max_either_rating: Option<u16>,
    min_average_rating: Option<u16>,
    max_average_rating: Option<u16>,
    from_year: Option<u16>,
    through_year: Option<u16>,
    from_date: Option<String>,
    through_date: Option<String>,
    result: Option<String>,
    event: Option<String>,
    event_id: Option<String>,
    site: Option<String>,
    eco: Option<String>,
    time_control_category: Option<String>,
    min_plies: Option<u16>,
    max_plies: Option<u16>,
    sort: Option<GameSort>,
    limit: Option<usize>,
    cursor: Option<String>,
    exact_count: Option<bool>,
    version: Option<String>,
}

pub(crate) fn query_params<T: serde::de::DeserializeOwned>(uri: &Uri) -> Result<T, ApiError> {
    if uri
        .query()
        .is_some_and(|query| query.len() > MAX_QUERY_BYTES)
    {
        return Err(ApiError::InvalidQuery(
            "Search query exceeds 8192 bytes.".into(),
        ));
    }
    Query::<T>::try_from_uri(uri)
        .map(|Query(params)| params)
        .map_err(|error| ApiError::InvalidQuery(error.body_text()))
}

pub(crate) fn collection(value: &str) -> Result<Collection, ApiError> {
    match value {
        "masters-otb" => Ok(Collection::MastersOtb),
        "elite-online" => Ok(Collection::EliteOnline),
        _ => Err(ApiError::InvalidCollection(value.to_owned())),
    }
}

pub(crate) fn search_snapshot(
    state: &AppState,
    collection: Collection,
    metadata_only: bool,
) -> Result<Arc<DatasetSnapshot>, ApiError> {
    let snapshot = state.get_snapshot(collection)?;
    let allowed = if metadata_only {
        state.search.options.game_metadata(&collection)
    } else {
        state.search.options.discovery(&collection)
    };
    if !allowed {
        return Err(ApiError::SearchDisabled(collection.to_string()));
    }
    if snapshot.search.is_none() {
        return Err(ApiError::SearchUnavailable(collection.to_string()));
    }
    Ok(snapshot)
}

pub(crate) fn no_store(body: impl serde::Serialize) -> Response {
    ([(header::CACHE_CONTROL, "private, no-store")], Json(body)).into_response()
}

pub(crate) fn normalized(value: &str, field: &str) -> Result<String, ApiError> {
    normalized_for(value, field, normalize_name_search)
}

fn normalized_for(
    value: &str,
    field: &str,
    transform: impl FnOnce(&str) -> String,
) -> Result<String, ApiError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(ApiError::InvalidQuery(format!("Invalid '{field}' value.")));
    }
    let value = transform(value);
    if value.is_empty() || value.len() > 512 {
        return Err(ApiError::InvalidQuery(format!("Invalid '{field}' value.")));
    }
    Ok(value)
}

pub(crate) fn catalog_id(value: &str) -> Result<u64, ApiError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|id| (1..=i64::MAX as u64).contains(id))
        .ok_or_else(|| ApiError::InvalidQuery("Invalid snapshot-local identifier.".into()))
}

fn selector(
    name: Option<&str>,
    id: Option<&str>,
    field: &str,
) -> Result<Option<PlayerSelector>, ApiError> {
    match (name, id) {
        (Some(_), Some(_)) => Err(ApiError::InvalidQuery(format!(
            "Use either '{field}' or '{field}_id'."
        ))),
        (Some(name), None) => {
            let normalized = normalized(name, field)?;
            Ok(Some(PlayerSelector::Name(normalized)))
        }
        (None, Some(id)) => Ok(Some(PlayerSelector::Id(catalog_id(id)?))),
        (None, None) => Ok(None),
    }
}

fn range(min: Option<u16>, max: Option<u16>, name: &str) -> Result<Range, ApiError> {
    let range = Range {
        min: min.map(u32::from),
        max: max.map(u32::from),
    };
    check_range(range, name)
}

fn check_range(range: Range, name: &str) -> Result<Range, ApiError> {
    if matches!((range.min, range.max), (Some(min), Some(max)) if min > max) {
        return Err(ApiError::InvalidQuery(format!("Inverted {name} range.")));
    }
    Ok(range)
}

fn iso_date(value: &str) -> Result<u32, ApiError> {
    if value.len() == 10 && value.as_bytes()[4] == b'-' && value.as_bytes()[7] == b'-' {
        if let Some(bounds) = parse_pgn_date_bounds(&value.replace('-', ".")) {
            if bounds.lower_yyyymmdd == bounds.upper_yyyymmdd {
                return Ok(bounds.lower_yyyymmdd);
            }
        }
    }
    Err(ApiError::InvalidQuery(
        "Date bounds must be valid YYYY-MM-DD dates.".into(),
    ))
}

impl GameParams {
    pub(crate) fn filters(&self) -> Result<GameFilters, ApiError> {
        let player = selector(self.player.as_deref(), self.player_id.as_deref(), "player")?;
        let opponent = selector(
            self.opponent.as_deref(),
            self.opponent_id.as_deref(),
            "opponent",
        )?;
        let player_rating = range(
            self.min_player_rating,
            self.max_player_rating,
            "selected player rating",
        )?;
        if player.is_none()
            && (opponent.is_some()
                || self.player_side != PlayerSide::Either
                || player_rating != Range::default())
        {
            return Err(ApiError::InvalidQuery(
                "Player side, opponent and player rating limits require a player.".into(),
            ));
        }
        let date = check_range(
            Range {
                min: self.from_date.as_deref().map(iso_date).transpose()?,
                max: self.through_date.as_deref().map(iso_date).transpose()?,
            },
            "date",
        )?;
        let year = range(self.from_year, self.through_year, "year")?;
        if [self.from_year, self.through_year]
            .into_iter()
            .flatten()
            .any(|year| year == 0 || year > 9999)
        {
            return Err(ApiError::InvalidQuery(
                "Year must be between 1 and 9999.".into(),
            ));
        }
        if date != Range::default() && year != Range::default() {
            return Err(ApiError::InvalidQuery(
                "Use date bounds or year bounds.".into(),
            ));
        }
        if self.event.is_some() && self.event_id.is_some() {
            return Err(ApiError::InvalidQuery(
                "Use either 'event' or 'event_id'.".into(),
            ));
        }
        if self
            .result
            .as_deref()
            .is_some_and(|value| !matches!(value, "1-0" | "0-1" | "1/2-1/2" | "*"))
        {
            return Err(ApiError::InvalidQuery("Invalid PGN result.".into()));
        }
        if self.time_control_category.as_deref().is_some_and(|value| {
            !matches!(
                value,
                "bullet" | "blitz" | "rapid" | "classical" | "unknown" | "other_format"
            )
        }) {
            return Err(ApiError::InvalidQuery(
                "Invalid time-control category.".into(),
            ));
        }
        let eco = self
            .eco
            .as_ref()
            .map(|value| {
                let eco = value.to_ascii_uppercase();
                let bytes = eco.as_bytes();
                if bytes.len() != 3
                    || !matches!(bytes[0], b'A'..=b'E')
                    || !bytes[1..].iter().all(u8::is_ascii_digit)
                {
                    return Err(ApiError::InvalidQuery(
                        "ECO must be a code from A00 to E99.".into(),
                    ));
                }
                Ok(eco)
            })
            .transpose()?;

        Ok(GameFilters {
            player,
            opponent,
            player_rating,
            date,
            year,
            eco,
            player_side: self.player_side,
            white: selector(self.white.as_deref(), self.white_id.as_deref(), "white")?,
            black: selector(self.black.as_deref(), self.black_id.as_deref(), "black")?,
            white_rating: range(self.min_white_rating, self.max_white_rating, "White rating")?,
            black_rating: range(self.min_black_rating, self.max_black_rating, "Black rating")?,
            both_ratings: range(self.min_both_ratings, self.max_both_ratings, "both ratings")?,
            either_rating: range(
                self.min_either_rating,
                self.max_either_rating,
                "either rating",
            )?,
            average_rating: range(
                self.min_average_rating,
                self.max_average_rating,
                "average rating",
            )?,
            result: self.result.clone(),
            event_id: self.event_id.as_deref().map(catalog_id).transpose()?,
            event: self
                .event
                .as_deref()
                .map(|value| normalized(value, "event"))
                .transpose()?,
            site: self
                .site
                .as_deref()
                .map(|value| normalized(value, "site"))
                .transpose()?,
            time_control_category: self.time_control_category.clone(),
            plies: range(self.min_plies, self.max_plies, "game length")?,
        })
    }

    pub(crate) fn board(&self, state: &AppState) -> Result<Option<Chess>, ApiError> {
        match (self.fen.as_deref(), self.moves.as_deref()) {
            (None, None) => Ok(None),
            (Some(_), Some(_)) => Err(ApiError::AmbiguousPosition),
            (Some(fen), None) => parse_fen(fen).map(Some),
            (None, Some(moves)) => {
                let mut board = Chess::default();
                parse_and_apply_moves(&mut board, moves, state.config.max_move_plies)?;
                Ok(Some(board))
            }
        }
    }

    pub(crate) fn check_version(&self, snapshot: &DatasetSnapshot) -> Result<(), ApiError> {
        let has_catalog_ids = [
            self.player_id.as_ref(),
            self.opponent_id.as_ref(),
            self.white_id.as_ref(),
            self.black_id.as_ref(),
            self.event_id.as_ref(),
        ]
        .into_iter()
        .any(|id| id.is_some());
        validate_version(self.version.as_deref(), snapshot, has_catalog_ids)
    }
}

pub(crate) fn validate_version(
    version: Option<&str>,
    snapshot: &DatasetSnapshot,
    required: bool,
) -> Result<(), ApiError> {
    match version {
        Some(version) if version != snapshot.manifest.dataset_build_id => {
            Err(ApiError::StaleCursor)
        }
        None if required => Err(ApiError::InvalidQuery(
            "A dataset version is required for snapshot-local identifiers.".into(),
        )),
        _ => Ok(()),
    }
}

fn compact_continuation(board: &Chess, move_text: &str) -> Result<CompactMove, ApiError> {
    let played = move_text
        .parse::<UciMove>()
        .ok()
        .and_then(|uci| uci.to_move(board).ok())
        .or_else(|| {
            SanPlus::from_ascii(move_text.as_bytes())
                .ok()
                .and_then(|san| san.san.to_move(board).ok())
        })
        .ok_or_else(|| {
            ApiError::InvalidMoves("Continuation must be a single legal SAN or UCI move.".into())
        })?;
    let UciMove::Normal {
        from,
        to,
        promotion,
    } = UciMove::from_move(played, CastlingMode::Standard)
    else {
        return Err(ApiError::InvalidMoves("Unsupported continuation.".into()));
    };
    let promotion = match promotion {
        None => None,
        Some(Role::Knight) => Some(1),
        Some(Role::Bishop) => Some(2),
        Some(Role::Rook) => Some(3),
        Some(Role::Queen) => Some(4),
        _ => return Err(ApiError::InvalidMoves("Unsupported promotion.".into())),
    };
    Ok(CompactMove::new(u8::from(from), u8::from(to), promotion))
}

pub(crate) fn page_size(limit: Option<usize>, state: &AppState) -> Result<usize, ApiError> {
    let max = state.search.options.max_page_size;
    let limit = limit.unwrap_or(max.min(25));
    if limit == 0 || limit > max {
        return Err(ApiError::InvalidQuery(format!(
            "Page size must be between 1 and {max}."
        )));
    }
    Ok(limit)
}

pub(crate) fn map_query_error(error: QueryError) -> ApiError {
    match error {
        QueryError::Deadline => ApiError::SearchDeadline,
        QueryError::WorkLimit => ApiError::SearchWorkLimit,
        QueryError::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) => match failure.code {
            rusqlite::ErrorCode::OperationInterrupted => ApiError::SearchDeadline,
            rusqlite::ErrorCode::DiskFull
            | rusqlite::ErrorCode::OutOfMemory
            | rusqlite::ErrorCode::TooBig => ApiError::SearchWorkLimit,
            _ => ApiError::Storage(failure.to_string()),
        },
        error => ApiError::Storage(error.to_string()),
    }
}

pub(crate) struct CursorContext {
    snapshot: [u8; 16],
    query: [u8; 16],
}

impl CursorContext {
    pub(crate) fn new(
        snapshot: &DatasetSnapshot,
        request: &impl serde::Serialize,
    ) -> Result<Self, ApiError> {
        let bytes =
            serde_json::to_vec(request).map_err(|error| ApiError::Internal(error.to_string()))?;
        let mut snapshot_hash = Sha256::new();
        snapshot_hash.update(snapshot.collection.as_str());
        snapshot_hash.update([0]);
        snapshot_hash.update(snapshot.manifest.dataset_build_id.as_bytes());
        Ok(Self {
            snapshot: snapshot_hash.finalize()[..16].try_into().unwrap(),
            query: Sha256::digest(bytes)[..16].try_into().unwrap(),
        })
    }

    pub(crate) fn encode(&self, after: PageKey) -> String {
        let mut data = [0; CURSOR_BYTES];
        data[0] = CURSOR_VERSION;
        data[1..17].copy_from_slice(&self.snapshot);
        data[17..33].copy_from_slice(&self.query);
        data[33..41].copy_from_slice(&after.value.to_be_bytes());
        data[41..45].copy_from_slice(&after.ordinal.to_be_bytes());
        URL_SAFE_NO_PAD.encode(data)
    }

    pub(crate) fn decode(&self, text: &str) -> Result<PageKey, ApiError> {
        if text.len() > 64 {
            return Err(ApiError::InvalidCursor);
        }
        let data = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| ApiError::InvalidCursor)?;
        if data.len() != CURSOR_BYTES || data[0] != CURSOR_VERSION || data[17..33] != self.query {
            return Err(ApiError::InvalidCursor);
        }
        if data[1..17] != self.snapshot {
            return Err(ApiError::StaleCursor);
        }
        Ok(PageKey {
            value: i64::from_be_bytes(data[33..41].try_into().unwrap()),
            ordinal: u32::from_be_bytes(data[41..45].try_into().unwrap()),
        })
    }
}

pub async fn games(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Path(collection_name): Path<String>,
) -> Result<Response, ApiError> {
    let params = query_params::<GameParams>(&uri)?;
    if params.collection.is_some() {
        return Err(ApiError::InvalidQuery(
            "Specify collection in the request path.".into(),
        ));
    }
    let sort = params.sort.unwrap_or_default();

    let collection = collection(&collection_name)?;
    let snapshot = search_snapshot(&state, collection, false)?;
    params.check_version(&snapshot)?;
    let filters = params.filters()?;
    let limit = page_size(params.limit, &state)?;
    let board = params.board(&state)?;
    let position = match board.as_ref() {
        Some(board) => Some(PositionFilter {
            key: compute_key(board),
            continuation: params
                .continuation
                .as_deref()
                .map(|value| compact_continuation(board, value))
                .transpose()?,
        }),
        None if params.continuation.is_some() => return Err(ApiError::MissingPosition),
        None => None,
    };
    let cursor = CursorContext::new(&snapshot, &("games", &filters, position, sort))?;
    let after = params
        .cursor
        .as_deref()
        .map(|value| cursor.decode(value))
        .transpose()?;
    state
        .search
        .run(move |control| {
            let search = snapshot.search.as_ref().expect("checked search snapshot");
            let page = search.with_catalog(control, |catalog| {
                search_games(
                    catalog.connection(),
                    &search.postings,
                    GamePageRequest {
                        filters: &filters,
                        position,
                        sort,
                        after,
                        page_size: limit,
                        exact_count: params.exact_count.unwrap_or(false),
                    },
                    control,
                )
                .map_err(map_query_error)
            })?;
            let metadata = snapshot
                .manifest
                .search_index
                .as_ref()
                .expect("checked search manifest");
            Ok(no_store(serde_json::json!({
                "collection": snapshot.collection,
                "dataset_version": snapshot.manifest.dataset_build_id,
                "position_coverage": position.map(|_| metadata.coverage),
                "games": page.games,
                "has_more": page.after.is_some(),
                "next_cursor": page.after.map(|after| cursor.encode(after)),
                "total_games": page.total,
            })))
        })
        .await
}

pub async fn metadata(
    State(state): State<Arc<AppState>>,
    Path((collection_name, raw_id)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, ApiError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MetadataParams {}
    query_params::<MetadataParams>(&uri)?;
    let collection = collection(&collection_name)?;
    let id = raw_id
        .parse::<u64>()
        .map_err(|_| ApiError::InvalidGameId(raw_id))?;
    let snapshot = search_snapshot(&state, collection, true)?;
    state
        .search
        .run(move |control| {
            let search = snapshot.search.as_ref().expect("checked search snapshot");
            search.with_catalog(control, |catalog| {
                let record = game_metadata(catalog.connection(), id)
                    .map_err(map_query_error)?
                    .ok_or_else(|| ApiError::GameNotFound(id, collection.to_string()))?;
                Ok(no_store(serde_json::json!({
                    "collection": collection,
                    "dataset_version": snapshot.manifest.dataset_build_id,
                    "game": record,
                    "source_attributions": source_attributions(catalog.connection(), id).map_err(map_query_error)?,
                })))
            })
        })
        .await
}

/// Exact filtered Opening Explorer statistics with the collection's CPI semantics.
pub async fn filtered_explorer(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, ApiError> {
    use crate::chess_util::{derive_san, format_fen};
    use crate::models::{CandidateMoveResponse, PositionResponse};
    use crate::routes::explorer::calc_pct;
    use chess_database_index::search::aggregate::filtered_position_stats;

    let params = query_params::<GameParams>(&uri)?;
    if params.sort.is_some()
        || params.cursor.is_some()
        || params.limit.is_some()
        || params.exact_count.is_some()
        || params.continuation.is_some()
    {
        return Err(ApiError::InvalidQuery(
            "Filtered Explorer does not accept sorting, pagination, exact_count, or a continuation filter.".into(),
        ));
    }
    let collection = collection(
        params
            .collection
            .as_deref()
            .ok_or(ApiError::MissingCollection)?,
    )?;
    let board = params.board(&state)?.ok_or(ApiError::MissingPosition)?;
    let mut filters = params.filters()?;
    if filters.plies.min == Some(0) {
        filters.plies.min = None;
    }
    let aggregate = !filters.condition().values.is_empty();
    let snapshot = search_snapshot(&state, collection, false)?;
    params.check_version(&snapshot)?;
    let depth = snapshot
        .filtered_explorer_depth()
        .ok_or_else(|| ApiError::SearchUnavailable(collection.to_string()))?;
    let key = compute_key(&board);
    state
        .search
        .run_in_lane(aggregate, move |control| {
            let search = snapshot.search.as_ref().expect("checked search snapshot");
            let stats = if !aggregate {
                snapshot
                    .cpi_index
                    .as_ref()
                    .expect("checked CPI coverage")
                    .query(key)
                    .map_err(|error| ApiError::Storage(error.to_string()))?
                    .unwrap_or_default()
            } else if let Some(projected) = search
                .aggregate
                .as_ref()
                .map(|projection| {
                    projection.aggregate(&search.postings, key, depth, &filters, control)
                })
                .transpose()
                .map_err(map_query_error)?
                .flatten()
            {
                projected
            } else {
                search.with_catalog(control, |catalog| {
                    filtered_position_stats(
                        catalog.connection(),
                        &search.postings,
                        key,
                        depth,
                        &filters,
                        control,
                    )
                    .map_err(map_query_error)
                })?
            };
            let mut moves = Vec::with_capacity(stats.moves.len());
            for candidate in stats.moves {
                let white_wins = u64::from(candidate.white_wins);
                let draws = u64::from(candidate.draws);
                let black_wins = u64::from(candidate.black_wins);
                let total_games = white_wins + draws + black_wins;
                moves.push(CandidateMoveResponse {
                    san: derive_san(&board, &candidate.uci),
                    uci: candidate.uci,
                    white_wins,
                    draws,
                    black_wins,
                    total_games,
                    white_win_pct: calc_pct(white_wins, total_games),
                    draw_pct: calc_pct(draws, total_games),
                    black_win_pct: calc_pct(black_wins, total_games),
                });
            }
            moves.sort_by(|a, b| {
                b.total_games
                    .cmp(&a.total_games)
                    .then_with(|| a.uci.cmp(&b.uci))
            });
            let white_wins = u64::from(stats.white_wins);
            let draws = u64::from(stats.draws);
            let black_wins = u64::from(stats.black_wins);
            let total_games = u64::from(stats.total_games);
            let result = PositionResponse {
                collection: collection.to_string(),
                dataset_version: snapshot.manifest.dataset_build_id.clone(),
                dataset_freshness: snapshot.manifest.build_timestamp.clone(),
                cpi_depth_plies: depth as u32,
                position_key: key.to_hex(),
                fen: format_fen(&board),
                total_games,
                white_wins,
                draws,
                black_wins,
                moves,
                white_win_pct: calc_pct(white_wins, total_games),
                draw_pct: calc_pct(draws, total_games),
                black_win_pct: calc_pct(black_wins, total_games),
            };
            Ok(no_store(result))
        })
        .await
}
