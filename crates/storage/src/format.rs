use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use chess_database_core::{Collection, CompactMove, GameMetadata, GameResult, ParsedGame};
use shakmaty::san::SanPlus;
use shakmaty::Chess;
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use thiserror::Error;

pub const BLOCK_MAGIC_V2: &[u8; 4] = b"CGB2";
pub const BLOCK_MAGIC_V3: &[u8; 4] = b"CGB3";

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Zstd compression error: {0}")]
    Zstd(String),
    #[error("Encoding error: {0}")]
    Encoding(String),
    #[error("Decoding error: {0}")]
    Decoding(String),
    #[error("Invalid block magic or version")]
    InvalidMagic,
    #[error("Game ID not found: {0}")]
    GameNotFound(u64),
    #[error("Integrity error: {0}")]
    Integrity(String),
    #[error("Database error in storage catalog: {0}")]
    Database(String),
    #[error("Dataset lock conflict: {0}")]
    Lock(String),
}

impl From<rusqlite::Error> for StorageError {
    fn from(err: rusqlite::Error) -> Self {
        StorageError::Database(err.to_string())
    }
}

impl From<chess_database_index::position::IndexError> for StorageError {
    fn from(err: chess_database_index::position::IndexError) -> Self {
        match err {
            chess_database_index::position::IndexError::Io(io_err) => StorageError::Io(io_err),
            other => StorageError::Integrity(other.to_string()),
        }
    }
}

/// An entry in the random-access block index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockIndexEntry {
    pub start_game_id: u64,
    pub game_count: u32,
    pub file_offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
}

/// In-memory representation of a single compressed game block container.
#[derive(Debug, Clone)]
pub struct GameBlock {
    pub collection: Collection,
    pub games: Vec<ParsedGame>,
}

impl GameBlock {
    pub fn new(collection: Collection) -> Self {
        GameBlock {
            collection,
            games: Vec::new(),
        }
    }

    /// Serializes and compresses this block of games into a binary payload using dictionary encoding.
    /// Preserves all supported game metadata and provenance fields without fixed-width truncation:
    /// players, ratings, date, event, site, round, result, ECO, time control, source ID (`GameURL`),
    /// broadcast URL (`BroadcastURL`), starting FEN, and legal moves.
    pub fn compress(&self, zstd_level: i32) -> Result<Vec<u8>, StorageError> {
        let (compressed, _uncompressed_size) = self.compress_with_stats(zstd_level)?;
        Ok(compressed)
    }

