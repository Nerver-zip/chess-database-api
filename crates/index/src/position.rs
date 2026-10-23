use byteorder::{BigEndian, WriteBytesExt};
use codb_core::{CompactMove, GameResult, ParsedGame, PositionKey};
use rusqlite::{params, Connection};
use shakmaty::zobrist::Zobrist128;
use shakmaty::{Chess, EnPassantMode, Move, Position, Role, Square};
use std::collections::BTreeMap;
use std::io::Cursor;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum IndexError {
    #[error("Database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Zstd error: {0}")]
    Zstd(String),
    #[error("Reconstruction error: {0}")]
    Reconstruction(String),
}

/// Aggregated opening statistics for a candidate next move from a position.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MoveStats {
    pub compact_move: CompactMove,
    pub uci: String,
    pub white_wins: u32,
    pub draws: u32,
    pub black_wins: u32,
    pub total_games: u32,
}

/// Complete Opening Explorer query response for a position.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct PositionStats {
    pub position_key: String,
    pub total_games: u32,
    pub white_wins: u32,
    pub draws: u32,
    pub black_wins: u32,
    pub moves: Vec<MoveStats>,
}

/// Computes a canonical, 128-bit collision-resistant `PositionKey`.
pub fn compute_position_key(pos: &Chess) -> PositionKey {
    // Primary 128-bit Zobrist hash with strict legal en-passant mode
    let zobrist: Zobrist128 = pos.zobrist_hash(EnPassantMode::Legal);
    PositionKey(zobrist.0)
}

/// Extracts all `(PositionKey, CompactMove, GameResult)` entries from a game up to `max_plies`.
pub fn extract_game_positions(
    game: &ParsedGame,
    max_plies: usize,
) -> Result<Vec<(PositionKey, CompactMove, GameResult)>, IndexError> {
    let mut board = Chess::default();
    let limit = game.moves.len().min(max_plies);
    let mut entries = Vec::with_capacity(limit);

    for (i, &cm) in game.moves[..limit].iter().enumerate() {
        let key = compute_position_key(&board);

        // Advance board
        let from = Square::new(cm.from_sq() as u32);
        let to = Square::new(cm.to_sq() as u32);
        let promo = cm.promotion_piece().map(|p| match p {
            1 => Role::Knight,
            2 => Role::Bishop,
            3 => Role::Rook,
            4 => Role::Queen,
            _ => Role::Queen,
        });

        let legal_moves = board.legal_moves();
        let m = legal_moves
            .into_iter()
            .find(|cand| match cand {
                Move::Normal {
                    from: f,
                    to: t,
                    promotion,
                    ..
                } => *f == from && *t == to && *promotion == promo,
                Move::EnPassant { from: f, to: t } => *f == from && *t == to,
                Move::Castle { king, rook } => {
                    let to_sq = if rook.file() > king.file() {
                        if king.rank() == shakmaty::Rank::First {
                            Square::G1
                        } else {
                            Square::G8
                        }
                    } else {
                        if king.rank() == shakmaty::Rank::First {
                            Square::C1
                        } else {
                            Square::C8
                        }
                    };
                    *king == from && to_sq == to
                }
                _ => false,
            })
            .ok_or_else(|| {
                IndexError::Reconstruction(format!(
                    "Illegal move at ply {} ({})",
                    i + 1,
                    cm.to_uci()
                ))
            })?;

        board.play_unchecked(m);
        entries.push((key, cm, game.metadata.result));
    }

    Ok(entries)
}

/// SQLite Embedded Storage Engine.
pub struct SqlitePositionIndex {
    conn: Connection,
}

impl SqlitePositionIndex {
    pub fn in_memory() -> Result<Self, IndexError> {
        let conn = Connection::open_in_memory()?;
        Self::init_tables(&conn)?;
        Ok(SqlitePositionIndex { conn })
    }

