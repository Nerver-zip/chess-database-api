use codb_core::{CompactMove, ParsedGame};
use std::collections::HashMap;
use std::path::Path;

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
        format!(
            "{}|{}|{}|{}|{}",
            self.white_norm,
            self.black_norm,
            self.year.map(|y| y.to_string()).unwrap_or_default(),
            self.event_norm,
            self.round.as_deref().unwrap_or_default()
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
        }
    }

    pub fn evaluate(&mut self, game: &ParsedGame) -> DuplicateVerdict {
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
                    return DuplicateVerdict::ConfirmedDuplicate {
                        canonical_id: existing.id,
                        reason: "Exact metadata and verified move sequence match".into(),
                    };
                }

                // Check for verified prefix relationship
                if ply_count > existing.moves.len() {
                    // Check if existing shorter game is a strict prefix of candidate game
                    if existing.moves == game.moves[..existing.moves.len()] {
                        let old_id = existing.id;
                        let old_hash = existing.move_hash;

                        // Update metadata record to point to the new fuller game
                        existing.id = game.metadata.id;
                        existing.move_hash = move_hash;
                        existing.moves = game.moves.clone();
                        existing.has_full_date = has_full_date;
                        existing.has_ratings = has_ratings;

                        // Ensure index consistency in by_moves: remove old reference, add new
                        if let Some(list) = self.by_moves.get_mut(&old_hash) {
                            list.retain(|r| r.id != old_id);
                        }
                        self.by_moves
                            .entry(move_hash)
                            .or_default()
                            .push(existing.clone());

                        return DuplicateVerdict::SupersedesPrevious {
                            old_id,
                            reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission"
                                .into(),
                        };
                    }
                } else if ply_count < existing.moves.len() {
                    // Check if candidate game is a strict prefix of existing longer game
                    if game.moves == existing.moves[..ply_count] {
                        return DuplicateVerdict::ConfirmedDuplicate {
                            canonical_id: existing.id,
                            reason: "Verified prefix relationship: truncated candidate discarded in favor of existing longer transmission"
                                .into(),
                        };
                    }
                }
            }
        }

        // 2. Check for matches by move hash across all games (transpositions & book openings)
        if let Some(existing_games) = self.by_moves.get(&move_hash) {
            for existing in existing_games {
                // Verify move equality to protect against hash collisions
                if existing.moves == game.moves && existing.metadata_key != meta_key {
                    let record = SeenGameRecord {
                        id: game.metadata.id,
                        move_hash,
                        moves: game.moves.clone(),
                        metadata_key: meta_key.clone(),
                        has_full_date,
                        has_ratings,
                    };
                    self.by_metadata
                        .entry(meta_key)
                        .or_default()
                        .push(record.clone());
                    self.by_moves.entry(move_hash).or_default().push(record);
                    return DuplicateVerdict::TranspositionDifferentGame;
                }
            }
        }

        // 3. Entirely unique game
        let record = SeenGameRecord {
            id: game.metadata.id,
            move_hash,
            moves: game.moves.clone(),
            metadata_key: meta_key.clone(),
            has_full_date,
            has_ratings,
        };

        self.by_metadata
            .entry(meta_key)
            .or_default()
            .push(record.clone());
        self.by_moves.entry(move_hash).or_default().push(record);

        DuplicateVerdict::Unique
    }
}

/// Disk-backed temporary deduplication store using embedded SQLite.
/// Scales to millions of games with bounded RAM consumption (<64 MB cache).
pub struct DiskDedupeStore {
    conn: rusqlite::Connection,
}

impl DiskDedupeStore {
    pub fn open(db_path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = rusqlite::Connection::open(db_path)?;
        Self::init_connection(conn)
    }

