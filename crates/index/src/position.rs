use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use codb_core::{CompactMove, GameResult, ParsedGame, PositionKey};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use shakmaty::zobrist::Zobrist128;
use shakmaty::{Chess, EnPassantMode, Move, Position, Role, Square};
use std::collections::BTreeMap;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::path::Path;
use thiserror::Error;

pub const CPI_MAGIC: &[u8; 8] = b"CODB_CPI";
pub const CPI_VERSION: u32 = 2;
pub const DEFAULT_POSITIONS_PER_BLOCK: usize = 256;
const HEADER_SIZE: usize = 64;

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
    #[error("Invalid CPI index format: {0}")]
    InvalidFormat(String),
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

    pub fn unique_pairs_count(&self) -> Result<usize, IndexError> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM position_moves;", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn unique_positions_count(&self) -> Result<usize, IndexError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT pos_primary || ':' || pos_secondary) FROM position_moves;",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
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

    pub fn unique_position_move_pairs(&self) -> usize {
        self.data.values().map(|moves| moves.len()).sum()
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

    /// Serializes the index to a persistent `.cpi` file with block-level compression and sparse index.
    pub fn write_to_file<P: AsRef<Path>>(
        &self,
        path: P,
        positions_per_block: usize,
        zstd_level: i32,
    ) -> Result<CpiMetadata, IndexError> {
        let file = std::fs::File::create(path)?;
        let mut writer = std::io::BufWriter::new(file);
        let meta = self.write_cpi(&mut writer, positions_per_block, zstd_level)?;
        writer.flush()?;
        Ok(meta)
    }

    /// Writes the CPI format to any seekable writer.
    pub fn write_cpi<W: Write + Seek>(
        &self,
        writer: &mut W,
        positions_per_block: usize,
        zstd_level: i32,
    ) -> Result<CpiMetadata, IndexError> {
        let pos_per_block = if positions_per_block == 0 {
            DEFAULT_POSITIONS_PER_BLOCK
        } else {
            positions_per_block
        };

        // Write placeholder header (48 bytes)
        let header_placeholder = [0u8; HEADER_SIZE];
        writer.write_all(&header_placeholder)?;

        let mut sparse_index = Vec::new();
        let mut block_uncompressed = Vec::new();
        let total_positions = self.data.len() as u64;
        let total_pairs = self.unique_position_move_pairs() as u64;

        type PositionEntry<'a> = (&'a PositionKey, &'a BTreeMap<CompactMove, (u32, u32, u32)>);
        let mut current_block_entries: Vec<PositionEntry<'_>> = Vec::with_capacity(pos_per_block);

        let mut flush_block = |entries: &[PositionEntry<'_>],
                               writer: &mut W,
                               sparse_index: &mut Vec<BlockIndexEntry>|
         -> Result<(), IndexError> {
            if entries.is_empty() {
                return Ok(());
            }
            block_uncompressed.clear();
            block_uncompressed.write_u16::<BigEndian>(entries.len() as u16)?;
            for (&key, moves_map) in entries {
                block_uncompressed.write_u128::<BigEndian>(key.0)?;
                block_uncompressed.write_u8(moves_map.len() as u8)?;
                for (&cm, &(w, d, b)) in *moves_map {
                    block_uncompressed.write_u16::<BigEndian>(cm.0)?;
                    block_uncompressed.write_u32::<BigEndian>(w)?;
                    block_uncompressed.write_u32::<BigEndian>(d)?;
                    block_uncompressed.write_u32::<BigEndian>(b)?;
                }
            }

            let uncompressed_len = block_uncompressed.len() as u32;
            let compressed = zstd::encode_all(Cursor::new(&block_uncompressed), zstd_level)
                .map_err(|e| IndexError::Zstd(e.to_string()))?;
            let compressed_len = compressed.len() as u32;

            let file_offset = writer.stream_position()?;
            writer.write_all(&compressed)?;

            sparse_index.push(BlockIndexEntry {
                first_key: entries[0].0 .0,
                file_offset,
                compressed_len,
                uncompressed_len,
                num_positions: entries.len() as u16,
            });

            Ok(())
        };

        for (key, moves) in &self.data {
            current_block_entries.push((key, moves));
            if current_block_entries.len() >= pos_per_block {
                flush_block(&current_block_entries, writer, &mut sparse_index)?;
                current_block_entries.clear();
            }
        }

        if !current_block_entries.is_empty() {
            flush_block(&current_block_entries, writer, &mut sparse_index)?;
            current_block_entries.clear();
        }

        // Write sparse index table
        let sparse_index_offset = writer.stream_position()?;
        for entry in &sparse_index {
            writer.write_u128::<BigEndian>(entry.first_key)?;
            writer.write_u64::<BigEndian>(entry.file_offset)?;
            writer.write_u32::<BigEndian>(entry.compressed_len)?;
            writer.write_u32::<BigEndian>(entry.uncompressed_len)?;
            writer.write_u16::<BigEndian>(entry.num_positions)?;
        }
        let total_file_size = writer.stream_position()?;
        let sparse_index_len = total_file_size - sparse_index_offset;

        // Seek to 0 and write header (64 bytes)
        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(CPI_MAGIC)?;
        writer.write_u32::<BigEndian>(CPI_VERSION)?;
        writer.write_u32::<BigEndian>(pos_per_block as u32)?;
        writer.write_u64::<BigEndian>(total_positions)?;
        writer.write_u64::<BigEndian>(total_pairs)?;
        writer.write_u32::<BigEndian>(sparse_index.len() as u32)?;
        writer.write_u32::<BigEndian>(0)?; // reserved 4 bytes
        writer.write_u64::<BigEndian>(sparse_index_offset)?;
        writer.write_u64::<BigEndian>(sparse_index_len)?;
        writer.write_u64::<BigEndian>(0)?; // flags 8 bytes

        // Seek back to end
        writer.seek(SeekFrom::Start(total_file_size))?;

        Ok(CpiMetadata {
            total_positions,
            total_pairs,
            block_count: sparse_index.len() as u32,
            total_file_size,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockIndexEntry {
    pub first_key: u128,
    pub file_offset: u64,
    pub compressed_len: u32,
    pub uncompressed_len: u32,
    pub num_positions: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpiMetadata {
    pub total_positions: u64,
    pub total_pairs: u64,
    pub block_count: u32,
    pub total_file_size: u64,
}

pub struct DiskPositionIndex {
    mmap: memmap2::Mmap,
    sparse_index: Vec<BlockIndexEntry>,
    total_positions: u64,
    total_pairs: u64,
}

impl DiskPositionIndex {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_mmap(mmap)
    }

    pub fn from_mmap(mmap: memmap2::Mmap) -> Result<Self, IndexError> {
        if mmap.len() < HEADER_SIZE {
            return Err(IndexError::InvalidFormat(
                "File too short for CPI header".into(),
            ));
        }
        if &mmap[0..8] != CPI_MAGIC {
            return Err(IndexError::InvalidFormat("Invalid CPI magic header".into()));
        }
        let mut cursor = Cursor::new(&mmap[8..]);
        let version = cursor.read_u32::<BigEndian>()?;
        if version != CPI_VERSION {
            return Err(IndexError::InvalidFormat(format!(
                "Unsupported CPI version: {}",
                version
            )));
        }
        let _pos_per_block = cursor.read_u32::<BigEndian>()?;
        let total_positions = cursor.read_u64::<BigEndian>()?;
        let total_pairs = cursor.read_u64::<BigEndian>()?;
        let block_count = cursor.read_u32::<BigEndian>()?;
        let _reserved = cursor.read_u32::<BigEndian>()?;
        let sparse_index_offset = cursor.read_u64::<BigEndian>()? as usize;
        let sparse_index_len = cursor.read_u64::<BigEndian>()? as usize;
        let _flags = cursor.read_u64::<BigEndian>()?;

        if sparse_index_offset + sparse_index_len > mmap.len() {
            return Err(IndexError::InvalidFormat(
                "Sparse index bounds exceed file size".into(),
            ));
        }

        let mut sparse_index = Vec::with_capacity(block_count as usize);
        let mut idx_cursor =
            Cursor::new(&mmap[sparse_index_offset..sparse_index_offset + sparse_index_len]);
        for _ in 0..block_count {
            let first_key = idx_cursor.read_u128::<BigEndian>()?;
            let file_offset = idx_cursor.read_u64::<BigEndian>()?;
            let compressed_len = idx_cursor.read_u32::<BigEndian>()?;
            let uncompressed_len = idx_cursor.read_u32::<BigEndian>()?;
            let num_positions = idx_cursor.read_u16::<BigEndian>()?;
            sparse_index.push(BlockIndexEntry {
                first_key,
                file_offset,
                compressed_len,
                uncompressed_len,
                num_positions,
            });
        }

        Ok(DiskPositionIndex {
            mmap,
            sparse_index,
            total_positions,
            total_pairs,
        })
    }

    pub fn total_positions(&self) -> u64 {
        self.total_positions
    }

    pub fn total_pairs(&self) -> u64 {
        self.total_pairs
    }

    pub fn block_count(&self) -> usize {
        self.sparse_index.len()
    }

    pub fn query(&self, key: PositionKey) -> Result<Option<PositionStats>, IndexError> {
        if self.sparse_index.is_empty() || key.0 < self.sparse_index[0].first_key {
            return Ok(None);
        }

        let idx = self
            .sparse_index
            .partition_point(|entry| entry.first_key <= key.0);
        if idx == 0 {
            return Ok(None);
        }
        let block_entry = &self.sparse_index[idx - 1];

        let start = block_entry.file_offset as usize;
        let end = start + block_entry.compressed_len as usize;
        if end > self.mmap.len() {
            return Err(IndexError::InvalidFormat(
                "Block bounds exceed file size".into(),
            ));
        }

        let compressed_slice = &self.mmap[start..end];
        let decompressed = zstd::decode_all(compressed_slice)
            .map_err(|e| IndexError::Zstd(format!("Failed to decompress CPI block: {}", e)))?;

        let mut cursor = Cursor::new(&decompressed);
        let num_positions = cursor.read_u16::<BigEndian>()?;
        for _ in 0..num_positions {
            let k = cursor.read_u128::<BigEndian>()?;
            let num_moves = cursor.read_u8()? as usize;
            if k == key.0 {
                let mut moves = Vec::with_capacity(num_moves);
                let mut total_w = 0;
                let mut total_d = 0;
                let mut total_b = 0;
                for _ in 0..num_moves {
                    let m_val = cursor.read_u16::<BigEndian>()?;
                    let w = cursor.read_u32::<BigEndian>()?;
                    let d = cursor.read_u32::<BigEndian>()?;
                    let b = cursor.read_u32::<BigEndian>()?;
                    let total = w + d + b;
                    total_w += w;
                    total_d += d;
                    total_b += b;
                    let cm = CompactMove(m_val);
                    moves.push(MoveStats {
                        compact_move: cm,
                        uci: cm.to_uci(),
                        white_wins: w,
                        draws: d,
                        black_wins: b,
                        total_games: total,
                    });
                }
                moves.sort_by_key(|a| std::cmp::Reverse(a.total_games));
                return Ok(Some(PositionStats {
                    position_key: key.to_hex(),
                    total_games: total_w + total_d + total_b,
                    white_wins: total_w,
                    draws: total_d,
                    black_wins: total_b,
                    moves,
                }));
            } else if k > key.0 {
                return Ok(None);
            } else {
                cursor.set_position(cursor.position() + (num_moves * 14) as u64);
            }
        }

        Ok(None)
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
        assert_eq!(sqlite.unique_pairs_count().unwrap(), 2);
        assert_eq!(sqlite.unique_positions_count().unwrap(), 2);
        assert_eq!(immutable.unique_position_move_pairs(), 2);

        // Compress immutable index
        let compressed = immutable.serialize_compressed(3).unwrap();
        assert!(!compressed.is_empty());
    }

    #[test]
    fn test_disk_position_index_round_trip() {
        let mut index = ImmutablePositionIndex::new();
        let key1 = PositionKey(0x11112222333344445555666677778888);
        let key2 = PositionKey(0x22223333444455556666777788889999);
        let key3 = PositionKey(0x3333444455556666777788889999aaaa);

        let m1 = CompactMove::new(12, 28, None);
        let m2 = CompactMove::new(11, 27, None);
        let m3 = CompactMove::new(10, 26, None);

        index.record(key1, m1, GameResult::WhiteWins);
        index.record(key1, m2, GameResult::Draw);
        index.record(key2, m3, GameResult::BlackWins);
        index.record(key3, m1, GameResult::WhiteWins);

        assert_eq!(index.len(), 3);
        assert_eq!(index.unique_position_move_pairs(), 4);

        let dir = tempfile::tempdir().unwrap();
        let cpi_path = dir.path().join("test.cpi");

        let meta = index.write_to_file(&cpi_path, 2, 3).unwrap();
        assert_eq!(meta.total_positions, 3);
        assert_eq!(meta.total_pairs, 4);

        let disk_idx = DiskPositionIndex::open(&cpi_path).unwrap();
        assert_eq!(disk_idx.total_positions(), 3);
        assert_eq!(disk_idx.total_pairs(), 4);

        // Query key1
        let q1_imm = index.query(key1).unwrap();
        let q1_disk = disk_idx.query(key1).unwrap().unwrap();
        assert_eq!(q1_imm.position_key, q1_disk.position_key);
        assert_eq!(q1_imm.total_games, q1_disk.total_games);
        assert_eq!(q1_imm.white_wins, q1_disk.white_wins);
        assert_eq!(q1_imm.draws, q1_disk.draws);
        assert_eq!(q1_imm.black_wins, q1_disk.black_wins);
        assert_eq!(q1_imm.moves.len(), q1_disk.moves.len());
        assert_eq!(q1_imm.moves[0].uci, q1_disk.moves[0].uci);

        // Query non-existent
        let missing = PositionKey(0x99999999999999999999999999999999);
        assert!(disk_idx.query(missing).unwrap().is_none());
    }
}