    pub fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self, IndexError> {
        let conn = Connection::open(path)?;
        // Optimize for write performance and reads
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;
             PRAGMA temp_store = MEMORY;",
        )?;
        Self::init_tables(&conn)?;
        Ok(SqlitePositionIndex { conn })
    }

    fn init_tables(conn: &Connection) -> Result<(), IndexError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS position_moves (
                pos_primary INTEGER NOT NULL,
                pos_secondary INTEGER NOT NULL,
                move_u16 INTEGER NOT NULL,
                white_wins INTEGER NOT NULL DEFAULT 0,
                draws INTEGER NOT NULL DEFAULT 0,
                black_wins INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (pos_primary, pos_secondary, move_u16)
            );
            CREATE INDEX IF NOT EXISTS idx_pos ON position_moves (pos_primary, pos_secondary);",
        )?;
        Ok(())
    }

    pub fn insert_batch(
        &mut self,
        entries: &[(PositionKey, CompactMove, GameResult)],
    ) -> Result<(), IndexError> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO position_moves (pos_primary, pos_secondary, move_u16, white_wins, draws, black_wins)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(pos_primary, pos_secondary, move_u16) DO UPDATE SET
                    white_wins = white_wins + excluded.white_wins,
                    draws = draws + excluded.draws,
                    black_wins = black_wins + excluded.black_wins;",
            )?;

            for (key, cm, res) in entries {
                let w = if *res == GameResult::WhiteWins { 1 } else { 0 };
                let d = if *res == GameResult::Draw { 1 } else { 0 };
                let b = if *res == GameResult::BlackWins { 1 } else { 0 };

                stmt.execute(params![
                    key.primary() as i64,
                    key.secondary() as i64,
                    cm.0 as i32,
                    w,
                    d,
                    b
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn query_position(&self, key: PositionKey) -> Result<PositionStats, IndexError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT move_u16, white_wins, draws, black_wins
             FROM position_moves
             WHERE pos_primary = ?1 AND pos_secondary = ?2
             ORDER BY (white_wins + draws + black_wins) DESC;",
        )?;

        let mut rows = stmt.query(params![key.primary() as i64, key.secondary() as i64])?;
        let mut moves = Vec::new();
        let mut total_w = 0;
        let mut total_d = 0;
        let mut total_b = 0;

        while let Some(row) = rows.next()? {
            let m_val: i32 = row.get(0)?;
            let w: u32 = row.get(1)?;
            let d: u32 = row.get(2)?;
            let b: u32 = row.get(3)?;
            let total = w + d + b;

            total_w += w;
            total_d += d;
            total_b += b;

            let cm = CompactMove(m_val as u16);
            moves.push(MoveStats {
                compact_move: cm,
                uci: cm.to_uci(),
                white_wins: w,
                draws: d,
                black_wins: b,
                total_games: total,
            });
        }

        Ok(PositionStats {
            position_key: key.to_hex(),
            total_games: total_w + total_d + total_b,
            white_wins: total_w,
            draws: total_d,
            black_wins: total_b,
            moves,
        })
    }
}

/// Custom Immutable Block-Compressed Sorted Position Index (`.cpi`).
pub struct ImmutablePositionIndex {
    /// In-memory table of positions and aggregated next moves: Key -> (Move -> (W, D, B))
    data: BTreeMap<PositionKey, BTreeMap<CompactMove, (u32, u32, u32)>>,
}

