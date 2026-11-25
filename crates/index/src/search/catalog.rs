//! Bounded-memory SQLite projection of canonical game metadata.

use super::metadata::{
    classify_time_control, derive_fide_id, normalize_name_search, parse_pgn_date_bounds,
    PlayerIdentityEvidence,
};
use opening_explorer_core::{Collection, GameSourceAttribution, ParsedGame};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const SEARCH_CATALOG_SCHEMA_VERSION: i32 = 1;
const SEARCH_CATALOG_APPLICATION_ID: i32 = 0x4344_4154; // "CDAT"

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("SQLite search catalog error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Search catalog I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Search catalog serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Search catalog validation failed: {0}")]
    InvalidData(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogBuildSummary {
    pub games: u64,
    pub players: u64,
    pub events: u64,
    pub file_size_bytes: u64,
}

/// Writes one private catalog artifact from an ordered canonical game stream.
/// Source-attribution records are applied separately because the canonical
/// stream order and stable-ID sidecar order are independent.
pub struct SearchCatalogBuilder {
    connection: Option<Connection>,
    path: PathBuf,
    collection: Collection,
    expected_games: u64,
    next_ordinal: u64,
    last_attribution_game_id: Option<u64>,
    completed: bool,
}

impl SearchCatalogBuilder {
    /// Create a new catalog at a path inside the caller's private staging area.
    /// `cache_limit_mib` is a SQLite page-cache bound. Temp sorting is file-backed.
    pub fn create(
        path: &Path,
        collection: Collection,
        expected_games: u64,
        cache_limit_mib: u32,
    ) -> Result<Self, CatalogError> {
        if path.exists() {
            return Err(CatalogError::InvalidData(format!(
                "Search catalog path already exists: {}",
                path.display()
            )));
        }
        if cache_limit_mib == 0 {
            return Err(CatalogError::InvalidData(
                "Search catalog cache limit must be positive".to_owned(),
            ));
        }

        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.execute_batch(&format!(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = OFF;
             PRAGMA synchronous = OFF;
             PRAGMA temp_store = FILE;
             PRAGMA locking_mode = EXCLUSIVE;
             PRAGMA cache_size = -{};
             PRAGMA foreign_keys = ON;
             PRAGMA application_id = {};
             PRAGMA user_version = {};",
            u64::from(cache_limit_mib) * 1024,
            SEARCH_CATALOG_APPLICATION_ID,
            SEARCH_CATALOG_SCHEMA_VERSION
        ))?;
        create_schema(&connection)?;
        connection.execute_batch("BEGIN IMMEDIATE")?;

        Ok(Self {
            connection: Some(connection),
            path: path.to_path_buf(),
            collection,
            expected_games,
            next_ordinal: 0,
            last_attribution_game_id: None,
            completed: false,
        })
    }

    /// Add one canonical game in the same deterministic order used by its snapshot.
    pub fn push(&mut self, game: &ParsedGame) -> Result<u64, CatalogError> {
        if game.metadata.collection != self.collection {
            return Err(CatalogError::InvalidData(format!(
                "Game {} belongs to {}, catalog is {}",
                game.metadata.id, game.metadata.collection, self.collection
            )));
        }
        if self.next_ordinal >= self.expected_games {
            return Err(CatalogError::InvalidData(format!(
                "Search catalog received more than {} games",
                self.expected_games
            )));
        }
        let ordinal = i64::try_from(self.next_ordinal).map_err(|_| {
            CatalogError::InvalidData("Search catalog ordinal exceeds SQLite INTEGER".to_owned())
        })?;
        let id_bytes = game.metadata.id.to_be_bytes();
        let date_bounds = game
            .metadata
            .date
            .as_deref()
            .and_then(parse_pgn_date_bounds);
        let year = date_bounds.map(|bounds| bounds.lower_yyyymmdd / 10_000);
        let average_rating_sum = game
            .metadata
            .white_elo
            .zip(game.metadata.black_elo)
            .map(|(white, black)| i64::from(white) + i64::from(black));
        let white_name_norm = normalize_name_search(&game.metadata.white);
        let black_name_norm = normalize_name_search(&game.metadata.black);
        let white_identity_key = player_identity_key(self.collection, &game.metadata.white, None);
        let black_identity_key = player_identity_key(self.collection, &game.metadata.black, None);
        let speed = classify_time_control(game.metadata.time_control.as_deref()).as_str();
        let event_norm = game.metadata.event.as_deref().map(normalize_name_search);
        let site_norm = game.metadata.site.as_deref().map(normalize_name_search);
        let event_site = game
            .metadata
            .site
            .as_deref()
            .filter(|site| Some(*site) != game.metadata.source_id.as_deref());
        let event_key = event_identity_key(game.metadata.event.as_deref(), year, event_site)?;

        self.connection()?
            .prepare_cached(
                "INSERT INTO games (
                ordinal, game_id, white_name, black_name, white_name_norm, black_name_norm,
                white_identity_key, black_identity_key, white_fide_id, black_fide_id,
                white_rating, black_rating, average_rating_sum, result, date_original,
                date_lower, date_upper, year, event_name, event_norm, event_key,
                site_name, site_norm, round_name, eco, time_control, time_control_category,
                plies, starting_fen, source_id, broadcast_url
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL,
                ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29
             )",
            )?
            .execute(params![
                ordinal,
                id_bytes.as_slice(),
                game.metadata.white,
                game.metadata.black,
                white_name_norm,
                black_name_norm,
                white_identity_key,
                black_identity_key,
                game.metadata.white_elo.map(i64::from),
                game.metadata.black_elo.map(i64::from),
                average_rating_sum,
                game.metadata.result.to_str(),
                game.metadata.date,
                date_bounds.map(|bounds| i64::from(bounds.lower_yyyymmdd)),
                date_bounds.map(|bounds| i64::from(bounds.upper_yyyymmdd)),
                year.map(i64::from),
                game.metadata.event,
                event_norm,
                event_key,
                game.metadata.site,
                site_norm,
                game.metadata.round,
                game.metadata.eco,
                game.metadata.time_control,
                speed,
                i64::try_from(game.moves.len()).map_err(|_| {
                    CatalogError::InvalidData(format!(
                        "Game {} length exceeds SQLite INTEGER",
                        game.metadata.id
                    ))
                })?,
                game.fen_start,
                game.metadata.source_id,
                game.metadata.broadcast_url,
            ])?;
        self.next_ordinal += 1;
        Ok(self.next_ordinal - 1)
    }

    /// Attach the validated sidecar records for one stable game ID.
    /// Calls must follow ascending stable game ID order and may occur once per game.
    pub fn add_attributions(
        &mut self,
        game_id: u64,
        attributions: &[GameSourceAttribution],
    ) -> Result<(), CatalogError> {
        if attributions.is_empty() {
            return Err(CatalogError::InvalidData(format!(
                "Game {game_id} has an empty source-attribution list"
            )));
        }
        if self
            .last_attribution_game_id
            .is_some_and(|previous| game_id <= previous)
        {
            return Err(CatalogError::InvalidData(format!(
                "Source attributions are not strictly sorted at game {game_id}"
            )));
        }

        let id_bytes = game_id.to_be_bytes();
        let connection = self.connection()?;
        let (ordinal, white_name, black_name) = connection
            .prepare_cached("SELECT ordinal, white_name, black_name FROM games WHERE game_id = ?1")?
            .query_row([id_bytes.as_slice()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .optional()?
            .ok_or_else(|| {
                CatalogError::InvalidData(format!(
                    "Source attribution references unknown game ID {game_id}"
                ))
            })?;

        let json = serde_json::to_string(attributions)?;
        let white_id = derive_fide_id(attributions.iter().map(|record| PlayerIdentityEvidence {
            fide_id: record.white_fide_id.as_deref(),
            identity_evidence: record.white_identity_evidence.as_deref(),
        }));
        let black_id = derive_fide_id(attributions.iter().map(|record| PlayerIdentityEvidence {
            fide_id: record.black_fide_id.as_deref(),
            identity_evidence: record.black_identity_evidence.as_deref(),
        }));
        connection
            .prepare_cached(
                "INSERT INTO source_attributions (game_id, attributions_json) VALUES (?1, ?2)",
            )?
            .execute(params![id_bytes.as_slice(), json])?;
        connection
            .prepare_cached(
                "UPDATE games
             SET white_fide_id = ?1, black_fide_id = ?2,
                 white_identity_key = ?3, black_identity_key = ?4
             WHERE ordinal = ?5",
            )?
            .execute(params![
                white_id,
                black_id,
                player_identity_key(self.collection, &white_name, white_id.as_deref(),),
                player_identity_key(self.collection, &black_name, black_id.as_deref(),),
                ordinal,
            ])?;
        self.last_attribution_game_id = Some(game_id);
        Ok(())
    }

    /// Finalize player/event dictionaries and all catalog indexes.
    pub fn finish(mut self) -> Result<CatalogBuildSummary, CatalogError> {
        if self.next_ordinal != self.expected_games {
            return Err(CatalogError::InvalidData(format!(
                "Search catalog contains {} games, expected {}",
                self.next_ordinal, self.expected_games
            )));
        }
        let connection = self.connection()?;
        build_player_catalog(connection)?;
        build_event_catalog(connection)?;
        create_search_indexes(connection)?;
        connection.execute_batch("COMMIT")?;
        connection.execute_batch("VACUUM;")?;
        connection.execute_batch("PRAGMA locking_mode = NORMAL;")?;
        drop(self.connection.take());

        let file_size_bytes = fs::metadata(&self.path)?.len();
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let games = table_count(&connection, "games")?;
        let players = table_count(&connection, "players")?;
        let events = table_count(&connection, "events")?;
        if games != self.expected_games {
            return Err(CatalogError::InvalidData(format!(
                "Final search catalog contains {games} games, expected {}",
                self.expected_games
            )));
        }
        self.completed = true;
        Ok(CatalogBuildSummary {
            games,
            players,
            events,
            file_size_bytes,
        })
    }

    fn connection(&self) -> Result<&Connection, CatalogError> {
        self.connection.as_ref().ok_or_else(|| {
            CatalogError::InvalidData("Search catalog builder is already finalized".to_owned())
        })
    }
}

impl Drop for SearchCatalogBuilder {
    fn drop(&mut self) {
        drop(self.connection.take());
        if !self.completed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Read-only handle to a finalized metadata catalog.
pub struct SearchCatalog {
    connection: Connection,
}

impl SearchCatalog {
    pub fn open(path: &Path) -> Result<Self, CatalogError> {
        let path = path.canonicalize()?;
        let mut uri = String::from("file:");
        for &byte in path.as_os_str().as_encoded_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.') {
                uri.push(char::from(byte));
            } else {
                write!(uri, "%{byte:02X}").expect("writing into a String cannot fail");
            }
        }
        uri.push_str("?immutable=1");
        let connection = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        let application_id: i32 =
            connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
        let schema_version: i32 =
            connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if application_id != SEARCH_CATALOG_APPLICATION_ID
            || schema_version != SEARCH_CATALOG_SCHEMA_VERSION
        {
            return Err(CatalogError::InvalidData(format!(
                "Unsupported search catalog identity/version ({application_id:#x}/{schema_version})"
            )));
        }
        connection.execute_batch("PRAGMA query_only = ON; PRAGMA temp_store = FILE;")?;
        Ok(Self { connection })
    }

    /// Exposes the read-only SQLite handle for query executors owned by the caller.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Verify SQLite structure, dense ordinal maps, and derived date bounds.
    pub fn verify_integrity(&self) -> Result<CatalogBuildSummary, CatalogError> {
        let integrity: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(CatalogError::InvalidData(format!(
                "SQLite integrity check failed: {integrity}"
            )));
        }

        let games = table_count(&self.connection, "games")?;
        let players = table_count(&self.connection, "players")?;
        let events = table_count(&self.connection, "events")?;
        verify_dense_ordinals(&self.connection, "games", "ordinal", games)?;
        verify_dense_ids(&self.connection, "players", "player_id", players)?;
        verify_dense_ids(&self.connection, "events", "event_id", events)?;

        let dangling_player_ids: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM games
             WHERE white_player_id IS NULL OR black_player_id IS NULL
                OR NOT EXISTS (SELECT 1 FROM players WHERE player_id = white_player_id)
                OR NOT EXISTS (SELECT 1 FROM players WHERE player_id = black_player_id)",
            [],
            |row| row.get(0),
        )?;
        let incorrect_event_counts: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM events
             WHERE game_count != (SELECT COUNT(*) FROM games WHERE games.event_id = events.event_id)",
            [],
            |row| row.get(0),
        )?;
        if dangling_player_ids != 0 || incorrect_event_counts != 0 {
            return Err(CatalogError::InvalidData(format!(
                "Catalog reference checks failed ({dangling_player_ids} player, {incorrect_event_counts} event)"
            )));
        }

        let mut statement = self.connection.prepare(
            "SELECT date_original, date_lower, date_upper, year FROM games ORDER BY ordinal",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let original: Option<String> = row.get(0)?;
            let lower: Option<i64> = row.get(1)?;
            let upper: Option<i64> = row.get(2)?;
            let year: Option<i64> = row.get(3)?;
            let bounds = original.as_deref().and_then(parse_pgn_date_bounds);
            let expected = bounds.map(|value| {
                (
                    i64::from(value.lower_yyyymmdd),
                    i64::from(value.upper_yyyymmdd),
                    i64::from(value.lower_yyyymmdd / 10_000),
                )
            });
            let actual = lower.zip(upper).zip(year).map(|((lo, hi), y)| (lo, hi, y));
            if actual != expected {
                return Err(CatalogError::InvalidData(format!(
                    "Stored date bounds disagree with original PGN date {original:?}"
                )));
            }
        }

        Ok(CatalogBuildSummary {
            games,
            players,
            events,
            file_size_bytes: self.connection.path().map_or(0, |path| {
                fs::metadata(path).map_or(0, |metadata| metadata.len())
            }),
        })
    }
}