    /// Serializes and compresses this block, returning both the compressed bytes and the exact
    /// uncompressed byte count of the serialized payload.
    pub fn compress_with_stats(&self, zstd_level: i32) -> Result<(Vec<u8>, usize), StorageError> {
        let mut raw_buf = Vec::with_capacity(self.games.len() * 140);

        // Magic and format version header (CGB3: dictionary-interned ECO, Date, and BroadcastURL)
        raw_buf.write_all(BLOCK_MAGIC_V3)?;

        // String dictionaries within this block
        let mut players: Vec<String> = Vec::new();
        let mut player_map: HashMap<String, u16> = HashMap::new();

        let mut events: Vec<String> = Vec::new();
        let mut event_map: HashMap<String, u16> = HashMap::new();

        let mut sites: Vec<String> = Vec::new();
        let mut site_map: HashMap<String, u16> = HashMap::new();

        let mut rounds: Vec<String> = Vec::new();
        let mut round_map: HashMap<String, u16> = HashMap::new();

        let mut dates: Vec<String> = Vec::new();
        let mut date_map: HashMap<String, u16> = HashMap::new();

        let mut ecos: Vec<String> = Vec::new();
        let mut eco_map: HashMap<String, u16> = HashMap::new();

        let mut time_controls: Vec<String> = Vec::new();
        let mut tc_map: HashMap<String, u16> = HashMap::new();

        let mut source_ids: Vec<String> = Vec::new();
        let mut source_map: HashMap<String, u16> = HashMap::new();

        let mut broadcast_urls: Vec<String> = Vec::new();
        let mut broadcast_map: HashMap<String, u16> = HashMap::new();

        let mut fens: Vec<String> = Vec::new();
        let mut fen_map: HashMap<String, u16> = HashMap::new();

        let intern_str = |s: &str,
                          list: &mut Vec<String>,
                          map: &mut HashMap<String, u16>|
         -> Result<u16, StorageError> {
            if let Some(&idx) = map.get(s) {
                return Ok(idx);
            }
            if list.len() >= (u16::MAX - 1) as usize {
                return Err(StorageError::Encoding(
                    "Dictionary capacity exceeded (max 65534 unique strings per block)".into(),
                ));
            }
            let idx = list.len() as u16;
            map.insert(s.to_string(), idx);
            list.push(s.to_string());
            Ok(idx)
        };

        let intern_opt = |val: &Option<String>,
                          list: &mut Vec<String>,
                          map: &mut HashMap<String, u16>|
         -> Result<(), StorageError> {
            if let Some(s) = val {
                intern_str(s, list, map)?;
            }
            Ok(())
        };

        for g in &self.games {
            intern_str(&g.metadata.white, &mut players, &mut player_map)?;
            intern_str(&g.metadata.black, &mut players, &mut player_map)?;
            intern_opt(&g.metadata.event, &mut events, &mut event_map)?;
            intern_opt(&g.metadata.site, &mut sites, &mut site_map)?;
            intern_opt(&g.metadata.round, &mut rounds, &mut round_map)?;
            intern_opt(&g.metadata.date, &mut dates, &mut date_map)?;
            intern_opt(&g.metadata.eco, &mut ecos, &mut eco_map)?;
            intern_opt(&g.metadata.time_control, &mut time_controls, &mut tc_map)?;
            intern_opt(&g.metadata.source_id, &mut source_ids, &mut source_map)?;
            intern_opt(
                &g.metadata.broadcast_url,
                &mut broadcast_urls,
                &mut broadcast_map,
            )?;
            intern_opt(&g.fen_start, &mut fens, &mut fen_map)?;
        }

        // Header: game count (u32)
        raw_buf.write_u32::<BigEndian>(self.games.len() as u32)?;

        // Write dictionaries helper with explicit u16 length validation
        let write_dict = |buf: &mut Vec<u8>, list: &[String]| -> Result<(), StorageError> {
            if list.len() >= u16::MAX as usize {
                return Err(StorageError::Encoding(
                    "Dictionary entry count exceeds u16 limit".into(),
                ));
            }
            buf.write_u16::<BigEndian>(list.len() as u16)?;
            for item in list {
                let bytes = item.as_bytes();
                if bytes.len() > u16::MAX as usize {
                    return Err(StorageError::Encoding(format!(
                        "Metadata string length ({}) exceeds 65535-byte limit",
                        bytes.len()
                    )));
                }
                buf.write_u16::<BigEndian>(bytes.len() as u16)?;
                buf.write_all(bytes)?;
            }
            Ok(())
        };

        write_dict(&mut raw_buf, &players)?;
        write_dict(&mut raw_buf, &events)?;
        write_dict(&mut raw_buf, &sites)?;
        write_dict(&mut raw_buf, &rounds)?;
        write_dict(&mut raw_buf, &dates)?;
        write_dict(&mut raw_buf, &ecos)?;
        write_dict(&mut raw_buf, &time_controls)?;
        write_dict(&mut raw_buf, &source_ids)?;
        write_dict(&mut raw_buf, &broadcast_urls)?;
        write_dict(&mut raw_buf, &fens)?;

        // Games serialization
        for g in &self.games {
            raw_buf.write_u64::<BigEndian>(g.metadata.id)?;

            let w_idx = *player_map
                .get(&g.metadata.white)
                .ok_or_else(|| StorageError::Encoding("Missing interned white player".into()))?;
            let b_idx = *player_map
                .get(&g.metadata.black)
                .ok_or_else(|| StorageError::Encoding("Missing interned black player".into()))?;
            raw_buf.write_u16::<BigEndian>(w_idx)?;
            raw_buf.write_u16::<BigEndian>(b_idx)?;

            let get_idx = |val: &Option<String>, map: &HashMap<String, u16>| {
                val.as_ref()
                    .and_then(|s| map.get(s))
                    .copied()
                    .unwrap_or(u16::MAX)
            };

            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.event, &event_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.site, &site_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.round, &round_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.date, &date_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.eco, &eco_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.time_control, &tc_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.source_id, &source_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.broadcast_url, &broadcast_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.fen_start, &fen_map))?;

