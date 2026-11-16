use opening_explorer_core::{CompactMove, ParsedGame};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum DedupeError {
    #[error("Database operational failure in deduplication store: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Game ID collision: stable ID {id} was assigned to distinct games: existing '{existing_meta}' vs candidate '{new_meta}'")]
    IdCollision {
        id: u64,
        existing_meta: String,
        new_meta: String,
    },
    #[error("Deduplication schema or constraint error: {0}")]
    Constraint(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateVerdict {
    Unique,
    ConfirmedDuplicate { canonical_id: u64, reason: String },
    SupersedesPrevious { old_id: u64, reason: String },
    TranspositionDifferentGame,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedMetadataKey {
    pub white_norm: String,
    pub black_norm: String,
    pub year: Option<u16>,
    pub event_norm: String,
    pub round: Option<String>,
}

impl NormalizedMetadataKey {
    pub fn to_canonical_string(&self) -> String {
        // Sanitize '|' delimiter to prevent key ambiguity or injection
        let clean = |s: &str| s.replace('|', "/");
        format!(
            "{}|{}|{}|{}|{}",
            clean(&self.white_norm),
            clean(&self.black_norm),
            self.year.map(|y| y.to_string()).unwrap_or_default(),
            clean(&self.event_norm),
            clean(self.round.as_deref().unwrap_or_default())
        )
    }
}

#[derive(Debug, Clone)]
pub struct SeenGameRecord {
    pub id: u64,
    pub move_hash: u64,
    pub moves: Vec<CompactMove>,
    pub metadata_key: NormalizedMetadataKey,
    pub has_full_date: bool,
    pub has_ratings: bool,
}

/// In-memory deduplication store for fixtures and small tests.
pub struct MemoryDedupeStore {
    by_metadata: HashMap<NormalizedMetadataKey, Vec<SeenGameRecord>>,
    by_moves: HashMap<u64, Vec<SeenGameRecord>>,
    by_id: HashMap<u64, (NormalizedMetadataKey, Vec<CompactMove>)>,
}

impl Default for MemoryDedupeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryDedupeStore {
    pub fn new() -> Self {
        MemoryDedupeStore {
            by_metadata: HashMap::new(),
            by_moves: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    pub fn evaluate(&mut self, game: &ParsedGame) -> Result<DuplicateVerdict, DedupeError> {
        let move_hash = compute_move_hash(&game.moves);
        let meta_key = normalize_metadata(game);
        let ply_count = game.moves.len();
        let has_full_date = game
            .metadata
            .date
            .as_ref()
            .is_some_and(|d| !d.contains('?'));
        let has_ratings = game.metadata.white_elo.is_some() || game.metadata.black_elo.is_some();

        // 1. Check for duplicate or prefix relationships within the same metadata
        if let Some(existing_games) = self.by_metadata.get_mut(&meta_key) {
            for existing in existing_games.iter_mut() {
                // Exact move match (verified by actual move sequence, not hash alone)
                if existing.moves == game.moves {
                    return Ok(DuplicateVerdict::ConfirmedDuplicate {
                        canonical_id: existing.id,
                        reason: "Exact metadata and verified move sequence match".into(),
                    });
                }

                // Check for verified prefix relationship
                if ply_count > existing.moves.len() {
                    // Check if existing shorter game is a strict prefix of candidate game
                    if existing.moves == game.moves[..existing.moves.len()] {
                        let old_id = existing.id;
                        let old_hash = existing.move_hash;

                        // Check ID collision with unrelated game
                        if game.metadata.id != old_id {
                            if let Some((colliding_meta, _)) = self.by_id.get(&game.metadata.id) {
                                return Err(DedupeError::IdCollision {
                                    id: game.metadata.id,
                                    existing_meta: colliding_meta.to_canonical_string(),
                                    new_meta: meta_key.to_canonical_string(),
                                });
                            }
                        }

                        // Update metadata record to point to the new fuller game
                        existing.id = game.metadata.id;
                        existing.move_hash = move_hash;
                        existing.moves = game.moves.clone();
                        existing.has_full_date = has_full_date;
                        existing.has_ratings = has_ratings;

                        // Update by_id
                        self.by_id.remove(&old_id);
                        self.by_id
                            .insert(game.metadata.id, (meta_key.clone(), game.moves.clone()));

                        // Ensure index consistency in by_moves: remove old reference, add new
                        if let Some(list) = self.by_moves.get_mut(&old_hash) {
                            list.retain(|r| r.id != old_id);
                        }
                        self.by_moves
                            .entry(move_hash)
                            .or_default()
                            .push(existing.clone());

                        return Ok(DuplicateVerdict::SupersedesPrevious {
                            old_id,
                            reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission"
                                .into(),
                        });
                    }
                } else if ply_count < existing.moves.len() {
                    // Check if candidate game is a strict prefix of existing longer game
                    if game.moves == existing.moves[..ply_count] {
                        return Ok(DuplicateVerdict::ConfirmedDuplicate {
                            canonical_id: existing.id,
                            reason: "Verified prefix relationship: truncated candidate discarded in favor of existing longer transmission"
                                .into(),
                        });
                    }
                }
            }
        }

        // 2. Check for matches by move hash across all games (transpositions & book openings)
        if let Some(existing_games) = self.by_moves.get(&move_hash) {
            for existing in existing_games {
                // Verify move equality to protect against hash collisions
                if existing.moves == game.moves && existing.metadata_key != meta_key {
                    // Check ID collision
                    if let Some((colliding_meta, _)) = self.by_id.get(&game.metadata.id) {
                        return Err(DedupeError::IdCollision {
                            id: game.metadata.id,
                            existing_meta: colliding_meta.to_canonical_string(),
                            new_meta: meta_key.to_canonical_string(),
                        });
                    }

                    let record = SeenGameRecord {
                        id: game.metadata.id,
                        move_hash,
                        moves: game.moves.clone(),
                        metadata_key: meta_key.clone(),
                        has_full_date,
                        has_ratings,
                    };
                    self.by_metadata
                        .entry(meta_key.clone())
                        .or_default()
                        .push(record.clone());
                    self.by_moves.entry(move_hash).or_default().push(record);
                    self.by_id
                        .insert(game.metadata.id, (meta_key, game.moves.clone()));
                    return Ok(DuplicateVerdict::TranspositionDifferentGame);
                }
            }
        }

        // 3. Entirely unique game: check ID collision, then record
        if let Some((colliding_meta, _)) = self.by_id.get(&game.metadata.id) {
            return Err(DedupeError::IdCollision {
                id: game.metadata.id,
                existing_meta: colliding_meta.to_canonical_string(),
                new_meta: meta_key.to_canonical_string(),
            });
        }

        let record = SeenGameRecord {
            id: game.metadata.id,
            move_hash,
            moves: game.moves.clone(),
            metadata_key: meta_key.clone(),
            has_full_date,
            has_ratings,
        };

        self.by_metadata
            .entry(meta_key.clone())
            .or_default()
            .push(record.clone());
        self.by_moves.entry(move_hash).or_default().push(record);
        self.by_id
            .insert(game.metadata.id, (meta_key, game.moves.clone()));

        Ok(DuplicateVerdict::Unique)
    }
}

/// Disk-backed temporary deduplication store using embedded SQLite.
/// Scales to millions of games with bounded RAM consumption and a 256 KiB SQLite page cache.
pub struct DiskDedupeStore {
    conn: rusqlite::Connection,
}

impl DiskDedupeStore {
    pub fn open(db_path: &Path) -> Result<Self, DedupeError> {
        let conn = rusqlite::Connection::open(db_path)?;
        Self::init_connection(conn)
    }

    pub fn temp() -> Result<Self, DedupeError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Self::init_connection(conn)
    }

    fn init_connection(conn: rusqlite::Connection) -> Result<Self, DedupeError> {
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             PRAGMA journal_mode = WAL;
             PRAGMA temp_store = FILE;
             PRAGMA cache_size = -256;
             CREATE TABLE IF NOT EXISTS seen_games (
                 id INTEGER PRIMARY KEY,
                 move_hash INTEGER NOT NULL,
                 meta_key TEXT NOT NULL,
                 has_full_date INTEGER NOT NULL,
                 has_ratings INTEGER NOT NULL,
                 moves BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_seen_meta ON seen_games(meta_key);
             CREATE INDEX IF NOT EXISTS idx_seen_moves ON seen_games(move_hash);",
        )?;
        Ok(DiskDedupeStore { conn })
    }

    pub fn evaluate(&mut self, game: &ParsedGame) -> Result<DuplicateVerdict, DedupeError> {
        let move_hash = compute_move_hash(&game.moves) as i64;
        let meta_key_struct = normalize_metadata(game);
        let meta_key_str = meta_key_struct.to_canonical_string();
        let ply_count = game.moves.len();
        let has_full_date = (game
            .metadata
            .date
            .as_ref()
            .is_some_and(|d| !d.contains('?'))) as i32;
        let has_ratings =
            (game.metadata.white_elo.is_some() || game.metadata.black_elo.is_some()) as i32;

        let moves_blob = encode_moves_blob(&game.moves);

        let tx = self.conn.transaction()?;

        // 1. Check for duplicates/prefixes within the same normalized metadata
        {
            let mut stmt =
                tx.prepare("SELECT id, move_hash, moves FROM seen_games WHERE meta_key = ?1")?;
            let mut rows = stmt.query([&meta_key_str])?;

            while let Some(row) = rows.next()? {
                let existing_id: i64 = row.get(0)?;
                let _existing_hash: i64 = row.get(1)?;
                let existing_blob: Vec<u8> = row.get(2)?;
                let existing_moves = decode_moves_blob(&existing_blob);

                if existing_moves == game.moves {
                    drop(rows);
                    drop(stmt);
                    tx.rollback()?;
                    return Ok(DuplicateVerdict::ConfirmedDuplicate {
                        canonical_id: existing_id as u64,
                        reason: "Exact metadata and verified move sequence match".into(),
                    });
                }

                if ply_count > existing_moves.len() {
                    if existing_moves == game.moves[..existing_moves.len()] {
                        drop(rows);
                        drop(stmt);

                        // Check if new ID collides with an existing unrelated game
                        if game.metadata.id != existing_id as u64 {
                            let collision: Option<String> = match tx.query_row(
                                "SELECT meta_key FROM seen_games WHERE id = ?1",
                                [game.metadata.id as i64],
                                |r| r.get(0),
                            ) {
                                Ok(m) => Some(m),
                                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                                Err(e) => return Err(DedupeError::Database(e)),
                            };
                            if let Some(colliding_meta) = collision {
                                return Err(DedupeError::IdCollision {
                                    id: game.metadata.id,
                                    existing_meta: colliding_meta,
                                    new_meta: meta_key_str,
                                });
                            }
                        }

                        // Supersedes prior game! Update SQLite record in-place
                        tx.execute(
                            "UPDATE seen_games SET id = ?1, move_hash = ?2, moves = ?3, has_full_date = ?4, has_ratings = ?5 WHERE id = ?6",
                            rusqlite::params![
                                game.metadata.id as i64,
                                move_hash,
                                moves_blob,
                                has_full_date,
                                has_ratings,
                                existing_id
                            ],
                        )?;
                        tx.commit()?;
                        return Ok(DuplicateVerdict::SupersedesPrevious {
                            old_id: existing_id as u64,
                            reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission".into(),
                        });
                    }
                } else if ply_count < existing_moves.len()
                    && game.moves == existing_moves[..ply_count]
                {
                    drop(rows);
                    drop(stmt);
                    tx.rollback()?;
                    return Ok(DuplicateVerdict::ConfirmedDuplicate {
                        canonical_id: existing_id as u64,
                        reason: "Verified prefix relationship: truncated candidate discarded in favor of existing longer transmission".into(),
                    });
                }
            }
        }

        // 2. Check for move hash matches across different metadata (transpositions)
        {
            let mut stmt =
                tx.prepare("SELECT id, meta_key, moves FROM seen_games WHERE move_hash = ?1")?;
            let mut rows = stmt.query([move_hash])?;

            while let Some(row) = rows.next()? {
                let _existing_id: i64 = row.get(0)?;
                let existing_meta_key: String = row.get(1)?;
                let existing_blob: Vec<u8> = row.get(2)?;
                let existing_moves = decode_moves_blob(&existing_blob);

                if existing_moves == game.moves && existing_meta_key != meta_key_str {
                    drop(rows);
                    drop(stmt);

                    // Check for ID collision
                    let collision: Option<String> = match tx.query_row(
                        "SELECT meta_key FROM seen_games WHERE id = ?1",
                        [game.metadata.id as i64],
                        |r| r.get(0),
                    ) {
                        Ok(m) => Some(m),
                        Err(rusqlite::Error::QueryReturnedNoRows) => None,
                        Err(e) => return Err(DedupeError::Database(e)),
                    };
                    if let Some(colliding_meta) = collision {
                        return Err(DedupeError::IdCollision {
                            id: game.metadata.id,
                            existing_meta: colliding_meta,
                            new_meta: meta_key_str,
                        });
                    }

                    // Legitimate transposition: insert candidate
                    tx.execute(
                        "INSERT INTO seen_games (id, move_hash, meta_key, has_full_date, has_ratings, moves) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![
                            game.metadata.id as i64,
                            move_hash,
                            meta_key_str,
                            has_full_date,
                            has_ratings,
                            moves_blob
                        ],
                    )?;
                    tx.commit()?;
                    return Ok(DuplicateVerdict::TranspositionDifferentGame);
                }
            }
        }

        // 3. Unique game: check ID collision, then insert into store
        let collision: Option<String> = match tx.query_row(
            "SELECT meta_key FROM seen_games WHERE id = ?1",
            [game.metadata.id as i64],
            |r| r.get(0),
        ) {
            Ok(m) => Some(m),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(DedupeError::Database(e)),
        };
        if let Some(colliding_meta) = collision {
            return Err(DedupeError::IdCollision {
                id: game.metadata.id,
                existing_meta: colliding_meta,
                new_meta: meta_key_str,
            });
        }

        tx.execute(
            "INSERT INTO seen_games (id, move_hash, meta_key, has_full_date, has_ratings, moves) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                game.metadata.id as i64,
                move_hash,
                meta_key_str,
                has_full_date,
                has_ratings,
                moves_blob
            ],
        )?;
        tx.commit()?;

        Ok(DuplicateVerdict::Unique)
    }
}

fn encode_moves_blob(moves: &[CompactMove]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(moves.len() * 2);
    for m in moves {
        blob.extend_from_slice(&m.0.to_be_bytes());
    }
    blob
}

fn decode_moves_blob(blob: &[u8]) -> Vec<CompactMove> {
    blob.as_chunks::<2>()
        .0
        .iter()
        .map(|&[b0, b1]| CompactMove(u16::from_be_bytes([b0, b1])))
        .collect()
}

pub enum DedupeBackend {
    Memory(MemoryDedupeStore),
    Disk(DiskDedupeStore),
}

/// Unified deduplicator supporting in-memory mode (for CI/fixtures) and disk-backed mode (for millions of games).
pub struct Deduplicator {
    backend: DedupeBackend,
}

impl Default for Deduplicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Deduplicator {
    /// In-memory deduplicator (compatible with PR-01 default).
    pub fn new() -> Self {
        Deduplicator {
            backend: DedupeBackend::Memory(MemoryDedupeStore::new()),
        }
    }

    /// Disk-backed deduplicator with bounded RAM consumption using embedded SQLite at `db_path`.
    pub fn new_disk(db_path: &Path) -> Result<Self, DedupeError> {
        let store = DiskDedupeStore::open(db_path)?;
        Ok(Deduplicator {
            backend: DedupeBackend::Disk(store),
        })
    }

    /// In-memory temporary disk deduplicator.
    pub fn new_temp_disk() -> Result<Self, DedupeError> {
        let store = DiskDedupeStore::temp()?;
        Ok(Deduplicator {
            backend: DedupeBackend::Disk(store),
        })
    }

    /// Evaluates candidate game against deduplication policy.
    pub fn evaluate(&mut self, game: &ParsedGame) -> Result<DuplicateVerdict, DedupeError> {
        match &mut self.backend {
            DedupeBackend::Memory(mem) => mem.evaluate(game),
            DedupeBackend::Disk(disk) => disk.evaluate(game),
        }
    }
}

pub fn compute_move_hash(moves: &[CompactMove]) -> u64 {
    // 64-bit FNV-1a hash over compact 16-bit moves
    let mut hash: u64 = 0xcbf29ce484222325;
    for m in moves {
        let b1 = (m.0 & 0xFF) as u8;
        let b2 = ((m.0 >> 8) & 0xFF) as u8;
        hash ^= b1 as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        hash ^= b2 as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn normalize_player_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some((last, first)) = trimmed.split_once(',') {
        let last_clean: String = last
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        let first_initial = first
            .trim()
            .to_ascii_lowercase()
            .chars()
            .find(|c| c.is_alphabetic())
            .unwrap_or('_');
        format!("{},{}", last_clean, first_initial)
    } else {
        // Space separated e.g. "Magnus Carlsen"
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 {
            let last_clean: String = parts
                .last()
                .unwrap()
                .to_ascii_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            let first_initial = parts
                .first()
                .unwrap()
                .to_ascii_lowercase()
                .chars()
                .find(|c| c.is_alphabetic())
                .unwrap_or('_');
            format!("{},{}", last_clean, first_initial)
        } else {
            trimmed
                .to_ascii_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect()
        }
    }
}

pub fn normalize_metadata(game: &ParsedGame) -> NormalizedMetadataKey {
    let (white_identity, black_identity) = game
        .metadata
        .source_attribution
        .as_ref()
        .map(|source| {
            (
                source.white_identity_evidence.as_deref(),
                source.black_identity_evidence.as_deref(),
            )
        })
        .unwrap_or((None, None));
    let white_norm = normalize_dedupe_player_name(&game.metadata.white, white_identity);
    let black_norm = normalize_dedupe_player_name(&game.metadata.black, black_identity);

    let year = game.metadata.date.as_ref().and_then(|d| {
        let parts: Vec<&str> = d.split('.').collect();
        parts.first().and_then(|y| y.parse::<u16>().ok())
    });

    let event_norm = game
        .metadata
        .event
        .as_ref()
        .map(|e| e.trim().to_ascii_lowercase())
        .unwrap_or_default();

    let round = game.metadata.round.as_ref().map(|r| r.trim().to_string());

    NormalizedMetadataKey {
        white_norm,
        black_norm,
        year,
        event_norm,
        round,
    }
}

fn normalize_dedupe_player_name(name: &str, identity_evidence: Option<&str>) -> String {
    if identity_evidence.is_none_or(|value| {
        value.eq_ignore_ascii_case("Strong") || value.eq_ignore_ascii_case("Confirmed")
    }) {
        return normalize_player_name(name);
    }

    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opening_explorer_core::{Collection, GameMetadata, GameResult, GameSourceAttribution};

    fn make_test_game(
        id: u64,
        white: &str,
        black: &str,
        date: &str,
        event: &str,
        moves: Vec<CompactMove>,
    ) -> ParsedGame {
        ParsedGame {
            metadata: GameMetadata {
                id,
                collection: Collection::MastersOtb,
                white: white.into(),
                black: black.into(),
                white_elo: Some(2500),
                black_elo: Some(2480),
                date: Some(date.into()),
                event: Some(event.into()),
                site: None,
                round: Some("1".into()),
                result: GameResult::Draw,
                eco: Some("B12".into()),
                time_control: None,
                source_id: None,
                broadcast_url: None,
                source_attribution: None,
            },
            moves,
            fen_start: None,
        }
    }

    #[test]
    fn test_exact_duplicate_detection() {
        let mut dedupe = Deduplicator::new();
        let moves = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
        ];

        let g1 = make_test_game(
            1,
            "Carlsen, Magnus",
            "Nakamura, Hikaru",
            "2024.01.01",
            "World Blitz",
            moves.clone(),
        );
        let g2 = make_test_game(
            2,
            "Carlsen, M.",
            "Nakamura, H",
            "2024.01.01",
            "World Blitz",
            moves,
        );

        assert_eq!(dedupe.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g2).unwrap(),
            DuplicateVerdict::ConfirmedDuplicate {
                canonical_id: 1,
                reason: "Exact metadata and verified move sequence match".into()
            }
        );
    }

    #[test]
    fn weak_identity_names_keep_full_names_in_dedupe_key() {
        let moves = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
        ];
        let mut first = make_test_game(
            91,
            "Smith, John",
            "Opponent, Alex",
            "2024.01.01",
            "Same Event",
            moves.clone(),
        );
        let mut second = make_test_game(
            92,
            "Smith, James",
            "Opponent, Alex",
            "2024.01.01",
            "Same Event",
            moves,
        );
        let weak_source = || GameSourceAttribution {
            source_tag: Some("Historical Archive".into()),
            source_quality: None,
            white_fide_id: None,
            black_fide_id: None,
            white_identity_evidence: Some("NoMatch".into()),
            black_identity_evidence: Some("NoMatch".into()),
            mode: None,
            white_title: None,
            black_title: None,
        };
        first.metadata.source_attribution = Some(weak_source());
        second.metadata.source_attribution = Some(weak_source());

        assert_ne!(normalize_metadata(&first), normalize_metadata(&second));
        let mut dedupe = Deduplicator::new();
        assert_eq!(dedupe.evaluate(&first).unwrap(), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&second).unwrap(),
            DuplicateVerdict::TranspositionDifferentGame
        );
    }

    #[test]
    fn test_verified_prefix_superseding() {
        let mut dedupe = Deduplicator::new();
        let m1 = vec![CompactMove::new(12, 28, None)];
        let m2 = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
        ];

        let g_truncated = make_test_game(
            10,
            "Caruana, Fabiano",
            "So, Wesley",
            "2024.05.10",
            "Superbet Classic",
            m1,
        );
        let g_full = make_test_game(
            20,
            "Caruana, F",
            "So, W",
            "2024.05.10",
            "Superbet Classic",
            m2.clone(),
        );

        assert_eq!(
            dedupe.evaluate(&g_truncated).unwrap(),
            DuplicateVerdict::Unique
        );
        assert_eq!(
            dedupe.evaluate(&g_full).unwrap(),
            DuplicateVerdict::SupersedesPrevious {
                old_id: 10,
                reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission".into()
            }
        );

        // Another candidate arriving with truncated moves should be discarded
        let g_late_truncated = make_test_game(
            30,
            "Caruana, Fabiano",
            "So, Wesley",
            "2024.05.10",
            "Superbet Classic",
            vec![CompactMove::new(12, 28, None)],
        );
        assert_eq!(
            dedupe.evaluate(&g_late_truncated).unwrap(),
            DuplicateVerdict::ConfirmedDuplicate {
                canonical_id: 20,
                reason: "Verified prefix relationship: truncated candidate discarded in favor of existing longer transmission".into()
            }
        );
    }

    #[test]
    fn test_disk_backed_deduplicator_equivalence() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("dedupe.db");
        let mut disk_dedupe = Deduplicator::new_disk(&db_path).unwrap();

        let moves = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
        ];

        let g1 = make_test_game(
            1,
            "Carlsen, Magnus",
            "Nakamura, Hikaru",
            "2024.01.01",
            "World Blitz",
            moves.clone(),
        );
        let g2 = make_test_game(
            2,
            "Carlsen, M.",
            "Nakamura, H",
            "2024.01.01",
            "World Blitz",
            moves.clone(),
        );

        assert_eq!(disk_dedupe.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);
        assert_eq!(
            disk_dedupe.evaluate(&g2).unwrap(),
            DuplicateVerdict::ConfirmedDuplicate {
                canonical_id: 1,
                reason: "Exact metadata and verified move sequence match".into()
            }
        );

        // Test prefix superseding on disk
        let m_full = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
            CompactMove::new(6, 21, None),
        ];
        let g_full = make_test_game(
            3,
            "Carlsen, Magnus",
            "Nakamura, Hikaru",
            "2024.01.01",
            "World Blitz",
            m_full,
        );
        assert_eq!(
            disk_dedupe.evaluate(&g_full).unwrap(),
            DuplicateVerdict::SupersedesPrevious {
                old_id: 1,
                reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission".into()
            }
        );
    }

    #[test]
    fn test_transposition_in_different_events() {
        let mut dedupe = Deduplicator::new();
        let moves = vec![
            CompactMove::new(12, 28, None), // 1. e4
            CompactMove::new(50, 34, None), // 1... c5 (Sicilian)
        ];

        let g1 = make_test_game(
            100,
            "Player A",
            "Player B",
            "2024.01.01",
            "Event 1",
            moves.clone(),
        );
        let g2 = make_test_game(200, "Player C", "Player D", "2024.02.01", "Event 2", moves);

        assert_eq!(dedupe.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g2).unwrap(),
            DuplicateVerdict::TranspositionDifferentGame
        );
    }

    #[test]
    fn test_divergent_moves_do_not_supersede() {
        let mut dedupe = Deduplicator::new();
        let g1 = make_test_game(
            1,
            "Carlsen, M",
            "Nakamura, H",
            "2024.01.01",
            "Event",
            vec![
                CompactMove::new(12, 28, None),
                CompactMove::new(52, 36, None),
            ],
        );
        let g2 = make_test_game(
            2,
            "Carlsen, M",
            "Nakamura, H",
            "2024.01.01",
            "Event",
            vec![
                CompactMove::new(12, 28, None),
                CompactMove::new(50, 34, None),
            ],
        );

        assert_eq!(dedupe.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);
        assert_eq!(dedupe.evaluate(&g2).unwrap(), DuplicateVerdict::Unique);
    }

    #[test]
    fn test_multi_game_superseding_chain() {
        let mut dedupe = Deduplicator::new();
        let g1 = make_test_game(
            1,
            "A, P",
            "B, P",
            "2024",
            "Open",
            vec![CompactMove::new(12, 28, None)],
        );
        let g2 = make_test_game(
            2,
            "A, P",
            "B, P",
            "2024",
            "Open",
            vec![
                CompactMove::new(12, 28, None),
                CompactMove::new(52, 36, None),
            ],
        );
        let g3 = make_test_game(
            3,
            "A, P",
            "B, P",
            "2024",
            "Open",
            vec![
                CompactMove::new(12, 28, None),
                CompactMove::new(52, 36, None),
                CompactMove::new(6, 21, None),
            ],
        );

        assert_eq!(dedupe.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g2).unwrap(),
            DuplicateVerdict::SupersedesPrevious {
                old_id: 1,
                reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission".into()
            }
        );
        assert_eq!(
            dedupe.evaluate(&g3).unwrap(),
            DuplicateVerdict::SupersedesPrevious {
                old_id: 2,
                reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission".into()
            }
        );
    }

    #[test]
    fn test_hash_collision_protection() {
        let mut dedupe = Deduplicator::new();
        let m1 = vec![CompactMove::new(12, 28, None)];
        let m2 = vec![CompactMove::new(11, 27, None)];

        let g1 = make_test_game(1, "Player A", "Player B", "2024", "Event", m1);
        let g2 = make_test_game(2, "Player A", "Player B", "2024", "Event", m2);

        assert_eq!(dedupe.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);
        assert_eq!(dedupe.evaluate(&g2).unwrap(), DuplicateVerdict::Unique);
    }

    // --- Regression tests for F1 (Eliminate Silent SQLite Deduplication Failures) ---

    #[test]
    fn test_sqlite_operational_failure_fails_closed_no_unique_verdict() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("corrupted.db");
        let mut store = DiskDedupeStore::open(&db_path).unwrap();

        // Deliberately drop the table to simulate database/schema corruption
        store.conn.execute_batch("DROP TABLE seen_games;").unwrap();

        let g = make_test_game(
            1,
            "Player A",
            "Player B",
            "2024",
            "Event",
            vec![CompactMove::new(12, 28, None)],
        );

        // Must FAIL CLOSED with Database error, NEVER silently return DuplicateVerdict::Unique!
        let result = store.evaluate(&g);
        assert!(
            matches!(result, Err(DedupeError::Database(_))),
            "Expected Database error, got: {:?}",
            result
        );
    }

    #[test]
    fn test_id_collision_between_distinct_games_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("collision.db");
        let mut store = DiskDedupeStore::open(&db_path).unwrap();

        // Game 1 with ID 42
        let g1 = make_test_game(
            42,
            "Player Alpha",
            "Player Beta",
            "2024",
            "Event 1",
            vec![CompactMove::new(12, 28, None)],
        );
        // Game 2 with identical ID 42 but completely distinct players and moves
        let g2 = make_test_game(
            42,
            "Player Gamma",
            "Player Delta",
            "2025",
            "Event 2",
            vec![CompactMove::new(11, 27, None)],
        );

        assert_eq!(store.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);

        // Inserting distinct game with colliding ID must fail closed with IdCollision!
        let res2 = store.evaluate(&g2);
        assert!(
            matches!(res2, Err(DedupeError::IdCollision { id: 42, .. })),
            "Expected IdCollision error, got: {:?}",
            res2
        );
    }

    #[test]
    fn test_failed_transaction_rolls_back_cleanly() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("rollback.db");
        let mut store = DiskDedupeStore::open(&db_path).unwrap();

        let g1 = make_test_game(
            1,
            "Player A",
            "Player B",
            "2024",
            "Event",
            vec![CompactMove::new(12, 28, None)],
        );
        assert_eq!(store.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);

        // Check row count is 1
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM seen_games", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Now attempt an invalid operation (colliding ID)
        let g_colliding = make_test_game(
            1,
            "Player X",
            "Player Y",
            "2025",
            "Other",
            vec![CompactMove::new(50, 34, None)],
        );
        let res = store.evaluate(&g_colliding);
        assert!(res.is_err());

        // Verify transaction rolled back completely: row count is STILL 1
        let count_after: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM seen_games", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_after, 1);
    }

    #[test]
    fn test_reopening_database_after_failure_maintains_consistent_decisions() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("reopen.db");

        let g1 = make_test_game(
            100,
            "Carlsen, M",
            "Nakamura, H",
            "2024",
            "Blitz",
            vec![CompactMove::new(12, 28, None)],
        );

        {
            let mut store = DiskDedupeStore::open(&db_path).unwrap();
            assert_eq!(store.evaluate(&g1).unwrap(), DuplicateVerdict::Unique);
        }

        // Reopen database connection
        let mut reopened_store = DiskDedupeStore::open(&db_path).unwrap();

        // Exact match must still be confirmed duplicate
        let dup = reopened_store.evaluate(&g1).unwrap();
        assert_eq!(
            dup,
            DuplicateVerdict::ConfirmedDuplicate {
                canonical_id: 100,
                reason: "Exact metadata and verified move sequence match".into(),
            }
        );
    }

    #[test]
    fn test_delimiter_sanitization_in_metadata_key() {
        let key = NormalizedMetadataKey {
            white_norm: "Carlsen|Magnus".into(),
            black_norm: "Nakamura|Hikaru".into(),
            year: Some(2024),
            event_norm: "World|Blitz".into(),
            round: Some("1|A".into()),
        };
        let canonical = key.to_canonical_string();
        // Pipe delimiters within field values should be sanitized to '/'
        assert_eq!(
            canonical,
            "Carlsen/Magnus|Nakamura/Hikaru|2024|World/Blitz|1/A"
        );
    }
}