fn create_schema(connection: &Connection) -> Result<(), CatalogError> {
    connection.execute_batch(
        "CREATE TABLE games (
            ordinal INTEGER PRIMARY KEY CHECK(ordinal >= 0),
            game_id BLOB NOT NULL UNIQUE CHECK(length(game_id) = 8),
            white_name TEXT NOT NULL,
            black_name TEXT NOT NULL,
            white_name_norm TEXT NOT NULL,
            black_name_norm TEXT NOT NULL,
            white_identity_key TEXT NOT NULL,
            black_identity_key TEXT NOT NULL,
            white_fide_id TEXT,
            black_fide_id TEXT,
            white_rating INTEGER CHECK(white_rating BETWEEN 0 AND 65535),
            black_rating INTEGER CHECK(black_rating BETWEEN 0 AND 65535),
            average_rating_sum INTEGER,
            result TEXT NOT NULL CHECK(result IN ('1-0', '1/2-1/2', '0-1', '*')),
            date_original TEXT,
            date_lower INTEGER,
            date_upper INTEGER,
            year INTEGER,
            event_name TEXT,
            event_norm TEXT,
            event_key TEXT,
            event_id INTEGER,
            site_name TEXT,
            site_norm TEXT,
            round_name TEXT,
            eco TEXT,
            time_control TEXT,
            time_control_category TEXT NOT NULL,
            plies INTEGER NOT NULL CHECK(plies >= 0),
            starting_fen TEXT,
            source_id TEXT,
            broadcast_url TEXT,
            white_player_id INTEGER,
            black_player_id INTEGER
         );
         CREATE TABLE source_attributions (
            game_id BLOB PRIMARY KEY REFERENCES games(game_id),
            attributions_json TEXT NOT NULL CHECK(json_valid(attributions_json))
         ) WITHOUT ROWID;
         CREATE TABLE players (
            player_id INTEGER PRIMARY KEY,
            identity_key TEXT NOT NULL UNIQUE,
            display_name TEXT NOT NULL,
            game_count INTEGER NOT NULL CHECK(game_count >= 0),
            min_rating INTEGER,
            max_rating INTEGER,
            first_date INTEGER,
            last_date INTEGER,
            fide_id TEXT
         );
         CREATE TABLE player_names (
            player_id INTEGER NOT NULL REFERENCES players(player_id),
            display_name TEXT NOT NULL,
            normalized_name TEXT NOT NULL,
            PRIMARY KEY(player_id, display_name)
         ) WITHOUT ROWID;
         CREATE TABLE events (
            event_id INTEGER PRIMARY KEY,
            event_key TEXT NOT NULL UNIQUE,
            display_name TEXT NOT NULL,
            normalized_name TEXT NOT NULL,
            year INTEGER,
            site_name TEXT,
            normalized_site TEXT,
            game_count INTEGER NOT NULL CHECK(game_count >= 0),
            first_date INTEGER,
            last_date INTEGER
         );
         ",
    )?;
    Ok(())
}

