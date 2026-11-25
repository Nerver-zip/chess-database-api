//! Player and event discovery over a pinned metadata catalog.

use super::search::{
    catalog_id, collection, no_store, normalized, page_size, query_params, search_snapshot,
    validate_version, CursorContext,
};
use crate::error::ApiError;
use crate::state::{AppState, DatasetSnapshot};
use axum::extract::{OriginalUri, Path, State};
use axum::response::Response;
use opening_explorer_core::Collection;
use opening_explorer_index::search::metadata::parse_pgn_date_bounds;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PlayerSearchParams {
    name: Option<String>,
    prefix: Option<String>,
    fide_id: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct EventSearchParams {
    name: Option<String>,
    prefix: Option<String>,
    year: Option<u16>,
    from_date: Option<String>,
    through_date: Option<String>,
    site: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct PlayerSummary {
    id: String,
    display_name: String,
    fide_id: Option<String>,
    game_count: u64,
    min_rating: Option<u16>,
    max_rating: Option<u16>,
    first_date_lower: Option<u32>,
    last_date_upper: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
struct EventSummary {
    id: String,
    display_name: String,
    year: Option<u32>,
    site: Option<String>,
    game_count: u64,
    first_date_lower: Option<u32>,
    last_date_upper: Option<u32>,
}

#[derive(Serialize)]
struct PlayerSearchResponse {
    collection: Collection,
    dataset_version: String,
    players: Vec<PlayerSummary>,
    has_more: bool,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct EventSearchResponse {
    collection: Collection,
    dataset_version: String,
    events: Vec<EventSummary>,
    has_more: bool,
    next_cursor: Option<String>,
}

/// Search players by exact normalized name, normalized-name prefix, or FIDE ID.
///
/// Exactly one selector is required. Results use snapshot-local IDs and stable
/// ascending ID pagination. The response omits exact totals to keep broad
/// discovery requests bounded.
pub async fn players(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Path(collection_name): Path<String>,
) -> Result<Response, ApiError> {
    let params = query_params::<PlayerSearchParams>(&uri)?;
    let collection = collection(&collection_name)?;
    let snapshot = search_snapshot(&state, collection, false)?;
    let limit = page_size(params.limit, &state)?;
    let selector_count = usize::from(params.name.is_some())
        + usize::from(params.prefix.is_some())
        + usize::from(params.fide_id.is_some());
    if selector_count != 1 {
        return Err(ApiError::InvalidQuery(
            "Specify exactly one of 'name', 'prefix', or 'fide_id'.".into(),
        ));
    }

    let (mode, selector) = if let Some(name) = params.name.as_deref() {
        ("name", normalized(name, "name")?)
    } else if let Some(prefix) = params.prefix.as_deref() {
        ("prefix", normalized(prefix, "prefix")?)
    } else {
        let fide_id = params
            .fide_id
            .as_deref()
            .expect("one selector was validated");
        ("fide_id", validate_fide_id(fide_id)?)
    };
    let upper_bound = (mode == "prefix").then(|| prefix_upper_bound(&selector));
    let cursor = CursorContext::new(&snapshot, &("players", mode, selector.as_str()))?;
    let after = params
        .cursor
        .as_deref()
        .map(|value| cursor.decode(value))
        .transpose()?
        .map(|key| {
            if key.ordinal != 0 || key.value <= 0 {
                return Err(ApiError::InvalidCursor);
            }
            Ok(key.value)
        })
        .transpose()?;

    state
        .search
        .run(move |control| {
            let search = snapshot.search.as_ref().expect("checked search snapshot");
            search.with_catalog(control, |catalog| {
                let connection = catalog.connection();
                let mut statement = match mode {
                    "name" => connection.prepare(
                        "SELECT p.player_id, p.display_name, p.fide_id, p.game_count,
                                p.min_rating, p.max_rating, p.first_date, p.last_date
                         FROM players AS p
                         WHERE EXISTS (
                            SELECT 1 FROM player_names AS n
                            WHERE n.player_id = p.player_id AND n.normalized_name = ?1
                         ) AND p.player_id > ?2
                         ORDER BY p.player_id LIMIT ?3",
                    ),
                    "prefix" => connection.prepare(
                        "SELECT p.player_id, p.display_name, p.fide_id, p.game_count,
                                p.min_rating, p.max_rating, p.first_date, p.last_date
                         FROM players AS p
                         WHERE EXISTS (
                            SELECT 1 FROM player_names AS n
                            WHERE n.player_id = p.player_id
                              AND n.normalized_name >= ?1 AND n.normalized_name < ?2
                         ) AND p.player_id > ?3
                         ORDER BY p.player_id LIMIT ?4",
                    ),
                    _ => connection.prepare(
                        "SELECT p.player_id, p.display_name, p.fide_id, p.game_count,
                                p.min_rating, p.max_rating, p.first_date, p.last_date
                         FROM players AS p WHERE p.fide_id = ?1 AND p.player_id > ?2
                         ORDER BY p.player_id LIMIT ?3",
                    ),
                }
                .map_err(storage_error)?;
                let mut rows = match mode {
                    "name" => {
                        statement.query(params![selector, after.unwrap_or(0), (limit + 1) as i64])
                    }
                    "prefix" => statement.query(params![
                        selector,
                        upper_bound.as_deref().unwrap_or(""),
                        after.unwrap_or(0),
                        (limit + 1) as i64
                    ]),
                    _ => statement.query(params![selector, after.unwrap_or(0), (limit + 1) as i64]),
                }
                .map_err(storage_error)?;
                let mut results = Vec::with_capacity(limit + 1);
                while let Some(row) = rows.next().map_err(storage_error)? {
                    results.push(read_player(row).map_err(storage_error)?);
                }
                let has_more = results.len() > limit;
                results.truncate(limit);
                let next_cursor = has_more.then(|| {
                    let last = results.last().expect("non-empty next page");
                    let value = last.id.parse::<i64>().expect("SQLite catalog ID");
                    cursor.encode(opening_explorer_index::search::query::PageKey {
                        value,
                        ordinal: 0,
                    })
                });
                Ok(no_store(PlayerSearchResponse {
                    collection: snapshot.collection,
                    dataset_version: snapshot.manifest.dataset_build_id.clone(),
                    players: results,
                    has_more,
                    next_cursor,
                }))
            })
        })
        .await
}

/// Read one player summary. The caller must supply the dataset version because
/// the player identifier belongs to one immutable collection snapshot.
pub async fn player(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Path((collection_name, raw_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        version: String,
    }
    let params = query_params::<Params>(&uri)?;
    let collection = collection(&collection_name)?;
    let id = catalog_id(&raw_id)?;
    let snapshot = search_snapshot(&state, collection, false)?;
    validate_version(Some(&params.version), &snapshot, true)?;
    detail(
        state,
        snapshot,
        id,
        "SELECT player_id, display_name, fide_id, game_count, min_rating, max_rating,
                first_date, last_date FROM players WHERE player_id = ?1",
        "player",
        collection,
    )
    .await
}

/// Search conservative event groups by exact normalized name or prefix, with
/// optional year, date-range, and site filters.
pub async fn events(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Path(collection_name): Path<String>,
) -> Result<Response, ApiError> {
    let params = query_params::<EventSearchParams>(&uri)?;
    let collection = collection(&collection_name)?;
    let snapshot = search_snapshot(&state, collection, false)?;
    let limit = page_size(params.limit, &state)?;
    if params.name.is_some() == params.prefix.is_some() {
        return Err(ApiError::InvalidQuery(
            "Specify exactly one of 'name' or 'prefix'.".into(),
        ));
    }
    if params.year.is_some_and(|year| year == 0) {
        return Err(ApiError::InvalidQuery(
            "Year must be between 1 and 9999.".into(),
        ));
    }
    let (mode, name) = if let Some(name) = params.name.as_deref() {
        ("name", normalized(name, "name")?)
    } else {
        (
            "prefix",
            normalized(params.prefix.as_deref().unwrap(), "prefix")?,
        )
    };
    let upper_bound = (mode == "prefix").then(|| prefix_upper_bound(&name));
    let site = params
        .site
        .as_deref()
        .map(|value| normalized(value, "site"))
        .transpose()?;
    let date_lower = params
        .from_date
        .as_deref()
        .map(parse_iso_date)
        .transpose()?;
    let date_upper = params
        .through_date
        .as_deref()
        .map(parse_iso_date)
        .transpose()?;
    if matches!((date_lower, date_upper), (Some(lower), Some(upper)) if lower > upper) {
        return Err(ApiError::InvalidQuery("Inverted date range.".into()));
    }
    let cursor = CursorContext::new(
        &snapshot,
        &(
            "events",
            mode,
            name.as_str(),
            params.year,
            date_lower,
            date_upper,
            site.as_deref(),
        ),
    )?;
    let after = params
        .cursor
        .as_deref()
        .map(|value| cursor.decode(value))
        .transpose()?
        .map(|key| {
            if key.ordinal != 0 || key.value <= 0 {
                return Err(ApiError::InvalidCursor);
            }
            Ok(key.value)
        })
        .transpose()?;

    state
        .search
        .run(move |control| {
            let search = snapshot.search.as_ref().expect("checked search snapshot");
            search.with_catalog(control, |catalog| {
                let connection = catalog.connection();
                let mut sql = String::from(
                    "SELECT e.event_id, e.display_name, e.year, e.site_name, e.game_count,
                            e.first_date, e.last_date FROM events AS e WHERE ",
                );
                let mut values = Vec::<rusqlite::types::Value>::new();
                match mode {
                    "name" => {
                        sql.push_str("e.normalized_name = ? ");
                        values.push(name.clone().into());
                    }
                    _ => {
                        sql.push_str("e.normalized_name >= ? AND e.normalized_name < ? ");
                        values.push(name.clone().into());
                        values.push(upper_bound.clone().unwrap_or_default().into());
                    }
                }
                if let Some(year) = params.year {
                    sql.push_str("AND e.year = ? ");
                    values.push(i64::from(year).into());
                }
                if let Some(site) = &site {
                    sql.push_str("AND e.normalized_site = ? ");
                    values.push(site.clone().into());
                }
                if date_lower.is_some() || date_upper.is_some() {
                    sql.push_str(
                        "AND EXISTS (
                            SELECT 1 FROM games AS g WHERE g.event_id = e.event_id
                              AND g.date_upper >= COALESCE(?, 0)
                              AND g.date_lower <= COALESCE(?, 100000000)
                         ) ",
                    );
                    values.push(date_lower.map(i64::from).into());
                    values.push(date_upper.map(i64::from).into());
                }
                if let Some(after) = after {
                    sql.push_str("AND e.event_id > ? ");
                    values.push(after.into());
                }
                sql.push_str("ORDER BY e.event_id LIMIT ?");
                values.push(((limit + 1) as i64).into());
                let mut statement = connection.prepare(&sql).map_err(storage_error)?;
                let mut rows = statement
                    .query(rusqlite::params_from_iter(values))
                    .map_err(storage_error)?;
                let mut results = Vec::with_capacity(limit + 1);
                while let Some(row) = rows.next().map_err(storage_error)? {
                    results.push(read_event(row).map_err(storage_error)?);
                }
                let has_more = results.len() > limit;
                results.truncate(limit);
                let next_cursor = has_more.then(|| {
                    let value = results
                        .last()
                        .expect("non-empty next page")
                        .id
                        .parse::<i64>()
                        .expect("SQLite catalog ID");
                    cursor.encode(opening_explorer_index::search::query::PageKey {
                        value,
                        ordinal: 0,
                    })
                });
                Ok(no_store(EventSearchResponse {
                    collection: snapshot.collection,
                    dataset_version: snapshot.manifest.dataset_build_id.clone(),
                    events: results,
                    has_more,
                    next_cursor,
                }))
            })
        })
        .await
}

/// Read one event edition. Its ID is snapshot-local, so `version` is required.
pub async fn event(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Path((collection_name, raw_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        version: String,
    }
    let params = query_params::<Params>(&uri)?;
    let collection = collection(&collection_name)?;
    let id = catalog_id(&raw_id)?;
    let snapshot = search_snapshot(&state, collection, false)?;
    validate_version(Some(&params.version), &snapshot, true)?;
    detail(
        state,
        snapshot,
        id,
        "SELECT event_id, display_name, year, site_name, game_count,
                first_date, last_date FROM events WHERE event_id = ?1",
        "event",
        collection,
    )
    .await
}

async fn detail(
    state: Arc<AppState>,
    snapshot: Arc<DatasetSnapshot>,
    id: u64,
    sql: &'static str,
    kind: &'static str,
    collection: Collection,
) -> Result<Response, ApiError> {
    state
        .search
        .run(move |control| {
            let search = snapshot.search.as_ref().expect("checked search snapshot");
            search.with_catalog(control, |catalog| {
                let record = catalog
                    .connection()
                    .query_row(sql, [id as i64], |row| {
                        if kind == "player" {
                            read_player(row).map(DiscoveryRecord::Player)
                        } else {
                            read_event(row).map(DiscoveryRecord::Event)
                        }
                    })
                    .optional()
                    .map_err(storage_error)?
                    .ok_or(ApiError::NotFound)?;
                let body = match record {
                    DiscoveryRecord::Player(player) => serde_json::json!({
                        "collection": collection,
                        "dataset_version": snapshot.manifest.dataset_build_id,
                        "player": player,
                    }),
                    DiscoveryRecord::Event(event) => serde_json::json!({
                        "collection": collection,
                        "dataset_version": snapshot.manifest.dataset_build_id,
                        "event": event,
                    }),
                };
                Ok(no_store(body))
            })
        })
        .await
}

enum DiscoveryRecord {
    Player(PlayerSummary),
    Event(EventSummary),
}

fn read_player(row: &Row<'_>) -> rusqlite::Result<PlayerSummary> {
    let id: i64 = row.get(0)?;
    let game_count: i64 = row.get(3)?;
    Ok(PlayerSummary {
        id: id.to_string(),
        display_name: row.get(1)?,
        fide_id: row.get(2)?,
        game_count: game_count.max(0) as u64,
        min_rating: row
            .get::<_, Option<i64>>(4)?
            .and_then(|value| u16::try_from(value).ok()),
        max_rating: row
            .get::<_, Option<i64>>(5)?
            .and_then(|value| u16::try_from(value).ok()),
        first_date_lower: row
            .get::<_, Option<i64>>(6)?
            .and_then(|value| u32::try_from(value).ok()),
        last_date_upper: row
            .get::<_, Option<i64>>(7)?
            .and_then(|value| u32::try_from(value).ok()),
    })
}

fn read_event(row: &Row<'_>) -> rusqlite::Result<EventSummary> {
    let id: i64 = row.get(0)?;
    let year: Option<i64> = row.get(2)?;
    let game_count: i64 = row.get(4)?;
    Ok(EventSummary {
        id: id.to_string(),
        display_name: row.get(1)?,
        year: year.and_then(|value| u32::try_from(value).ok()),
        site: row.get(3)?,
        game_count: game_count.max(0) as u64,
        first_date_lower: row
            .get::<_, Option<i64>>(5)?
            .and_then(|value| u32::try_from(value).ok()),
        last_date_upper: row
            .get::<_, Option<i64>>(6)?
            .and_then(|value| u32::try_from(value).ok()),
    })
}

fn validate_fide_id(value: &str) -> Result<String, ApiError> {
    if value.is_empty()
        || value.len() > 20
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.trim_start_matches('0').is_empty()
    {
        return Err(ApiError::InvalidQuery("Invalid FIDE ID.".into()));
    }
    Ok(value.to_owned())
}

fn parse_iso_date(value: &str) -> Result<u32, ApiError> {
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

fn prefix_upper_bound(prefix: &str) -> String {
    let mut characters: Vec<char> = prefix.chars().collect();
    while let Some(last) = characters.pop() {
        let next = last as u32 + 1;
        if let Some(next_character) = char::from_u32(next) {
            characters.push(next_character);
            return characters.into_iter().collect();
        }
    }
    // The normalized field is bounded to 512 bytes, so the all-U+10FFFF edge
    // is exceptionally rare. SQLite's BINARY upper bound can use this marker.
    format!("{prefix}\u{10ffff}")
}

fn storage_error(error: rusqlite::Error) -> ApiError {
    ApiError::Storage(format!("Search catalog: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use crate::routes::create_router;
    use crate::search_runtime::SearchOptions;
    use crate::state::AppState;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use opening_explorer_core::{
        CompactMove, GameMetadata, GameResult, IngestionStats, ParsedGame,
    };
    use opening_explorer_storage::{
        build_dataset_position_index, DatasetIndexConfig, DatasetWriter, DatasetWriterConfig,
        SourceArchiveMetadata,
    };
    use std::path::Path;
    use tempfile::tempdir;
    use tower::ServiceExt;

    fn build_discovery_dataset(directory: &Path) {
        let mut writer = DatasetWriter::new(
            directory.to_path_buf(),
            DatasetWriterConfig {
                collection: Collection::MastersOtb,
                block_capacity: 4,
                max_games_per_shard: 10,
                zstd_level: 1,
                ..Default::default()
            },
        )
        .unwrap();
        writer
            .push_game(ParsedGame {
                metadata: GameMetadata {
                    id: 1,
                    collection: Collection::MastersOtb,
                    white: "Alice Example".to_owned(),
                    black: "Bob Example".to_owned(),
                    white_elo: Some(2400),
                    black_elo: Some(2300),
                    date: Some("2024.02.29".to_owned()),
                    event: Some("Spring Open".to_owned()),
                    site: Some("Oslo".to_owned()),
                    round: Some("1".to_owned()),
                    result: GameResult::WhiteWins,
                    eco: Some("C20".to_owned()),
                    time_control: Some("600+0".to_owned()),
                    source_id: None,
                    broadcast_url: None,
                    source_attribution: None,
                },
                moves: vec![CompactMove::new(12, 28, None)],
                fen_start: None,
            })
            .unwrap();
        writer
            .finalize(
                IngestionStats {
                    total_encountered: 1,
                    accepted_unique: 1,
                    ..Default::default()
                },
                vec![SourceArchiveMetadata {
                    filename: "fixture.pgn".to_owned(),
                    url: None,
                    sha256: Some("00".repeat(32)),
                    size_bytes: 1,
                    license: "CC0-1.0".to_owned(),
                    checksum_verified: Some(true),
                }],
            )
            .unwrap();
        build_dataset_position_index(
            directory,
            DatasetIndexConfig {
                depth_plies: Some(40),
                expected_collection: Some(Collection::MastersOtb),
                memory_limit_bytes: 1024 * 1024,
                max_merge_fan_in: 4,
                positions_per_block: 16,
                zstd_level: 1,
                build_id_override: None,
            },
        )
        .unwrap();
        opening_explorer_storage::search::build_search_index(
            directory,
            opening_explorer_storage::search::BuildSearchConfig::default(),
        )
        .unwrap();
    }

    async fn get(router: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn discovery_uses_discovery_gate_independently_of_game_metadata() {
        let temporary = tempdir().unwrap();
        let dataset = temporary.path().join("masters");
        build_discovery_dataset(&dataset);
        let state = AppState::new_with_search(
            ServerConfig {
                masters_dataset: Some(dataset),
                ..Default::default()
            },
            SearchOptions {
                masters_discovery: true,
                masters_game_metadata: false,
                ..Default::default()
            },
        )
        .unwrap();
        let version = state
            .get_snapshot(Collection::MastersOtb)
            .unwrap()
            .manifest
            .dataset_build_id
            .clone();
        let router = create_router(state);

        let (status, players_body) =
            get(&router, "/v1/collections/masters-otb/players?prefix=alice").await;
        assert_eq!(status, StatusCode::OK, "{players_body}");
        let player_id = players_body["players"][0]["id"].as_str().unwrap();
        let (status, player_body) = get(
            &router,
            &format!("/v1/collections/masters-otb/players/{player_id}?version={version}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{player_body}");

        let (status, events_body) =
            get(&router, "/v1/collections/masters-otb/events?prefix=spring").await;
        assert_eq!(status, StatusCode::OK, "{events_body}");
        let event_id = events_body["events"][0]["id"].as_str().unwrap();
        let (status, event_body) = get(
            &router,
            &format!("/v1/collections/masters-otb/events/{event_id}?version={version}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{event_body}");

        let (status, game_body) =
            get(&router, "/v1/collections/masters-otb/games/1/metadata").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{game_body}");
    }
}