            raw_buf.write_u16::<BigEndian>(g.metadata.white_elo.unwrap_or(0))?;
            raw_buf.write_u16::<BigEndian>(g.metadata.black_elo.unwrap_or(0))?;

            // Result (1 byte)
            let res_byte = match g.metadata.result {
                GameResult::WhiteWins => 1,
                GameResult::Draw => 2,
                GameResult::BlackWins => 3,
                GameResult::Star => 0,
            };
            raw_buf.write_u8(res_byte)?;

            // Moves: count (u16) + [u16; count]
            if g.moves.len() > u16::MAX as usize {
                return Err(StorageError::Encoding(format!(
                    "Game move count ({}) exceeds 65535 plies",
                    g.moves.len()
                )));
            }
            raw_buf.write_u16::<BigEndian>(g.moves.len() as u16)?;
            for m in &g.moves {
                raw_buf.write_u16::<BigEndian>(m.0)?;
            }
        }

        let uncompressed_len = raw_buf.len();

        // Compress buffer with Zstandard
        let compressed = zstd::encode_all(Cursor::new(raw_buf), zstd_level)
            .map_err(|e| StorageError::Zstd(e.to_string()))?;

        Ok((compressed, uncompressed_len))
    }

    /// Decompresses and deserializes games from a compressed block payload.
    pub fn decompress(
        compressed: &[u8],
        collection: Collection,
    ) -> Result<Vec<ParsedGame>, StorageError> {
        let raw_bytes =
            zstd::decode_all(compressed).map_err(|e| StorageError::Zstd(e.to_string()))?;
        let mut r = Cursor::new(raw_bytes);

        // Verify magic
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != BLOCK_MAGIC_V3 {
            return Err(StorageError::InvalidMagic);
        }

        let game_count = r.read_u32::<BigEndian>()? as usize;

        let read_dict = |r: &mut Cursor<Vec<u8>>| -> Result<Vec<String>, StorageError> {
            let count = r.read_u16::<BigEndian>()? as usize;
            let mut list = Vec::with_capacity(count);
            for _ in 0..count {
                let len = r.read_u16::<BigEndian>()? as usize;
                let mut buf = vec![0u8; len];
                r.read_exact(&mut buf)?;
                let s = String::from_utf8(buf)
                    .map_err(|e| StorageError::Decoding(format!("Invalid UTF-8 in dict: {}", e)))?;
                list.push(s);
            }
            Ok(list)
        };

        let players = read_dict(&mut r)?;
        let events = read_dict(&mut r)?;
        let sites = read_dict(&mut r)?;
        let rounds = read_dict(&mut r)?;
        let dates = read_dict(&mut r)?;
        let ecos = read_dict(&mut r)?;
        let time_controls = read_dict(&mut r)?;
        let source_ids = read_dict(&mut r)?;
        let broadcast_urls = read_dict(&mut r)?;
        let fens = read_dict(&mut r)?;

        let mut games = Vec::with_capacity(game_count);

        let resolve_str =
            |idx: u16, list: &[String], field: &str| -> Result<Option<String>, StorageError> {
                if idx == u16::MAX {
                    Ok(None)
                } else {
                    list.get(idx as usize).cloned().map(Some).ok_or_else(|| {
                        StorageError::Decoding(format!(
                            "Dictionary index {} out of bounds for {}",
                            idx, field
                        ))
                    })
                }
            };

        for _ in 0..game_count {
            let id = r.read_u64::<BigEndian>()?;
            let w_idx = r.read_u16::<BigEndian>()? as usize;
            let b_idx = r.read_u16::<BigEndian>()? as usize;

            let ev_idx = r.read_u16::<BigEndian>()?;
            let st_idx = r.read_u16::<BigEndian>()?;
            let rd_idx = r.read_u16::<BigEndian>()?;
            let dt_idx = r.read_u16::<BigEndian>()?;
            let eco_idx = r.read_u16::<BigEndian>()?;
            let tc_idx = r.read_u16::<BigEndian>()?;
            let src_idx = r.read_u16::<BigEndian>()?;
            let burl_idx = r.read_u16::<BigEndian>()?;
            let fen_idx = r.read_u16::<BigEndian>()?;

            let white = players.get(w_idx).cloned().ok_or_else(|| {
                StorageError::Decoding(format!("White player index {} out of bounds", w_idx))
            })?;
            let black = players.get(b_idx).cloned().ok_or_else(|| {
                StorageError::Decoding(format!("Black player index {} out of bounds", b_idx))
            })?;

            let event = resolve_str(ev_idx, &events, "event")?;
            let site = resolve_str(st_idx, &sites, "site")?;
            let round = resolve_str(rd_idx, &rounds, "round")?;
            let date = resolve_str(dt_idx, &dates, "date")?;
            let eco = resolve_str(eco_idx, &ecos, "eco")?;
            let time_control = resolve_str(tc_idx, &time_controls, "time_control")?;
            let source_id = resolve_str(src_idx, &source_ids, "source_id")?;
            let broadcast_url = resolve_str(burl_idx, &broadcast_urls, "broadcast_url")?;
            let fen_start = resolve_str(fen_idx, &fens, "fen_start")?;

            let w_elo_raw = r.read_u16::<BigEndian>()?;
            let b_elo_raw = r.read_u16::<BigEndian>()?;
            let white_elo = if w_elo_raw > 0 { Some(w_elo_raw) } else { None };
            let black_elo = if b_elo_raw > 0 { Some(b_elo_raw) } else { None };

            let res_byte = r.read_u8()?;
            let result = match res_byte {
                1 => GameResult::WhiteWins,
                2 => GameResult::Draw,
                3 => GameResult::BlackWins,
                0 => GameResult::Star,
                other => {
                    return Err(StorageError::Decoding(format!(
                        "Invalid game result byte: {}",
                        other
                    )))
                }
            };

            let move_count = r.read_u16::<BigEndian>()? as usize;
            let mut moves = Vec::with_capacity(move_count);
            for _ in 0..move_count {
                moves.push(CompactMove(r.read_u16::<BigEndian>()?));
            }

            games.push(ParsedGame {
                metadata: GameMetadata {
                    id,
                    collection,
                    white,
                    black,
                    white_elo,
                    black_elo,
                    date,
                    event,
                    site,
                    round,
                    result,
                    eco,
                    time_control,
                    source_id,
                    broadcast_url,
                    source_attribution: None,
                },
                moves,
                fen_start,
            });
        }

        Ok(games)
    }
}