fn create_search_indexes(connection: &Connection) -> Result<(), CatalogError> {
    connection.execute_batch(
        "CREATE INDEX games_white_player ON games(white_player_id, ordinal);
         CREATE INDEX games_black_player ON games(black_player_id, ordinal);
         CREATE INDEX games_player_pair ON games(white_player_id, black_player_id, ordinal);
         CREATE INDEX games_white_name ON games(white_name_norm, ordinal);
         CREATE INDEX games_black_name ON games(black_name_norm, ordinal);
         CREATE INDEX games_year_date ON games(year, date_lower, ordinal);
         CREATE INDEX games_date_asc ON games(date_lower, ordinal);
         CREATE INDEX games_date_sort_asc ON games(COALESCE(date_lower, 100000000), ordinal);
         CREATE INDEX games_date_sort_desc ON games(COALESCE(date_lower, -1), ordinal);
         CREATE INDEX games_white_rating ON games(white_rating, ordinal);
         CREATE INDEX games_black_rating ON games(black_rating, ordinal);
         CREATE INDEX games_rating_sum ON games(average_rating_sum, ordinal);
         CREATE INDEX games_white_rating_sort_desc ON games(COALESCE(white_rating, -1), ordinal);
         CREATE INDEX games_black_rating_sort_desc ON games(COALESCE(black_rating, -1), ordinal);
         CREATE INDEX games_rating_sum_sort_desc ON games(COALESCE(average_rating_sum, -1), ordinal);
         CREATE INDEX games_event ON games(event_id, ordinal);
         CREATE INDEX games_event_name ON games(event_norm, ordinal);
         CREATE INDEX games_site ON games(site_norm, ordinal);
         CREATE INDEX games_eco ON games(eco, ordinal);
         CREATE INDEX games_time_control_category ON games(time_control_category, ordinal);
         CREATE INDEX games_plies ON games(plies, ordinal);
         CREATE INDEX player_names_exact ON player_names(normalized_name, player_id);
         CREATE INDEX events_name ON events(normalized_name, year, normalized_site, event_id);
         CREATE INDEX events_year ON events(year, event_id);
         CREATE INDEX events_site ON events(normalized_site, event_id);",
    )?;
    Ok(())
}