impl Default for ImmutablePositionIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ImmutablePositionIndex {
    pub fn new() -> Self {
        ImmutablePositionIndex {
            data: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn record(&mut self, key: PositionKey, cm: CompactMove, res: GameResult) {
        let entry = self
            .data
            .entry(key)
            .or_default()
            .entry(cm)
            .or_insert((0, 0, 0));
        match res {
            GameResult::WhiteWins => entry.0 += 1,
            GameResult::Draw => entry.1 += 1,
            GameResult::BlackWins => entry.2 += 1,
            GameResult::Star => {}
        }
    }

    pub fn query(&self, key: PositionKey) -> Option<PositionStats> {
        let moves_map = self.data.get(&key)?;
        let mut moves = Vec::with_capacity(moves_map.len());
        let mut total_w = 0;
        let mut total_d = 0;
        let mut total_b = 0;

        for (&cm, &(w, d, b)) in moves_map {
            let total = w + d + b;
            total_w += w;
            total_d += d;
            total_b += b;

            moves.push(MoveStats {
                compact_move: cm,
                uci: cm.to_uci(),
                white_wins: w,
                draws: d,
                black_wins: b,
                total_games: total,
            });
        }

        // Sort descending by total games
        moves.sort_by_key(|a| std::cmp::Reverse(a.total_games));

        Some(PositionStats {
            position_key: key.to_hex(),
            total_games: total_w + total_d + total_b,
            white_wins: total_w,
            draws: total_d,
            black_wins: total_b,
            moves,
        })
    }

    /// Serializes the entire index to a packed, block-compressed binary file.
    pub fn serialize_compressed(&self, zstd_level: i32) -> Result<Vec<u8>, IndexError> {
        let mut buf = Vec::new();
        // Number of unique positions
        buf.write_u32::<BigEndian>(self.data.len() as u32)?;

        for (&key, moves_map) in &self.data {
            buf.write_u128::<BigEndian>(key.0)?;
            buf.write_u8(moves_map.len() as u8)?;
            for (&cm, &(w, d, b)) in moves_map {
                buf.write_u16::<BigEndian>(cm.0)?;
                buf.write_u32::<BigEndian>(w)?;
                buf.write_u32::<BigEndian>(d)?;
                buf.write_u32::<BigEndian>(b)?;
            }
        }

        zstd::encode_all(Cursor::new(buf), zstd_level).map_err(|e| IndexError::Zstd(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codb_core::{Collection, GameMetadata};

    #[test]
    fn test_transposition_position_key_identity() {
        // Path 1: 1. d4 Nf6 2. c4 e6
        let mut b1 = Chess::default();
        b1.play_unchecked(Move::Normal {
            role: Role::Pawn,
            from: Square::D2,
            to: Square::D4,
            capture: None,
            promotion: None,
        });
        b1.play_unchecked(Move::Normal {
            role: Role::Knight,
            from: Square::G8,
            to: Square::F6,
            capture: None,
            promotion: None,
        });
        b1.play_unchecked(Move::Normal {
            role: Role::Pawn,
            from: Square::C2,
            to: Square::C4,
            capture: None,
            promotion: None,
        });
        b1.play_unchecked(Move::Normal {
            role: Role::Pawn,
            from: Square::E7,
            to: Square::E6,
            capture: None,
            promotion: None,
        });

        // Path 2: 1. c4 e6 2. d4 Nf6 (English Opening transposing into Nimzo/Queen's Indian)
        let mut b2 = Chess::default();
        b2.play_unchecked(Move::Normal {
            role: Role::Pawn,
            from: Square::C2,
            to: Square::C4,
            capture: None,
            promotion: None,
        });
        b2.play_unchecked(Move::Normal {
            role: Role::Pawn,
            from: Square::E7,
            to: Square::E6,
            capture: None,
            promotion: None,
        });
        b2.play_unchecked(Move::Normal {
            role: Role::Pawn,
            from: Square::D2,
            to: Square::D4,
            capture: None,
            promotion: None,
        });
        b2.play_unchecked(Move::Normal {
            role: Role::Knight,
            from: Square::G8,
            to: Square::F6,
            capture: None,
            promotion: None,
        });

        let key1 = compute_position_key(&b1);
        let key2 = compute_position_key(&b2);

        assert_eq!(
            key1, key2,
            "Transposed positions must yield identical PositionKey!"
        );
    }

    #[test]
    fn test_sqlite_and_immutable_index() {
        let mut sqlite = SqlitePositionIndex::in_memory().unwrap();
        let mut immutable = ImmutablePositionIndex::new();

        let game = ParsedGame {
            metadata: GameMetadata {
                id: 1,
                collection: Collection::MastersOtb,
                white: "A".into(),
                black: "B".into(),
                white_elo: Some(2500),
                black_elo: Some(2500),
                date: Some("2024.01.01".into()),
                event: Some("Test".into()),
                site: None,
                round: None,
                result: GameResult::WhiteWins,
                eco: None,
                time_control: None,
                source_id: None,
            },
            moves: vec![
                CompactMove::new(12, 28, None), // e2e4
                CompactMove::new(52, 36, None), // e7e5
            ],
            fen_start: None,
        };

        let entries = extract_game_positions(&game, 20).unwrap();
        assert_eq!(entries.len(), 2);

        sqlite.insert_batch(&entries).unwrap();
        for &(k, cm, r) in &entries {
            immutable.record(k, cm, r);
        }

        // Query starting position (e2e4 was played)
        let start_key = entries[0].0;
        let stats_sql = sqlite.query_position(start_key).unwrap();
        assert_eq!(stats_sql.total_games, 1);
        assert_eq!(stats_sql.white_wins, 1);
        assert_eq!(stats_sql.moves.len(), 1);
        assert_eq!(stats_sql.moves[0].uci, "e2e4");

        let stats_imm = immutable.query(start_key).unwrap();
        assert_eq!(stats_imm.total_games, 1);
        assert_eq!(stats_imm.white_wins, 1);
        assert_eq!(stats_imm.moves[0].uci, "e2e4");

        // Compress immutable index
        let compressed = immutable.serialize_compressed(3).unwrap();
        assert!(!compressed.is_empty());
    }
}