/// Reconstructs a full, standard PGN string from a `ParsedGame`.
pub fn export_to_pgn(game: &ParsedGame) -> Result<String, StorageError> {
    let mut pgn = String::with_capacity(512);

    // Headers
    pgn.push_str(&format!(
        "[Event \"{}\"]\n",
        game.metadata.event.as_deref().unwrap_or("?")
    ));
    pgn.push_str(&format!(
        "[Site \"{}\"]\n",
        game.metadata.site.as_deref().unwrap_or("?")
    ));
    pgn.push_str(&format!(
        "[Date \"{}\"]\n",
        game.metadata.date.as_deref().unwrap_or("????.??.??")
    ));
    pgn.push_str(&format!(
        "[Round \"{}\"]\n",
        game.metadata.round.as_deref().unwrap_or("?")
    ));
    pgn.push_str(&format!("[White \"{}\"]\n", game.metadata.white));
    pgn.push_str(&format!("[Black \"{}\"]\n", game.metadata.black));
    pgn.push_str(&format!("[Result \"{}\"]\n", game.metadata.result.to_str()));

    if let Some(elo) = game.metadata.white_elo {
        pgn.push_str(&format!("[WhiteElo \"{}\"]\n", elo));
    }
    if let Some(elo) = game.metadata.black_elo {
        pgn.push_str(&format!("[BlackElo \"{}\"]\n", elo));
    }
    if let Some(eco) = &game.metadata.eco {
        pgn.push_str(&format!("[ECO \"{}\"]\n", eco));
    }
    if let Some(tc) = &game.metadata.time_control {
        pgn.push_str(&format!("[TimeControl \"{}\"]\n", tc));
    }
    if let Some(burl) = &game.metadata.broadcast_url {
        pgn.push_str(&format!("[BroadcastURL \"{}\"]\n", burl));
    }
    if let Some(src) = &game.metadata.source_id {
        pgn.push_str(&format!("[GameURL \"{}\"]\n", src));
    }
    if let Some(fen) = &game.fen_start {
        pgn.push_str(&format!("[SetUp \"1\"]\n[FEN \"{}\"]\n", fen));
    }
    pgn.push('\n');

    // Replay moves legally to generate standard SAN
    let mut board = Chess::default();
    let mut move_num = 1;

    for (i, &cm) in game.moves.iter().enumerate() {
        let is_white = i % 2 == 0;
        if is_white {
            pgn.push_str(&format!("{}. ", move_num));
        }

        let m = cm.to_shakmaty_move(&board).ok_or_else(|| {
            StorageError::Decoding(format!(
                "Cannot reconstruct move {} ({}) on board FEN",
                i + 1,
                cm.to_uci()
            ))
        })?;

        let san = SanPlus::from_move_and_play_unchecked(&mut board, m);
        pgn.push_str(&format!("{} ", san));

        if !is_white {
            move_num += 1;
        }
    }

    pgn.push_str(game.metadata.result.to_str());
    pgn.push('\n');

    Ok(pgn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_block_round_trip_full_metadata() {
        let mut block = GameBlock::new(Collection::MastersOtb);

        let g1 = ParsedGame {
            metadata: GameMetadata {
                id: 1001,
                collection: Collection::MastersOtb,
                white: "Carlsen, Magnus".into(),
                black: "Nakamura, Hikaru".into(),
                white_elo: Some(2850),
                black_elo: Some(2800),
                date: Some("2024.01.15".into()),
                event: Some("Tata Steel 2024".into()),
                site: Some("Wijk aan Zee NED".into()),
                round: Some("5".into()),
                result: GameResult::Draw,
                eco: Some("C65".into()),
                time_control: Some("90+30".into()),
                source_id: Some(
                    "https://lichess.org/broadcast/tata-steel-2024/round-5/abc12345/def67890"
                        .into(),
                ),
                broadcast_url: Some(
                    "https://lichess.org/broadcast/tata-steel-2024/round-5/abc12345".into(),
                ),
                source_attribution: None,
            },
            moves: vec![
                CompactMove::new(12, 28, None), // e2e4
                CompactMove::new(52, 36, None), // e7e5
                CompactMove::new(6, 21, None),  // g1f3
                CompactMove::new(57, 42, None), // b8c6
            ],
            fen_start: None,
        };

        block.games.push(g1.clone());

        // Compress
        let compressed = block.compress(3).expect("compression failed");
        assert!(!compressed.is_empty());

        // Decompress
        let decoded = GameBlock::decompress(&compressed, Collection::MastersOtb)
            .expect("decompression failed");
        assert_eq!(decoded.len(), 1);

        let recovered = &decoded[0];
        assert_eq!(recovered, &g1);

        // PGN Export Round-trip
        let pgn_text = export_to_pgn(recovered).expect("export to pgn failed");
        assert!(pgn_text.contains("[White \"Carlsen, Magnus\"]"));
        assert!(pgn_text.contains("[Round \"5\"]"));
        assert!(pgn_text.contains("[ECO \"C65\"]"));
        assert!(pgn_text.contains("[TimeControl \"90+30\"]"));
        assert!(pgn_text.contains(
            "[BroadcastURL \"https://lichess.org/broadcast/tata-steel-2024/round-5/abc12345\"]"
        ));
        assert!(pgn_text.contains(
            "[GameURL \"https://lichess.org/broadcast/tata-steel-2024/round-5/abc12345/def67890\"]"
        ));
        assert!(pgn_text.contains("1. e4 e5 2. Nf3 Nc6 1/2-1/2"));
    }

    #[test]
    fn test_storage_no_silent_truncation_for_extended_eco_and_dates() {
        let mut block = GameBlock::new(Collection::MastersOtb);

        let g_extended = ParsedGame {
            metadata: GameMetadata {
                id: 2002,
                collection: Collection::MastersOtb,
                white: "Ikeda, Junta".into(),
                black: "Hegde, Sanat".into(),
                white_elo: Some(2366),
                black_elo: Some(1867),
                // Non-10-byte extended date range
                date: Some("2026.07.31-2026.08.02".into()),
                event: Some("2026 ANU Open".into()),
                site: Some("Canberra, Australia".into()),
                round: Some("2.1".into()),
                result: GameResult::WhiteWins,
                // Extended >3-byte sub-ECO classification
                eco: Some("B10a/12".into()),
                time_control: Some("60m+30s".into()),
                source_id: Some(
                    "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots/Qmy8doCq".into(),
                ),
                broadcast_url: Some(
                    "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots".into(),
                ),
                source_attribution: None,
            },
            moves: vec![
                CompactMove::new(12, 28, None), // e2e4
                CompactMove::new(50, 42, None), // c7c6
                CompactMove::new(6, 21, None),  // g1f3
                CompactMove::new(51, 35, None), // d7d5
                CompactMove::new(28, 35, None), // e4d5
                CompactMove::new(42, 35, None), // c6d5
            ],
            fen_start: None,
        };

        block.games.push(g_extended.clone());
        let compressed = block.compress(3).unwrap();
        let decoded = GameBlock::decompress(&compressed, Collection::MastersOtb).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].metadata.eco.as_deref(), Some("B10a/12"));
        assert_eq!(
            decoded[0].metadata.date.as_deref(),
            Some("2026.07.31-2026.08.02")
        );
        assert_eq!(decoded[0], g_extended);
    }

    #[test]
    fn test_incompatible_block_magic_rejected() {
        let mut bad_raw = Vec::new();
        bad_raw.extend_from_slice(BLOCK_MAGIC_V2); // Old CGB2 magic
        bad_raw.extend_from_slice(&[0u8; 32]);
        let compressed_v2 = zstd::encode_all(Cursor::new(bad_raw), 3).unwrap();
        match GameBlock::decompress(&compressed_v2, Collection::MastersOtb) {
            Err(StorageError::InvalidMagic) => {}
            other => panic!("Expected InvalidMagic for CGB2, got {:?}", other),
        }

        let mut bad_raw2 = Vec::new();
        bad_raw2.extend_from_slice(b"CGB1");
        bad_raw2.extend_from_slice(&[0u8; 32]);
        let compressed_v1 = zstd::encode_all(Cursor::new(bad_raw2), 3).unwrap();
        match GameBlock::decompress(&compressed_v1, Collection::MastersOtb) {
            Err(StorageError::InvalidMagic) => {}
            other => panic!("Expected InvalidMagic for CGB1, got {:?}", other),
        }
    }

    #[test]
    fn test_special_moves_round_trip_and_pgn_export() {
        // Test sequence containing:
        // 1. e4 d5 2. exd5 c6 3. dxc6 Nf6 4. cxb7 e5 5. bxa8=N Bc5 6. Nf3 O-O
        let moves = vec![
            CompactMove::new(12, 28, None),    // 1. e4
            CompactMove::new(51, 35, None),    // 1... d5
            CompactMove::new(28, 35, None),    // 2. exd5
            CompactMove::new(50, 42, None),    // 2... c6
            CompactMove::new(35, 42, None),    // 3. dxc6
            CompactMove::new(62, 45, None),    // 3... Nf6
            CompactMove::new(42, 49, None),    // 4. cxb7
            CompactMove::new(52, 36, None),    // 4... e5
            CompactMove::new(49, 56, Some(1)), // 5. bxa8=N (underpromotion to Knight)
            CompactMove::new(61, 34, None),    // 5... Bc5
            CompactMove::new(6, 21, None),     // 6. Nf3
            CompactMove::new(60, 62, None),    // 6... O-O (kingside castle)
        ];

        let game = ParsedGame {
            metadata: GameMetadata {
                id: 3003,
                collection: Collection::MastersOtb,
                white: "Player1".into(),
                black: "Player2".into(),
                white_elo: Some(2500),
                black_elo: Some(2500),
                date: Some("2026.07.31".into()),
                event: Some("Special Move Suite".into()),
                site: Some("Canberra".into()),
                round: Some("1".into()),
                result: GameResult::WhiteWins,
                eco: Some("B01".into()),
                time_control: Some("90+30".into()),
                source_id: None,
                broadcast_url: None,
                source_attribution: None,
            },
            moves,
            fen_start: None,
        };

        let mut block = GameBlock::new(Collection::MastersOtb);
        block.games.push(game.clone());
        let compressed = block.compress(3).unwrap();
        let decompressed = GameBlock::decompress(&compressed, Collection::MastersOtb).unwrap();
        assert_eq!(decompressed[0], game);

        let pgn = export_to_pgn(&decompressed[0]).unwrap();
        assert!(pgn.contains("5. bxa8=N Bc5 6. Nf3 O-O 1-0"));
    }
}