    pub fn temp() -> Result<Self, rusqlite::Error> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Self::init_connection(conn)
    }

    fn init_connection(conn: rusqlite::Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch(
            "PRAGMA synchronous = OFF;
             PRAGMA journal_mode = OFF;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -32000;
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

    pub fn evaluate(&mut self, game: &ParsedGame) -> DuplicateVerdict {
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

        // 1. Check for duplicates/prefixes within the same normalized metadata
        {
            let mut stmt = match self
                .conn
                .prepare("SELECT id, move_hash, moves FROM seen_games WHERE meta_key = ?1")
            {
                Ok(s) => s,
                Err(_) => return DuplicateVerdict::Unique,
            };

            let rows = stmt.query_map([&meta_key_str], |row| {
                let id: i64 = row.get(0)?;
                let m_hash: i64 = row.get(1)?;
                let raw_blob: Vec<u8> = row.get(2)?;
                Ok((id as u64, m_hash, raw_blob))
            });

            if let Ok(records) = rows {
                for rec_res in records.flatten() {
                    let (existing_id, _existing_hash, existing_blob) = rec_res;
                    let existing_moves = decode_moves_blob(&existing_blob);

                    if existing_moves == game.moves {
                        return DuplicateVerdict::ConfirmedDuplicate {
                            canonical_id: existing_id,
                            reason: "Exact metadata and verified move sequence match".into(),
                        };
                    }

                    if ply_count > existing_moves.len() {
                        if existing_moves == game.moves[..existing_moves.len()] {
                            // Supersedes prior game! Update SQLite record in-place
                            let _ = self.conn.execute(
                                "UPDATE seen_games SET id = ?1, move_hash = ?2, moves = ?3, has_full_date = ?4, has_ratings = ?5 WHERE id = ?6",
                                rusqlite::params![
                                    game.metadata.id as i64,
                                    move_hash,
                                    moves_blob,
                                    has_full_date,
                                    has_ratings,
                                    existing_id as i64
                                ],
                            );
                            return DuplicateVerdict::SupersedesPrevious {
                                old_id: existing_id,
                                reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission".into(),
                            };
                        }
                    } else if ply_count < existing_moves.len()
                        && game.moves == existing_moves[..ply_count]
                    {
                        return DuplicateVerdict::ConfirmedDuplicate {
                            canonical_id: existing_id,
                            reason: "Verified prefix relationship: truncated candidate discarded in favor of existing longer transmission".into(),
                        };
                    }
                }
            }
        }

        // 2. Check for move hash matches across different metadata (transpositions)
        {
            let mut stmt = match self
                .conn
                .prepare("SELECT id, meta_key, moves FROM seen_games WHERE move_hash = ?1")
            {
                Ok(s) => s,
                Err(_) => return DuplicateVerdict::Unique,
            };

            let rows = stmt.query_map([move_hash], |row| {
                let id: i64 = row.get(0)?;
                let m_key: String = row.get(1)?;
                let raw_blob: Vec<u8> = row.get(2)?;
                Ok((id as u64, m_key, raw_blob))
            });

            if let Ok(records) = rows {
                for rec_res in records.flatten() {
                    let (_existing_id, existing_meta_key, existing_blob) = rec_res;
                    let existing_moves = decode_moves_blob(&existing_blob);

                    if existing_moves == game.moves && existing_meta_key != meta_key_str {
                        // Legitimate transposition: insert candidate
                        let _ = self.conn.execute(
                            "INSERT INTO seen_games (id, move_hash, meta_key, has_full_date, has_ratings, moves) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            rusqlite::params![
                                game.metadata.id as i64,
                                move_hash,
                                meta_key_str,
                                has_full_date,
                                has_ratings,
                                moves_blob
                            ],
                        );
                        return DuplicateVerdict::TranspositionDifferentGame;
                    }
                }
            }
        }

        // 3. Unique game: insert into store
        let _ = self.conn.execute(
            "INSERT INTO seen_games (id, move_hash, meta_key, has_full_date, has_ratings, moves) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                game.metadata.id as i64,
                move_hash,
                meta_key_str,
                has_full_date,
                has_ratings,
                moves_blob
            ],
        );

        DuplicateVerdict::Unique
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
    pub fn new_disk(db_path: &Path) -> Result<Self, rusqlite::Error> {
        let store = DiskDedupeStore::open(db_path)?;
        Ok(Deduplicator {
            backend: DedupeBackend::Disk(store),
        })
    }

    /// In-memory temporary disk deduplicator.
    pub fn new_temp_disk() -> Result<Self, rusqlite::Error> {
        let store = DiskDedupeStore::temp()?;
        Ok(Deduplicator {
            backend: DedupeBackend::Disk(store),
        })
    }

    /// Evaluates candidate game against deduplication policy.
    pub fn evaluate(&mut self, game: &ParsedGame) -> DuplicateVerdict {
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
    let white_norm = normalize_player_name(&game.metadata.white);
    let black_norm = normalize_player_name(&game.metadata.black);

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

#[cfg(test)]
mod tests {
    use super::*;
    use codb_core::{Collection, GameMetadata, GameResult};

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

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g2),
            DuplicateVerdict::ConfirmedDuplicate {
                canonical_id: 1,
                reason: "Exact metadata and verified move sequence match".into()
            }
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

        assert_eq!(dedupe.evaluate(&g_truncated), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g_full),
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
            dedupe.evaluate(&g_late_truncated),
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

        assert_eq!(disk_dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(
            disk_dedupe.evaluate(&g2),
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
            disk_dedupe.evaluate(&g_full),
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

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g2),
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

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(dedupe.evaluate(&g2), DuplicateVerdict::Unique);
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

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g2),
            DuplicateVerdict::SupersedesPrevious {
                old_id: 1,
                reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission".into()
            }
        );
        assert_eq!(
            dedupe.evaluate(&g3),
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

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(dedupe.evaluate(&g2), DuplicateVerdict::Unique);
    }
}
