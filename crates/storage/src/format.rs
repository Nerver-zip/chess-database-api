use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use codb_core::{Collection, CompactMove, GameMetadata, GameResult, ParsedGame};
use shakmaty::san::SanPlus;
use shakmaty::{Chess, Move, Position, Role, Square};
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use thiserror::Error;

pub const BLOCK_MAGIC_V2: &[u8; 4] = b"CGB2";

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Zstd compression error: {0}")]
    Zstd(String),
    #[error("Decoding error: {0}")]
    Decoding(String),
    #[error("Invalid block magic or version")]
    InvalidMagic,
    #[error("Game ID not found: {0}")]
    GameNotFound(u64),
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
    /// Preserves 100% of game metadata: players, ratings, date, event, site, round, result,
    /// ECO, time control, source ID, starting FEN, and legal moves.
    pub fn compress(&self, zstd_level: i32) -> Result<Vec<u8>, StorageError> {
        let mut raw_buf = Vec::with_capacity(self.games.len() * 140);

        // Magic and format version header
        raw_buf.write_all(BLOCK_MAGIC_V2)?;

        // String dictionaries within this block
        let mut players: Vec<String> = Vec::new();
        let mut player_map: HashMap<String, u16> = HashMap::new();

        let mut events: Vec<String> = Vec::new();
        let mut event_map: HashMap<String, u16> = HashMap::new();

        let mut sites: Vec<String> = Vec::new();
        let mut site_map: HashMap<String, u16> = HashMap::new();

        let mut rounds: Vec<String> = Vec::new();
        let mut round_map: HashMap<String, u16> = HashMap::new();

        let mut time_controls: Vec<String> = Vec::new();
        let mut tc_map: HashMap<String, u16> = HashMap::new();

        let mut source_ids: Vec<String> = Vec::new();
        let mut source_map: HashMap<String, u16> = HashMap::new();

        let mut fens: Vec<String> = Vec::new();
        let mut fen_map: HashMap<String, u16> = HashMap::new();

        let intern =
            |val: &Option<String>, list: &mut Vec<String>, map: &mut HashMap<String, u16>| {
                if let Some(s) = val {
                    if !map.contains_key(s) {
                        let idx = list.len() as u16;
                        map.insert(s.clone(), idx);
                        list.push(s.clone());
                    }
                }
            };

        for g in &self.games {
            if !player_map.contains_key(&g.metadata.white) {
                let idx = players.len() as u16;
                player_map.insert(g.metadata.white.clone(), idx);
                players.push(g.metadata.white.clone());
            }
            if !player_map.contains_key(&g.metadata.black) {
                let idx = players.len() as u16;
                player_map.insert(g.metadata.black.clone(), idx);
                players.push(g.metadata.black.clone());
            }
            intern(&g.metadata.event, &mut events, &mut event_map);
            intern(&g.metadata.site, &mut sites, &mut site_map);
            intern(&g.metadata.round, &mut rounds, &mut round_map);
            intern(&g.metadata.time_control, &mut time_controls, &mut tc_map);
            intern(&g.metadata.source_id, &mut source_ids, &mut source_map);
            intern(&g.fen_start, &mut fens, &mut fen_map);
        }

        // Header: game count (u32)
        raw_buf.write_u32::<BigEndian>(self.games.len() as u32)?;

        // Write dictionaries helper
        let write_dict = |buf: &mut Vec<u8>, list: &[String]| -> Result<(), StorageError> {
            buf.write_u16::<BigEndian>(list.len() as u16)?;
            for item in list {
                let bytes = item.as_bytes();
                buf.write_u16::<BigEndian>(bytes.len() as u16)?;
                buf.write_all(bytes)?;
            }
            Ok(())
        };

        write_dict(&mut raw_buf, &players)?;
        write_dict(&mut raw_buf, &events)?;
        write_dict(&mut raw_buf, &sites)?;
        write_dict(&mut raw_buf, &rounds)?;
        write_dict(&mut raw_buf, &time_controls)?;
        write_dict(&mut raw_buf, &source_ids)?;
        write_dict(&mut raw_buf, &fens)?;

        // Games serialization
        for g in &self.games {
            raw_buf.write_u64::<BigEndian>(g.metadata.id)?;

            let w_idx = *player_map.get(&g.metadata.white).unwrap_or(&0);
            let b_idx = *player_map.get(&g.metadata.black).unwrap_or(&0);
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
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.time_control, &tc_map))?;
            raw_buf.write_u16::<BigEndian>(get_idx(&g.metadata.source_id, &source_map))?;
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

            // ECO: 3 ASCII bytes e.g. "B90" or empty spaces
            let mut eco_bytes = [b' '; 3];
            if let Some(eco) = &g.metadata.eco {
                let src = eco.as_bytes();
                let len = src.len().min(3);
                eco_bytes[..len].copy_from_slice(&src[..len]);
            }
            raw_buf.write_all(&eco_bytes)?;

            // Date: 10 bytes ASCII fixed buffer e.g. "2024.01.15" or empty spaces
            let mut date_bytes = [b' '; 10];
            if let Some(d) = &g.metadata.date {
                let src = d.as_bytes();
                let len = src.len().min(10);
                date_bytes[..len].copy_from_slice(&src[..len]);
            }
            raw_buf.write_all(&date_bytes)?;

            // Moves: count (u16) + [u16; count]
            raw_buf.write_u16::<BigEndian>(g.moves.len() as u16)?;
            for m in &g.moves {
                raw_buf.write_u16::<BigEndian>(m.0)?;
            }
        }

        // Compress buffer with Zstandard
        zstd::encode_all(Cursor::new(raw_buf), zstd_level)
            .map_err(|e| StorageError::Zstd(e.to_string()))
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
        if &magic != BLOCK_MAGIC_V2 {
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
                list.push(String::from_utf8_lossy(&buf).to_string());
            }
            Ok(list)
        };

        let players = read_dict(&mut r)?;
        let events = read_dict(&mut r)?;
        let sites = read_dict(&mut r)?;
        let rounds = read_dict(&mut r)?;
        let time_controls = read_dict(&mut r)?;
        let source_ids = read_dict(&mut r)?;
        let fens = read_dict(&mut r)?;

        let mut games = Vec::with_capacity(game_count);

        let resolve_str = |idx: u16, list: &[String]| -> Option<String> {
            if idx == u16::MAX {
                None
            } else {
                list.get(idx as usize).cloned()
            }
        };

        for _ in 0..game_count {
            let id = r.read_u64::<BigEndian>()?;
            let w_idx = r.read_u16::<BigEndian>()? as usize;
            let b_idx = r.read_u16::<BigEndian>()? as usize;

            let ev_idx = r.read_u16::<BigEndian>()?;
            let st_idx = r.read_u16::<BigEndian>()?;
            let rd_idx = r.read_u16::<BigEndian>()?;
            let tc_idx = r.read_u16::<BigEndian>()?;
            let src_idx = r.read_u16::<BigEndian>()?;
            let fen_idx = r.read_u16::<BigEndian>()?;

            let white = players.get(w_idx).cloned().unwrap_or_default();
            let black = players.get(b_idx).cloned().unwrap_or_default();

            let event = resolve_str(ev_idx, &events);
            let site = resolve_str(st_idx, &sites);
            let round = resolve_str(rd_idx, &rounds);
            let time_control = resolve_str(tc_idx, &time_controls);
            let source_id = resolve_str(src_idx, &source_ids);
            let fen_start = resolve_str(fen_idx, &fens);

            let w_elo_raw = r.read_u16::<BigEndian>()?;
            let b_elo_raw = r.read_u16::<BigEndian>()?;
            let white_elo = if w_elo_raw > 0 { Some(w_elo_raw) } else { None };
            let black_elo = if b_elo_raw > 0 { Some(b_elo_raw) } else { None };

            let res_byte = r.read_u8()?;
            let result = match res_byte {
                1 => GameResult::WhiteWins,
                2 => GameResult::Draw,
                3 => GameResult::BlackWins,
                _ => GameResult::Star,
            };

            let mut eco_bytes = [0u8; 3];
            r.read_exact(&mut eco_bytes)?;
            let eco_str = String::from_utf8_lossy(&eco_bytes).trim().to_string();
            let eco = if eco_str.is_empty() {
                None
            } else {
                Some(eco_str)
            };

            let mut date_bytes = [0u8; 10];
            r.read_exact(&mut date_bytes)?;
            let date_str = String::from_utf8_lossy(&date_bytes).trim().to_string();
            let date = if date_str.is_empty() {
                None
            } else {
                Some(date_str)
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
                    } else if king.rank() == shakmaty::Rank::First {
                        Square::C1
                    } else {
                        Square::C8
                    };
                    *king == from && to_sq == to
                }
                _ => false,
            })
            .ok_or_else(|| {
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
                source_id: Some("https://lichess.org/broadcast/123".into()),
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
        // FULL ROUND TRIP FIDELITY CHECK (Finding 2.1)
        assert_eq!(recovered.metadata.id, g1.metadata.id);
        assert_eq!(recovered.metadata.white, g1.metadata.white);
        assert_eq!(recovered.metadata.black, g1.metadata.black);
        assert_eq!(recovered.metadata.white_elo, g1.metadata.white_elo);
        assert_eq!(recovered.metadata.black_elo, g1.metadata.black_elo);
        assert_eq!(recovered.metadata.date, g1.metadata.date);
        assert_eq!(recovered.metadata.event, g1.metadata.event);
        assert_eq!(recovered.metadata.site, g1.metadata.site);
        assert_eq!(recovered.metadata.round, g1.metadata.round);
        assert_eq!(recovered.metadata.result, g1.metadata.result);
        assert_eq!(recovered.metadata.eco, g1.metadata.eco);
        assert_eq!(recovered.metadata.time_control, g1.metadata.time_control);
        assert_eq!(recovered.metadata.source_id, g1.metadata.source_id);
        assert_eq!(recovered.moves, g1.moves);

        // PGN Export Round-trip
        let pgn_text = export_to_pgn(recovered).expect("export to pgn failed");
        assert!(pgn_text.contains("[White \"Carlsen, Magnus\"]"));
        assert!(pgn_text.contains("[Round \"5\"]"));
        assert!(pgn_text.contains("[ECO \"C65\"]"));
        assert!(pgn_text.contains("[TimeControl \"90+30\"]"));
        assert!(pgn_text.contains("1. e4 e5 2. Nf3 Nc6 1/2-1/2"));
    }
}
