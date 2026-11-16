use byteorder::{BigEndian, ByteOrder, ReadBytesExt, WriteBytesExt};
use opening_explorer_core::{CompactMove, GameResult, ParsedGame, PositionKey};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use shakmaty::fen::Fen;
use shakmaty::{CastlingMode, Chess, Position};
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use thiserror::Error;

pub const CPI_MAGIC: &[u8; 8] = b"CODB_CPI";
pub const CPI_VERSION: u32 = 2;
pub const DEFAULT_POSITIONS_PER_BLOCK: usize = 256;
pub const CPI_HEADER_SIZE: usize = 64;
pub const CPI_SPARSE_ENTRY_SIZE: usize = 34;
pub const MAX_CPI_BLOCK_UNCOMPRESSED_SIZE: u32 = 16 * 1024 * 1024; // 16 MiB safety bound

const HEADER_SIZE: usize = CPI_HEADER_SIZE;

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
    #[error("Arithmetic overflow in index accounting: {0}")]
    Overflow(String),
}

/// Aggregated opening statistics for a candidate next move from a position.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveStats {
    pub compact_move: CompactMove,
    pub uci: String,
    pub white_wins: u32,
    pub draws: u32,
    pub black_wins: u32,
    pub total_games: u32,
}

/// Complete Opening Explorer query response for a position.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionStats {
    pub position_key: String,
    pub total_games: u32,
    pub white_wins: u32,
    pub draws: u32,
    pub black_wins: u32,
    pub moves: Vec<MoveStats>,
}

/// Detailed outcome of extracting opening positions from a single canonical game.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GamePositionExtraction {
    /// Extracted `(position_before_move, played_move, final_game_result)` tuples.
    /// Empty if `excluded_unknown_result` is true.
    pub entries: Vec<(PositionKey, CompactMove, GameResult)>,
    /// True if the game had an unknown result (`*`) and was excluded from W/D/L aggregation.
    pub excluded_unknown_result: bool,
    /// True if the game started from a legal non-default orthodox chess FEN.
    pub custom_starting_fen: bool,
    /// Number of intra-game position revisits skipped within the indexing depth window.
    pub repeated_positions_skipped: usize,
}

pub use opening_explorer_core::compute_position_key;

/// Extracts position-to-next-move records from a game up to `max_plies` with full accounting details.
///
/// ### Semantics:
/// 1. **Starting Position:** Reconstructs from the standard initial board when `fen_start` is `None`.
///    When `fen_start` is `Some(fen)`, validates and parses it as a legal orthodox chess position
///    (`CastlingMode::Standard`), failing closed with [`IndexError::Reconstruction`] if invalid or illegal.
/// 2. **Move Legality:** Every move within `0..min(moves.len(), max_plies)` is validated against the legal
///    board state, even if the game's result is `*`.
/// 3. **Intra-Game Repetition Policy:** A retained game contributes at most once to a given [`PositionKey`],
///    using the first occurrence within the indexing window (`max_plies`). Subsequent visits to the same
///    canonical position within the same game increment `repeated_positions_skipped` without emitting a
///    duplicate game contribution, while still advancing the board state for subsequent plies.
/// 4. **Unknown Result Policy (`*`):** Games with [`GameResult::Star`] are explicitly excluded from W/D/L
///    aggregation (`excluded_unknown_result = true`, `entries` empty) so that every indexed move and position
///    strictly satisfies `total_games = white_wins + draws + black_wins > 0`.
pub fn extract_game_positions_detailed(
    game: &ParsedGame,
    max_plies: usize,
) -> Result<GamePositionExtraction, IndexError> {
    let (mut board, custom_starting_fen) = if let Some(ref fen_str) = game.fen_start {
        let parsed_fen: Fen = fen_str.parse().map_err(|e| {
            IndexError::Reconstruction(format!(
                "Invalid starting FEN '{}' in game {}: {}",
                fen_str, game.metadata.id, e
            ))
        })?;
        let custom_pos: Chess = parsed_fen
            .into_position(CastlingMode::Standard)
            .map_err(|e| {
                IndexError::Reconstruction(format!(
                    "Illegal or unsupported orthodox starting FEN '{}' in game {}: {:?}",
                    fen_str, game.metadata.id, e
                ))
            })?;
        let is_custom = custom_pos != Chess::default();
        (custom_pos, is_custom)
    } else {
        (Chess::default(), false)
    };

    let limit = game.moves.len().min(max_plies);
    let is_unknown_result = game.metadata.result == GameResult::Star;
    let mut entries = if is_unknown_result {
        Vec::new()
    } else {
        Vec::with_capacity(limit)
    };

    // For typical opening depths (<= 64 plies), a small stack-like Vec is cache-local and faster than hashing;
    // for larger depths, fall back to a HashSet.
    let mut seen_vec: Vec<PositionKey> = Vec::with_capacity(limit.min(64));
    let mut seen_set: Option<HashSet<PositionKey>> = if limit > 64 {
        Some(HashSet::with_capacity(limit))
    } else {
        None
    };
    let mut repeated_positions_skipped = 0usize;

    for (i, &cm) in game.moves[..limit].iter().enumerate() {
        let key = compute_position_key(&board);

        let m = cm.to_shakmaty_move(&board).ok_or_else(|| {
            IndexError::Reconstruction(format!(
                "Illegal move at ply {} ({}) in game {}",
                i + 1,
                cm.to_uci(),
                game.metadata.id
            ))
        })?;

        board.play_unchecked(m);

        let is_first_visit = if let Some(ref mut set) = seen_set {
            set.insert(key)
        } else if seen_vec.contains(&key) {
            false
        } else {
            seen_vec.push(key);
            true
        };

        if is_first_visit {
            if !is_unknown_result {
                entries.push((key, cm, game.metadata.result));
            }
        } else {
            repeated_positions_skipped += 1;
        }
    }

    Ok(GamePositionExtraction {
        entries,
        excluded_unknown_result: is_unknown_result,
        custom_starting_fen,
        repeated_positions_skipped,
    })
}