fn build_player_catalog(connection: &Connection) -> Result<(), CatalogError> {
    connection.execute_batch(
        "CREATE TEMP TABLE player_appearances AS
         SELECT identity_key, display_name, normalized_name, rating, date_lower, date_upper, ordinal
         FROM (
            SELECT white_identity_key AS identity_key, white_name AS display_name,
                   white_name_norm AS normalized_name,
                   white_rating AS rating, date_lower, date_upper, ordinal FROM games
            UNION ALL
            SELECT black_identity_key, black_name, black_name_norm, black_rating,
                   date_lower, date_upper, ordinal FROM games
         );
         INSERT INTO players (
            player_id, identity_key, display_name, game_count, min_rating, max_rating,
            first_date, last_date, fide_id
         )
         SELECT ROW_NUMBER() OVER (ORDER BY appearances.identity_key), appearances.identity_key,
                MIN(appearances.display_name), COUNT(DISTINCT appearances.ordinal),
                MIN(appearances.rating), MAX(appearances.rating),
                MIN(appearances.date_lower), MAX(appearances.date_upper),
                CASE WHEN appearances.identity_key LIKE 'masters:fide:%'
                     THEN substr(appearances.identity_key, 14) ELSE NULL END
         FROM player_appearances AS appearances
         GROUP BY appearances.identity_key ORDER BY appearances.identity_key;
         INSERT INTO player_names (player_id, display_name, normalized_name)
         SELECT DISTINCT players.player_id, appearances.display_name,
                appearances.normalized_name
         FROM player_appearances AS appearances
         JOIN players USING(identity_key)
         ORDER BY players.player_id, appearances.display_name;",
    )?;
    connection.execute_batch(
        "UPDATE games SET
            white_player_id = (SELECT player_id FROM players WHERE identity_key = white_identity_key),
            black_player_id = (SELECT player_id FROM players WHERE identity_key = black_identity_key);",
    )?;
    Ok(())
}