/// Extracts all unique first-occurrence `(PositionKey, CompactMove, GameResult)` entries from a game up to `max_plies`.
pub fn extract_game_positions(
    game: &ParsedGame,
    max_plies: usize,
) -> Result<Vec<(PositionKey, CompactMove, GameResult)>, IndexError> {
    Ok(extract_game_positions_detailed(game, max_plies)?.entries)
}

/// SQLite Embedded Storage Engine (Reference Oracle).
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
                if *res == GameResult::Star {
                    continue;
                }
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
             ORDER BY (white_wins + draws + black_wins) DESC, move_u16 ASC;",
        )?;

        let mut rows = stmt.query(params![key.primary() as i64, key.secondary() as i64])?;
        let mut moves = Vec::new();
        let mut total_w = 0u32;
        let mut total_d = 0u32;
        let mut total_b = 0u32;

        while let Some(row) = rows.next()? {
            let m_val: i32 = row.get(0)?;
            let w: u32 = row.get(1)?;
            let d: u32 = row.get(2)?;
            let b: u32 = row.get(3)?;
            let total = w
                .checked_add(d)
                .and_then(|s| s.checked_add(b))
                .ok_or_else(|| {
                    IndexError::Overflow("Move total_games overflow in SQLite".into())
                })?;

            total_w = total_w
                .checked_add(w)
                .ok_or_else(|| IndexError::Overflow("Position white_wins overflow".into()))?;
            total_d = total_d
                .checked_add(d)
                .ok_or_else(|| IndexError::Overflow("Position draws overflow".into()))?;
            total_b = total_b
                .checked_add(b)
                .ok_or_else(|| IndexError::Overflow("Position black_wins overflow".into()))?;

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

        let pos_total = total_w
            .checked_add(total_d)
            .and_then(|s| s.checked_add(total_b))
            .ok_or_else(|| IndexError::Overflow("Position total_games overflow".into()))?;

        Ok(PositionStats {
            position_key: key.to_hex(),
            total_games: pos_total,
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

/// Custom Immutable Block-Compressed Sorted Position Index (`.cpi`) in-memory builder/oracle.
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
        if res == GameResult::Star {
            return;
        }
        let entry = self
            .data
            .entry(key)
            .or_default()
            .entry(cm)
            .or_insert((0, 0, 0));
        match res {
            GameResult::WhiteWins => entry.0 = entry.0.saturating_add(1),
            GameResult::Draw => entry.1 = entry.1.saturating_add(1),
            GameResult::BlackWins => entry.2 = entry.2.saturating_add(1),
            GameResult::Star => {}
        }
    }

    pub fn query(&self, key: PositionKey) -> Option<PositionStats> {
        let moves_map = self.data.get(&key)?;
        let mut moves = Vec::with_capacity(moves_map.len());
        let mut total_w = 0u32;
        let mut total_d = 0u32;
        let mut total_b = 0u32;

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

        // Sort descending by total games, with deterministic tie-breaker on compact_move
        moves.sort_by_key(|a| (std::cmp::Reverse(a.total_games), a.compact_move.0));

        Some(PositionStats {
            position_key: key.to_hex(),
            total_games: total_w + total_d + total_b,
            white_wins: total_w,
            draws: total_d,
            black_wins: total_b,
            moves,
        })
    }

    /// Serializes the entire index to a packed, block-compressed binary buffer.
    pub fn serialize_compressed(&self, zstd_level: i32) -> Result<Vec<u8>, IndexError> {
        let mut buf = Vec::new();
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

    /// Writes the CPI format to any seekable writer using [`StreamingCpiWriter`].
    pub fn write_cpi<W: Write + Seek>(
        &self,
        writer: &mut W,
        positions_per_block: usize,
        zstd_level: i32,
    ) -> Result<CpiMetadata, IndexError> {
        let mut streaming_writer =
            StreamingCpiWriter::new(writer, positions_per_block, zstd_level)?;
        let mut moves_buf = Vec::new();
        for (&key, moves_map) in &self.data {
            moves_buf.clear();
            for (&cm, &(w, d, b)) in moves_map {
                moves_buf.push((cm, w, d, b));
            }
            streaming_writer.write_position(key, &moves_buf)?;
        }
        streaming_writer.finish()
    }
}

/// Incremental, bounded-memory CPI v2 writer that encodes sorted position groups directly to disk
/// without requiring an in-memory `ImmutablePositionIndex`.
pub struct StreamingCpiWriter<W: Write + Seek> {
    writer: W,
    pos_per_block: usize,
    zstd_level: i32,
    sparse_writer: BufWriter<File>,
    block_count: u32,
    block_uncompressed: Vec<u8>,
    current_block_positions: u16,
    current_block_first_key: u128,
    prev_key: Option<PositionKey>,
    total_positions: u64,
    total_pairs: u64,
}

impl<W: Write + Seek> StreamingCpiWriter<W> {
    pub fn new(writer: W, positions_per_block: usize, zstd_level: i32) -> Result<Self, IndexError> {
        Self::new_with_temp_dir(writer, positions_per_block, zstd_level, None)
    }

    pub fn new_with_temp_dir(
        mut writer: W,
        positions_per_block: usize,
        zstd_level: i32,
        temp_dir: Option<&Path>,
    ) -> Result<Self, IndexError> {
        let pos_per_block = if positions_per_block == 0 {
            DEFAULT_POSITIONS_PER_BLOCK
        } else {
            positions_per_block
        };
        if pos_per_block > u16::MAX as usize {
            return Err(IndexError::InvalidFormat(format!(
                "positions_per_block ({}) exceeds u16::MAX ({})",
                pos_per_block,
                u16::MAX
            )));
        }

        writer.seek(SeekFrom::Start(0))?;
        let header_placeholder = [0u8; HEADER_SIZE];
        writer.write_all(&header_placeholder)?;

        let sparse_file = match temp_dir {
            Some(dir) => tempfile::tempfile_in(dir)?,
            None => tempfile::tempfile()?,
        };
        let sparse_writer = BufWriter::with_capacity(64 * 1024, sparse_file);

        Ok(StreamingCpiWriter {
            writer,
            pos_per_block,
            zstd_level,
            sparse_writer,
            block_count: 0,
            block_uncompressed: Vec::with_capacity(pos_per_block * 36),
            current_block_positions: 0,
            current_block_first_key: 0,
            prev_key: None,
            total_positions: 0,
            total_pairs: 0,
        })
    }

    /// Appends a single aggregated position and its candidate moves `(CompactMove, white_wins, draws, black_wins)`.
    ///
    /// Enforces:
    /// 1. Strictly increasing [`PositionKey`] order across calls.
    /// 2. Non-empty moves (`1..=255`) in strictly increasing [`CompactMove`] order.
    /// 3. Checked `u32` W/D/L arithmetic with `total_games = w + d + b > 0` for every move and position.
    pub fn write_position(
        &mut self,
        key: PositionKey,
        moves: &[(CompactMove, u32, u32, u32)],
    ) -> Result<(), IndexError> {
        if let Some(prev) = self.prev_key {
            if key.0 <= prev.0 {
                return Err(IndexError::InvalidFormat(format!(
                    "StreamingCpiWriter requires strictly increasing PositionKey order: {:032x} <= {:032x}",
                    key.0, prev.0
                )));
            }
        }
        if moves.is_empty() {
            return Err(IndexError::InvalidFormat(format!(
                "Position {:032x} has 0 candidate moves",
                key.0
            )));
        }
        if moves.len() > u8::MAX as usize {
            return Err(IndexError::InvalidFormat(format!(
                "Position {:032x} has {} moves, exceeding u8::MAX ({})",
                key.0,
                moves.len(),
                u8::MAX
            )));
        }

        let mut prev_move: Option<u16> = None;
        let mut pos_total_games = 0u32;
        for &(cm, w, d, b) in moves {
            if let Some(pm) = prev_move {
                if cm.0 <= pm {
                    return Err(IndexError::InvalidFormat(format!(
                        "Candidate moves for position {:032x} must be strictly sorted by CompactMove: {} <= {}",
                        key.0, cm.0, pm
                    )));
                }
            }
            prev_move = Some(cm.0);

            let move_total = w
                .checked_add(d)
                .and_then(|s| s.checked_add(b))
                .ok_or_else(|| {
                    IndexError::Overflow(format!(
                        "Move W/D/L sum overflow for position {:032x} move {}",
                        key.0,
                        cm.to_uci()
                    ))
                })?;
            if move_total == 0 {
                return Err(IndexError::InvalidFormat(format!(
                    "Zero-game move {} in position {:032x} (W=0, D=0, B=0)",
                    cm.to_uci(),
                    key.0
                )));
            }
            pos_total_games = pos_total_games.checked_add(move_total).ok_or_else(|| {
                IndexError::Overflow(format!("Position total_games overflow for {:032x}", key.0))
            })?;
        }

        if self.current_block_positions == 0 {
            self.block_uncompressed.clear();
            // Reserve 2 bytes for num_positions u16 at start of block buffer
            self.block_uncompressed.write_u16::<BigEndian>(0)?;
            self.current_block_first_key = key.0;
        }

        self.block_uncompressed.write_u128::<BigEndian>(key.0)?;
        self.block_uncompressed.write_u8(moves.len() as u8)?;
        for &(cm, w, d, b) in moves {
            self.block_uncompressed.write_u16::<BigEndian>(cm.0)?;
            self.block_uncompressed.write_u32::<BigEndian>(w)?;
            self.block_uncompressed.write_u32::<BigEndian>(d)?;
            self.block_uncompressed.write_u32::<BigEndian>(b)?;
        }

        self.current_block_positions += 1;
        self.prev_key = Some(key);
        self.total_positions = self
            .total_positions
            .checked_add(1)
            .ok_or_else(|| IndexError::Overflow("total_positions overflow".into()))?;
        self.total_pairs = self
            .total_pairs
            .checked_add(moves.len() as u64)
            .ok_or_else(|| IndexError::Overflow("total_pairs overflow".into()))?;

        if self.current_block_positions as usize >= self.pos_per_block {
            self.flush_current_block()?;
        }

        Ok(())
    }

    fn flush_current_block(&mut self) -> Result<(), IndexError> {
        if self.current_block_positions == 0 {
            return Ok(());
        }

        // Write actual num_positions into bytes [0..2] of block_uncompressed
        let count_bytes = self.current_block_positions.to_be_bytes();
        self.block_uncompressed[0] = count_bytes[0];
        self.block_uncompressed[1] = count_bytes[1];

        let uncompressed_len = u32::try_from(self.block_uncompressed.len()).map_err(|_| {
            IndexError::Overflow("Uncompressed CPI block exceeds u32::MAX bytes".into())
        })?;
        if uncompressed_len > MAX_CPI_BLOCK_UNCOMPRESSED_SIZE {
            return Err(IndexError::InvalidFormat(format!(
                "Uncompressed CPI block size ({}) exceeds safety limit ({})",
                uncompressed_len, MAX_CPI_BLOCK_UNCOMPRESSED_SIZE
            )));
        }

        let compressed = zstd::encode_all(Cursor::new(&self.block_uncompressed), self.zstd_level)
            .map_err(|e| IndexError::Zstd(e.to_string()))?;
        let compressed_len = u32::try_from(compressed.len()).map_err(|_| {
            IndexError::Overflow("Compressed CPI block exceeds u32::MAX bytes".into())
        })?;

        let file_offset = self.writer.stream_position()?;
        self.writer.write_all(&compressed)?;

        self.sparse_writer
            .write_u128::<BigEndian>(self.current_block_first_key)?;
        self.sparse_writer.write_u64::<BigEndian>(file_offset)?;
        self.sparse_writer.write_u32::<BigEndian>(compressed_len)?;
        self.sparse_writer
            .write_u32::<BigEndian>(uncompressed_len)?;
        self.sparse_writer
            .write_u16::<BigEndian>(self.current_block_positions)?;
        self.block_count = self.block_count.checked_add(1).ok_or_else(|| {
            IndexError::Overflow("CPI sparse block count exceeds u32::MAX".into())
        })?;

        self.block_uncompressed.clear();
        self.current_block_positions = 0;
        Ok(())
    }

    /// Flushes any remaining buffered positions, writes the sparse block directory and header,
    /// and returns the finalized [`CpiMetadata`].
    pub fn finish(mut self) -> Result<CpiMetadata, IndexError> {
        if self.current_block_positions > 0 {
            self.flush_current_block()?;
        }

        self.sparse_writer.flush()?;
        let mut sparse_file = self
            .sparse_writer
            .into_inner()
            .map_err(|e| IndexError::Io(e.into_error()))?;
        sparse_file.seek(SeekFrom::Start(0))?;

        let sparse_index_offset = self.writer.stream_position()?;
        let mut copy_buf = [0u8; 64 * 1024];
        loop {
            let n = sparse_file.read(&mut copy_buf)?;
            if n == 0 {
                break;
            }
            self.writer.write_all(&copy_buf[..n])?;
        }

        let total_file_size = self.writer.stream_position()?;
        let sparse_index_len = total_file_size - sparse_index_offset;

        // Seek to 0 and write 64-byte header
        self.writer.seek(SeekFrom::Start(0))?;
        self.writer.write_all(CPI_MAGIC)?;
        self.writer.write_u32::<BigEndian>(CPI_VERSION)?;
        self.writer
            .write_u32::<BigEndian>(self.pos_per_block as u32)?;
        self.writer.write_u64::<BigEndian>(self.total_positions)?;
        self.writer.write_u64::<BigEndian>(self.total_pairs)?;
        self.writer.write_u32::<BigEndian>(self.block_count)?;
        self.writer.write_u32::<BigEndian>(0)?; // reserved 4 bytes
        self.writer.write_u64::<BigEndian>(sparse_index_offset)?;
        self.writer.write_u64::<BigEndian>(sparse_index_len)?;
        self.writer.write_u64::<BigEndian>(0)?; // flags 8 bytes

        // Seek back to end and flush
        self.writer.seek(SeekFrom::Start(total_file_size))?;
        self.writer.flush()?;

        Ok(CpiMetadata {
            total_positions: self.total_positions,
            total_pairs: self.total_pairs,
            block_count: self.block_count,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpiMetadata {
    pub total_positions: u64,
    pub total_pairs: u64,
    pub block_count: u32,
    pub total_file_size: u64,
}

/// Summary returned by [`DiskPositionIndex::verify_integrity`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpiVerificationSummary {
    pub total_positions: u64,
    pub total_pairs: u64,
    pub blocks_verified: u32,
    pub total_file_size: u64,
}

/// Memory-mapped, hardened CPI v2 reader for random-access position queries.
pub struct DiskPositionIndex {
    mmap: memmap2::Mmap,
    pos_per_block: u32,
    sparse_index_offset: u64,
    block_count: u32,
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
            return Err(IndexError::InvalidFormat(format!(
                "File too short for CPI header ({} < {} bytes)",
                mmap.len(),
                HEADER_SIZE
            )));
        }
        if &mmap[0..8] != CPI_MAGIC {
            return Err(IndexError::InvalidFormat("Invalid CPI magic header".into()));
        }
        let mut cursor = Cursor::new(&mmap[8..HEADER_SIZE]);
        let version = cursor.read_u32::<BigEndian>()?;
        if version != CPI_VERSION {
            return Err(IndexError::InvalidFormat(format!(
                "Unsupported CPI version: {} (expected {})",
                version, CPI_VERSION
            )));
        }
        let pos_per_block = cursor.read_u32::<BigEndian>()?;
        if pos_per_block == 0 || pos_per_block > u16::MAX as u32 {
            return Err(IndexError::InvalidFormat(format!(
                "Invalid positions_per_block in CPI header: {}",
                pos_per_block
            )));
        }
        let total_positions = cursor.read_u64::<BigEndian>()?;
        let total_pairs = cursor.read_u64::<BigEndian>()?;
        let block_count = cursor.read_u32::<BigEndian>()?;
        let reserved = cursor.read_u32::<BigEndian>()?;
        let sparse_index_offset = cursor.read_u64::<BigEndian>()?;
        let sparse_index_len = cursor.read_u64::<BigEndian>()?;
        let flags = cursor.read_u64::<BigEndian>()?;

        if reserved != 0 || flags != 0 {
            return Err(IndexError::InvalidFormat(format!(
                "Unsupported non-zero reserved ({}) or flags ({}) fields in CPI header",
                reserved, flags
            )));
        }

        let file_len = mmap.len() as u64;
        if sparse_index_offset < HEADER_SIZE as u64 {
            return Err(IndexError::InvalidFormat(format!(
                "Invalid sparse_index_offset ({}) < header size ({})",
                sparse_index_offset, HEADER_SIZE
            )));
        }

        let expected_sparse_len = (block_count as u64)
            .checked_mul(CPI_SPARSE_ENTRY_SIZE as u64)
            .ok_or_else(|| IndexError::InvalidFormat("Sparse index length overflow".into()))?;
        if sparse_index_len != expected_sparse_len {
            return Err(IndexError::InvalidFormat(format!(
                "Sparse index length mismatch: header declares {} bytes, expected {} for {} blocks",
                sparse_index_len, expected_sparse_len, block_count
            )));
        }

        let sparse_end = sparse_index_offset
            .checked_add(sparse_index_len)
            .ok_or_else(|| IndexError::InvalidFormat("Sparse index end offset overflow".into()))?;
        if sparse_end != file_len {
            return Err(IndexError::InvalidFormat(format!(
                "Sparse index bounds ({}..{}) do not match file length ({})",
                sparse_index_offset, sparse_end, file_len
            )));
        }

        if total_positions == 0 {
            if block_count != 0 || total_pairs != 0 || sparse_index_offset != HEADER_SIZE as u64 {
                return Err(IndexError::InvalidFormat(format!(
                    "Inconsistent empty CPI header: total_positions=0 but block_count={}, total_pairs={}, offset={}",
                    block_count, total_pairs, sparse_index_offset
                )));
            }
        } else {
            if block_count == 0 {
                return Err(IndexError::InvalidFormat(
                    "Inconsistent CPI header: total_positions > 0 but block_count == 0".into(),
                ));
            }
            if total_pairs < total_positions {
                return Err(IndexError::InvalidFormat(format!(
                    "Inconsistent CPI header: total_pairs ({}) < total_positions ({})",
                    total_pairs, total_positions
                )));
            }
        }

        let mut idx_cursor = Cursor::new(
            &mmap[sparse_index_offset as usize..(sparse_index_offset + sparse_index_len) as usize],
        );
        let mut prev_first_key: Option<u128> = None;
        let mut expected_block_offset = HEADER_SIZE as u64;
        let mut sum_block_positions = 0u64;

        for block_idx in 0..block_count {
            let first_key = idx_cursor.read_u128::<BigEndian>()?;
            let file_offset = idx_cursor.read_u64::<BigEndian>()?;
            let compressed_len = idx_cursor.read_u32::<BigEndian>()?;
            let uncompressed_len = idx_cursor.read_u32::<BigEndian>()?;
            let num_positions = idx_cursor.read_u16::<BigEndian>()?;

            if let Some(prev) = prev_first_key {
                if first_key <= prev {
                    return Err(IndexError::InvalidFormat(format!(
                        "Sparse index keys not strictly sorted at block {}: {:032x} <= {:032x}",
                        block_idx, first_key, prev
                    )));
                }
            }
            prev_first_key = Some(first_key);

            if num_positions == 0 || (num_positions as u32) > pos_per_block {
                return Err(IndexError::InvalidFormat(format!(
                    "Invalid num_positions ({}) in sparse index at block {} (max {})",
                    num_positions, block_idx, pos_per_block
                )));
            }
            if compressed_len == 0
                || uncompressed_len == 0
                || uncompressed_len > MAX_CPI_BLOCK_UNCOMPRESSED_SIZE
            {
                return Err(IndexError::InvalidFormat(format!(
                    "Invalid block lengths (compressed={}, uncompressed={}) at block {}",
                    compressed_len, uncompressed_len, block_idx
                )));
            }
            if file_offset != expected_block_offset {
                return Err(IndexError::InvalidFormat(format!(
                    "Non-contiguous block offset at block {}: expected {}, got {}",
                    block_idx, expected_block_offset, file_offset
                )));
            }
            expected_block_offset = file_offset
                .checked_add(compressed_len as u64)
                .ok_or_else(|| IndexError::InvalidFormat("Block end offset overflow".into()))?;
            if expected_block_offset > sparse_index_offset {
                return Err(IndexError::InvalidFormat(format!(
                    "Block {} bounds ({}..{}) overlap sparse index at {}",
                    block_idx, file_offset, expected_block_offset, sparse_index_offset
                )));
            }

            sum_block_positions = sum_block_positions
                .checked_add(num_positions as u64)
                .ok_or_else(|| IndexError::InvalidFormat("Block position sum overflow".into()))?;
        }

        if expected_block_offset != sparse_index_offset {
            return Err(IndexError::InvalidFormat(format!(
                "Unindexed gap between last block end ({}) and sparse_index_offset ({})",
                expected_block_offset, sparse_index_offset
            )));
        }
        if sum_block_positions != total_positions {
            return Err(IndexError::InvalidFormat(format!(
                "Sum of block position counts ({}) != header total_positions ({})",
                sum_block_positions, total_positions
            )));
        }

        Ok(DiskPositionIndex {
            mmap,
            pos_per_block,
            sparse_index_offset,
            block_count,
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
        self.block_count as usize
    }

    pub fn positions_per_block(&self) -> u32 {
        self.pos_per_block
    }

    pub fn file_size_bytes(&self) -> u64 {
        self.mmap.len() as u64
    }

    pub fn get_block_entry(&self, block_idx: usize) -> Result<BlockIndexEntry, IndexError> {
        if block_idx >= self.block_count as usize {
            return Err(IndexError::InvalidFormat(format!(
                "Block index {} out of bounds (block_count={})",
                block_idx, self.block_count
            )));
        }
        let offset = self.sparse_index_offset as usize + block_idx * CPI_SPARSE_ENTRY_SIZE;
        let slice = &self.mmap[offset..offset + CPI_SPARSE_ENTRY_SIZE];
        let first_key = BigEndian::read_u128(&slice[0..16]);
        let file_offset = BigEndian::read_u64(&slice[16..24]);
        let compressed_len = BigEndian::read_u32(&slice[24..28]);
        let uncompressed_len = BigEndian::read_u32(&slice[28..32]);
        let num_positions = BigEndian::read_u16(&slice[32..34]);
        Ok(BlockIndexEntry {
            first_key,
            file_offset,
            compressed_len,
            uncompressed_len,
            num_positions,
        })
    }

    fn first_key_at(&self, block_idx: usize) -> u128 {
        let offset = self.sparse_index_offset as usize + block_idx * CPI_SPARSE_ENTRY_SIZE;
        BigEndian::read_u128(&self.mmap[offset..offset + 16])
    }

    fn decompress_block_bytes(
        &self,
        block_idx: usize,
    ) -> Result<(BlockIndexEntry, Vec<u8>), IndexError> {
        let block_entry = self.get_block_entry(block_idx)?;
        let start = usize::try_from(block_entry.file_offset)
            .map_err(|_| IndexError::InvalidFormat("Block offset exceeds usize".into()))?;
        let len = block_entry.compressed_len as usize;
        let end = start
            .checked_add(len)
            .ok_or_else(|| IndexError::InvalidFormat("Block slice bounds overflow".into()))?;
        if end > self.mmap.len() {
            return Err(IndexError::InvalidFormat(
                "Block bounds exceed file size".into(),
            ));
        }

        let compressed_slice = &self.mmap[start..end];
        let expected_uncompressed = block_entry.uncompressed_len as usize;
        let decompressed = zstd::bulk::decompress(compressed_slice, expected_uncompressed)
            .map_err(|e| {
                IndexError::InvalidFormat(format!(
                    "Failed to decompress CPI block {}: {}",
                    block_idx, e
                ))
            })?;

        if decompressed.len() != expected_uncompressed {
            return Err(IndexError::InvalidFormat(format!(
                "CPI block {} uncompressed size mismatch: expected {}, got {}",
                block_idx,
                expected_uncompressed,
                decompressed.len()
            )));
        }

        Ok((block_entry, decompressed))
    }

    /// Validates a single block's full internal structure and optionally extracts `target_key`.
    /// Returns `(block_pairs_count, last_key_in_block, Option<PositionStats>)`.
    fn scan_and_validate_block(
        &self,
        block_idx: usize,
        target_key: Option<PositionKey>,
    ) -> Result<(u64, u128, Option<PositionStats>), IndexError> {
        let (block_entry, decompressed) = self.decompress_block_bytes(block_idx)?;
        let next_block_first_key = if block_idx + 1 < self.block_count as usize {
            Some(self.first_key_at(block_idx + 1))
        } else {
            None
        };

        if decompressed.len() < 2 {
            return Err(IndexError::InvalidFormat(format!(
                "CPI block {} too short for position count",
                block_idx
            )));
        }

        let mut cursor = Cursor::new(&decompressed);
        let num_positions = cursor.read_u16::<BigEndian>()?;
        if num_positions != block_entry.num_positions {
            return Err(IndexError::InvalidFormat(format!(
                "CPI block {} num_positions ({}) != sparse directory entry ({})",
                block_idx, num_positions, block_entry.num_positions
            )));
        }

        let mut prev_key: Option<u128> = None;
        let mut block_pairs = 0u64;
        let mut found_stats: Option<PositionStats> = None;

        for pos_idx in 0..num_positions {
            let rem = decompressed.len() as u64 - cursor.position();
            if rem < 17 {
                return Err(IndexError::InvalidFormat(format!(
                    "Truncated position header at block {} position {}",
                    block_idx, pos_idx
                )));
            }

            let k = cursor.read_u128::<BigEndian>()?;
            let num_moves = cursor.read_u8()? as usize;

            if pos_idx == 0 && k != block_entry.first_key {
                return Err(IndexError::InvalidFormat(format!(
                    "CPI block {} first key {:032x} != sparse directory first_key {:032x}",
                    block_idx, k, block_entry.first_key
                )));
            }
            if let Some(prev) = prev_key {
                if k <= prev {
                    return Err(IndexError::InvalidFormat(format!(
                        "Unsorted or duplicate key in CPI block {} at index {}: {:032x} <= {:032x}",
                        block_idx, pos_idx, k, prev
                    )));
                }
            }
            if let Some(upper) = next_block_first_key {
                if k >= upper {
                    return Err(IndexError::InvalidFormat(format!(
                        "Key {:032x} in CPI block {} >= next block first_key {:032x}",
                        k, block_idx, upper
                    )));
                }
            }
            prev_key = Some(k);

            if num_moves == 0 {
                return Err(IndexError::InvalidFormat(format!(
                    "Position {:032x} in block {} has 0 moves",
                    k, block_idx
                )));
            }

            let moves_bytes = (num_moves as u64) * 14;
            let rem_after_header = decompressed.len() as u64 - cursor.position();
            if rem_after_header < moves_bytes {
                return Err(IndexError::InvalidFormat(format!(
                    "Truncated move records for position {:032x} in block {} (need {} bytes, have {})",
                    k, block_idx, moves_bytes, rem_after_header
                )));
            }

            block_pairs += num_moves as u64;

            let is_target = target_key.map(|tk| tk.0 == k).unwrap_or(false);
            let mut moves = if is_target {
                Vec::with_capacity(num_moves)
            } else {
                Vec::new()
            };

            let mut total_w = 0u32;
            let mut total_d = 0u32;
            let mut total_b = 0u32;
            let mut prev_move_u16: Option<u16> = None;

            for _ in 0..num_moves {
                let m_val = cursor.read_u16::<BigEndian>()?;
                let w = cursor.read_u32::<BigEndian>()?;
                let d = cursor.read_u32::<BigEndian>()?;
                let b = cursor.read_u32::<BigEndian>()?;

                if let Some(pm) = prev_move_u16 {
                    if m_val <= pm {
                        return Err(IndexError::InvalidFormat(format!(
                            "Unsorted or duplicate move {} <= {} in position {:032x} block {}",
                            m_val, pm, k, block_idx
                        )));
                    }
                }
                prev_move_u16 = Some(m_val);

                let total = w
                    .checked_add(d)
                    .and_then(|s| s.checked_add(b))
                    .ok_or_else(|| {
                        IndexError::InvalidFormat(format!(
                            "Move W/D/L overflow in position {:032x} block {}",
                            k, block_idx
                        ))
                    })?;
                if total == 0 {
                    return Err(IndexError::InvalidFormat(format!(
                        "Zero-game move record in position {:032x} block {}",
                        k, block_idx
                    )));
                }

                total_w = total_w.checked_add(w).ok_or_else(|| {
                    IndexError::InvalidFormat("Position white_wins overflow".into())
                })?;
                total_d = total_d
                    .checked_add(d)
                    .ok_or_else(|| IndexError::InvalidFormat("Position draws overflow".into()))?;
                total_b = total_b.checked_add(b).ok_or_else(|| {
                    IndexError::InvalidFormat("Position black_wins overflow".into())
                })?;

                if is_target {
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
            }

            let pos_total = total_w
                .checked_add(total_d)
                .and_then(|s| s.checked_add(total_b))
                .ok_or_else(|| {
                    IndexError::InvalidFormat(format!(
                        "Position total_games overflow in {:032x}",
                        k
                    ))
                })?;

            if is_target {
                moves.sort_by_key(|a| (std::cmp::Reverse(a.total_games), a.compact_move.0));
                found_stats = Some(PositionStats {
                    position_key: PositionKey(k).to_hex(),
                    total_games: pos_total,
                    white_wins: total_w,
                    draws: total_d,
                    black_wins: total_b,
                    moves,
                });
            }
        }

        if cursor.position() != decompressed.len() as u64 {
            return Err(IndexError::InvalidFormat(format!(
                "Trailing bytes in CPI block {}: consumed {} of {} bytes",
                block_idx,
                cursor.position(),
                decompressed.len()
            )));
        }

        Ok((block_pairs, prev_key.unwrap_or(0), found_stats))
    }

    pub fn query(&self, key: PositionKey) -> Result<Option<PositionStats>, IndexError> {
        if self.block_count == 0 || key.0 < self.first_key_at(0) {
            return Ok(None);
        }

        let mut left = 0;
        let mut right = self.block_count as usize;
        while left < right {
            let mid = left + (right - left) / 2;
            if self.first_key_at(mid) <= key.0 {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        if left == 0 {
            return Ok(None);
        }
        let block_idx = left - 1;
        let (_, _, found) = self.scan_and_validate_block(block_idx, Some(key))?;
        Ok(found)
    }

    /// Decompresses and verifies every block in the CPI file, validating all structural,
    /// key-ordering, move-ordering, and W/D/L accounting invariants.
    pub fn verify_integrity(&self) -> Result<CpiVerificationSummary, IndexError> {
        let mut total_pairs_verified = 0u64;
        let mut prev_block_last_key: Option<u128> = None;

        for block_idx in 0..self.block_count as usize {
            if let Some(prev_last) = prev_block_last_key {
                let first_key = self.first_key_at(block_idx);
                if first_key <= prev_last {
                    return Err(IndexError::InvalidFormat(format!(
                        "Cross-block key order violation between block {} ({:032x}) and block {} ({:032x})",
                        block_idx - 1,
                        prev_last,
                        block_idx,
                        first_key
                    )));
                }
            }
            let (block_pairs, last_key, _) = self.scan_and_validate_block(block_idx, None)?;
            total_pairs_verified = total_pairs_verified
                .checked_add(block_pairs)
                .ok_or_else(|| IndexError::InvalidFormat("Total pairs sum overflow".into()))?;
            prev_block_last_key = Some(last_key);
        }

        if total_pairs_verified != self.total_pairs {
            return Err(IndexError::InvalidFormat(format!(
                "Verified total_pairs across blocks ({}) != header total_pairs ({})",
                total_pairs_verified, self.total_pairs
            )));
        }

        Ok(CpiVerificationSummary {
            total_positions: self.total_positions,
            total_pairs: self.total_pairs,
            blocks_verified: self.block_count,
            total_file_size: self.mmap.len() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opening_explorer_core::{Collection, GameMetadata};
    use shakmaty::{Move, Role, Square};

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

        // Path 2: 1. c4 e6 2. d4 Nf6
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
    fn test_intra_game_repeated_positions_counted_once() {
        // 1. Nf3 Nf6 2. Ng1 Ng8 3. e4 e5
        // Plies 0..4 return to initial board at ply 4!
        let game = ParsedGame {
            metadata: GameMetadata {
                id: 42,
                collection: Collection::MastersOtb,
                white: "A".into(),
                black: "B".into(),
                white_elo: Some(2600),
                black_elo: Some(2600),
                date: Some("2024.01.01".into()),
                event: None,
                site: None,
                round: None,
                result: GameResult::WhiteWins,
                eco: None,
                time_control: None,
                source_id: None,
                broadcast_url: None,
                source_attribution: None,
            },
            moves: vec![
                CompactMove::new(6, 21, None),  // 1. g1f3
                CompactMove::new(62, 45, None), // 1... g8f6
                CompactMove::new(21, 6, None),  // 2. f3g1
                CompactMove::new(45, 62, None), // 2... f6g8 (returns to initial position)
                CompactMove::new(12, 28, None), // 3. e2e4 (second visit to initial position)
                CompactMove::new(52, 36, None), // 3... e7e5 (position after 1.e4)
            ],
            fen_start: None,
        };

        let detailed = extract_game_positions_detailed(&game, 40).unwrap();
        assert_eq!(detailed.repeated_positions_skipped, 1);
        assert_eq!(detailed.entries.len(), 5); // 6 plies minus 1 repeated initial position

        let mut imm = ImmutablePositionIndex::new();
        for &(k, cm, r) in &detailed.entries {
            imm.record(k, cm, r);
        }

        let start_key = compute_position_key(&Chess::default());
        let stats = imm.query(start_key).unwrap();
        // The single game must contribute at most once to the starting position
        assert_eq!(stats.total_games, 1);
        assert_eq!(stats.white_wins, 1);
        assert_eq!(stats.moves.len(), 1);
        assert_eq!(stats.moves[0].uci, "g1f3");
    }

    #[test]
    fn test_unknown_result_games_explicitly_excluded() {
        let star_game = ParsedGame {
            metadata: GameMetadata {
                id: 99,
                collection: Collection::MastersOtb,
                white: "A".into(),
                black: "B".into(),
                white_elo: Some(2600),
                black_elo: Some(2600),
                date: Some("2024.01.01".into()),
                event: None,
                site: None,
                round: None,
                result: GameResult::Star,
                eco: None,
                time_control: None,
                source_id: None,
                broadcast_url: None,
                source_attribution: None,
            },
            moves: vec![
                CompactMove::new(12, 28, None), // e2e4
                CompactMove::new(52, 36, None), // e7e5
            ],
            fen_start: None,
        };

        let detailed = extract_game_positions_detailed(&star_game, 40).unwrap();
        assert!(detailed.excluded_unknown_result);
        assert!(detailed.entries.is_empty());
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
                broadcast_url: None,
                source_attribution: None,
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
        let summary = disk_idx.verify_integrity().unwrap();
        assert_eq!(summary.total_positions, 3);
        assert_eq!(summary.total_pairs, 4);
        assert_eq!(summary.blocks_verified, 2);

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

    #[test]
    fn test_cpi_format_constants_and_layout() {
        assert_eq!(CPI_HEADER_SIZE, 64, "CPI header must be exactly 64 bytes");
        assert_eq!(
            CPI_SPARSE_ENTRY_SIZE, 34,
            "CPI sparse index entry must be exactly 34 bytes (16B first key + 8B offset + 8B length + 2B pairs)"
        );
    }
}