fn build_event_catalog(connection: &Connection) -> Result<(), CatalogError> {
    connection.execute_batch(
        "INSERT INTO events (
            event_id, event_key, display_name, normalized_name, year,
            site_name, normalized_site, game_count, first_date, last_date
         )
         SELECT ROW_NUMBER() OVER (ORDER BY event_key), event_key, MIN(event_name),
                MIN(event_norm), year,
                MIN(CASE WHEN site_name IS NOT source_id THEN site_name END),
                MIN(CASE WHEN site_name IS NOT source_id THEN site_norm END), COUNT(*),
                MIN(date_lower), MAX(date_upper)
         FROM games WHERE event_key IS NOT NULL
         GROUP BY event_key, year ORDER BY event_key;
         UPDATE games SET event_id = (
            SELECT events.event_id FROM events WHERE events.event_key = games.event_key
         );",
    )?;
    Ok(())
}

fn table_count(connection: &Connection, table: &str) -> Result<u64, CatalogError> {
    let query = format!("SELECT COUNT(*) FROM {table}");
    let count = connection.query_row(&query, [], |row| row.get::<_, i64>(0))?;
    u64::try_from(count)
        .map_err(|_| CatalogError::InvalidData(format!("Negative row count in {table} table")))
}

fn verify_dense_ordinals(
    connection: &Connection,
    table: &str,
    column: &str,
    expected_count: u64,
) -> Result<(), CatalogError> {
    let query = format!("SELECT MIN({column}), MAX({column}), COUNT(*) FROM {table}");
    let (minimum, maximum, count): (Option<i64>, Option<i64>, i64) =
        connection.query_row(&query, [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let expected_last = i64::try_from(expected_count)
        .ok()
        .and_then(|count| count.checked_sub(1));
    let expected_count = i64::try_from(expected_count).map_err(|_| {
        CatalogError::InvalidData(format!("{table} row count exceeds SQLite INTEGER"))
    })?;
    if count != expected_count
        || (expected_count > 0 && (minimum != Some(0) || maximum != expected_last))
        || (expected_count == 0 && (minimum.is_some() || maximum.is_some()))
    {
        return Err(CatalogError::InvalidData(format!(
            "{table}.{column} is not a dense zero-based ordinal map"
        )));
    }
    Ok(())
}

fn verify_dense_ids(
    connection: &Connection,
    table: &str,
    column: &str,
    expected_count: u64,
) -> Result<(), CatalogError> {
    let query = format!("SELECT MIN({column}), MAX({column}), COUNT(*) FROM {table}");
    let (minimum, maximum, count): (Option<i64>, Option<i64>, i64) =
        connection.query_row(&query, [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let expected_count = i64::try_from(expected_count).map_err(|_| {
        CatalogError::InvalidData(format!("{table} row count exceeds SQLite INTEGER"))
    })?;
    if count != expected_count
        || (expected_count > 0 && (minimum != Some(1) || maximum != Some(expected_count)))
        || (expected_count == 0 && (minimum.is_some() || maximum.is_some()))
    {
        return Err(CatalogError::InvalidData(format!(
            "{table}.{column} is not a dense one-based identifier map"
        )));
    }
    Ok(())
}

fn player_identity_key(
    collection: Collection,
    display_name: &str,
    fide_id: Option<&str>,
) -> String {
    match collection {
        Collection::MastersOtb => match fide_id {
            Some(id) => format!("masters:fide:{id}"),
            None => format!("masters:name:{}", hex(display_name.as_bytes())),
        },
        Collection::EliteOnline => {
            format!("elite-online:user:{}", display_name.to_ascii_lowercase())
        }
    }
}

fn event_identity_key(
    event: Option<&str>,
    year: Option<u32>,
    site: Option<&str>,
) -> Result<Option<String>, CatalogError> {
    event
        .filter(|name| !name.is_empty() && name != &"?")
        .map(|name| serde_json::to_string(&(name, year, site)))
        .transpose()
        .map_err(CatalogError::from)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use opening_explorer_core::{CompactMove, GameMetadata, GameResult};
    use tempfile::tempdir;

    #[test]
    fn per_game_urls_do_not_create_separate_event_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events #? test.sqlite");
        let mut builder =
            SearchCatalogBuilder::create(&path, Collection::MastersOtb, 2, 4).unwrap();
        for id in [1, 2] {
            let mut game = game(id, "White", "Black", Some("2024.01.01"));
            let url = format!("https://example.test/games/{id}");
            game.metadata.event = Some("Game Type Label".into());
            game.metadata.site = Some(url.clone());
            game.metadata.source_id = Some(url);
            builder.push(&game).unwrap();
        }
        builder.finish().unwrap();
        let catalog = SearchCatalog::open(&path).unwrap();
        assert_eq!(catalog.verify_integrity().unwrap().events, 1);
        let observed: (u32, Option<String>) = catalog
            .connection()
            .query_row("SELECT game_count, site_name FROM events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(observed, (2, None));
    }

    fn game(id: u64, white: &str, black: &str, date: Option<&str>) -> ParsedGame {
        ParsedGame {
            metadata: GameMetadata {
                id,
                collection: Collection::MastersOtb,
                white: white.to_owned(),
                black: black.to_owned(),
                white_elo: Some(2400),
                black_elo: Some(2500),
                date: date.map(str::to_owned),
                event: Some("Open 2024".to_owned()),
                site: Some("Oslo".to_owned()),
                round: Some("1".to_owned()),
                result: GameResult::WhiteWins,
                eco: Some("C20".to_owned()),
                time_control: Some("600+0".to_owned()),
                source_id: Some(format!("https://example.test/{id}")),
                broadcast_url: None,
                source_attribution: None,
            },
            moves: vec![CompactMove::default(); 4],
            fen_start: None,
        }
    }

    #[test]
    fn writes_deterministic_dense_ordinals_and_lossless_u64_ids() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let mut builder =
            SearchCatalogBuilder::create(&path, Collection::MastersOtb, 2, 4).unwrap();
        assert_eq!(
            builder
                .push(&game(u64::MAX, "A", "B", Some("2024.??.??")))
                .unwrap(),
            0
        );
        assert_eq!(
            builder
                .push(&game(7, "B", "C", Some("2024.02.29")))
                .unwrap(),
            1
        );
        let summary = builder.finish().unwrap();
        assert_eq!((summary.games, summary.players, summary.events), (2, 3, 1));

        let second_path = directory.path().join("catalog-second.sqlite");
        let mut second =
            SearchCatalogBuilder::create(&second_path, Collection::MastersOtb, 2, 4).unwrap();
        second
            .push(&game(u64::MAX, "A", "B", Some("2024.??.??")))
            .unwrap();
        second.push(&game(7, "B", "C", Some("2024.02.29"))).unwrap();
        second.finish().unwrap();
        assert_eq!(fs::read(&path).unwrap(), fs::read(&second_path).unwrap());

        let catalog = SearchCatalog::open(&path).unwrap();
        assert_eq!(catalog.verify_integrity().unwrap().games, 2);
        let connection = catalog.connection();
        let (ordinal, game_id, lower, upper, average, speed): (
            i64,
            Vec<u8>,
            i64,
            i64,
            i64,
            String,
        ) = connection
            .query_row(
                "SELECT ordinal, game_id, date_lower, date_upper, average_rating_sum,
                            time_control_category FROM games ORDER BY ordinal LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(ordinal, 0);
        assert_eq!(game_id.as_slice(), &u64::MAX.to_be_bytes());
        assert_eq!((lower, upper), (20240101, 20241231));
        assert_eq!(average, 4900);
        assert_eq!(speed, "rapid");
    }

    #[test]
    fn sidecar_evidence_derives_ids_and_conflicts_split_player_buckets() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let mut builder =
            SearchCatalogBuilder::create(&path, Collection::MastersOtb, 2, 2).unwrap();
        builder.push(&game(11, "Same Name", "Other", None)).unwrap();
        builder.push(&game(12, "Same Name", "Third", None)).unwrap();
        builder
            .add_attributions(
                11,
                &[GameSourceAttribution {
                    source_tag: Some("broadcast".to_owned()),
                    source_quality: None,
                    white_fide_id: Some("1234".to_owned()),
                    black_fide_id: None,
                    white_identity_evidence: Some("Confirmed".to_owned()),
                    black_identity_evidence: None,
                    mode: None,
                    white_title: Some("GM".to_owned()),
                    black_title: None,
                }],
            )
            .unwrap();
        builder
            .add_attributions(
                12,
                &[GameSourceAttribution {
                    source_tag: Some("second".to_owned()),
                    source_quality: None,
                    white_fide_id: Some("9999".to_owned()),
                    black_fide_id: None,
                    white_identity_evidence: Some("Strong".to_owned()),
                    black_identity_evidence: None,
                    mode: None,
                    white_title: None,
                    black_title: None,
                }],
            )
            .unwrap();
        builder.finish().unwrap();

        let catalog = SearchCatalog::open(&path).unwrap();
        let connection = catalog.connection();
        let identities: Vec<String> = {
            let mut statement = connection
                .prepare("SELECT identity_key FROM players WHERE identity_key LIKE 'masters:fide:%' ORDER BY identity_key")
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(identities, ["masters:fide:1234", "masters:fide:9999"]);
        let attribution_json: String = connection
            .query_row(
                "SELECT attributions_json FROM source_attributions WHERE game_id = ?1",
                [11_u64.to_be_bytes().as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(attribution_json.contains("Confirmed"));
        assert!(attribution_json.contains("GM"));
    }

    #[test]
    fn rejects_unmatched_and_duplicate_attribution_records() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let mut builder =
            SearchCatalogBuilder::create(&path, Collection::MastersOtb, 1, 2).unwrap();
        builder.push(&game(11, "A", "B", None)).unwrap();
        let attribution = GameSourceAttribution {
            source_tag: Some("source".to_owned()),
            source_quality: None,
            white_fide_id: None,
            black_fide_id: None,
            white_identity_evidence: None,
            black_identity_evidence: None,
            mode: None,
            white_title: None,
            black_title: None,
        };
        assert!(builder
            .add_attributions(10, std::slice::from_ref(&attribution))
            .is_err());
        builder
            .add_attributions(11, std::slice::from_ref(&attribution))
            .unwrap();
        assert!(builder.add_attributions(11, &[attribution]).is_err());
    }
}
