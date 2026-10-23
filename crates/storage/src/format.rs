use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use codb_core::{Collection, CompactMove, GameMetadata, GameResult, ParsedGame};
use shakmaty::san::SanPlus;
use shakmaty::{Chess, Move, Position, Role, Square};
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Zstd compression error: {0}")]
    Zstd(String),
    #[error("Decoding error: {0}")]
    Decoding(String),
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
    pub fn compress(&self, zstd_level: i32) -> Result<Vec<u8>, StorageError> {
        let mut raw_buf = Vec::with_capacity(self.games.len() * 128);

        // 1. Dictionaries for strings within this block: players, events, sites
        let mut players: Vec<String> = Vec::new();
        let mut player_map: HashMap<String, u16> = HashMap::new();

        let mut events: Vec<String> = Vec::new();
        let mut event_map: HashMap<String, u16> = HashMap::new();

        let mut sites: Vec<String> = Vec::new();
        let mut site_map: HashMap<String, u16> = HashMap::new();

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
            if let Some(ev) = &g.metadata.event {
                if !event_map.contains_key(ev) {
                    let idx = events.len() as u16;
                    event_map.insert(ev.clone(), idx);
                    events.push(ev.clone());
                }
            }
            if let Some(st) = &g.metadata.site {
                if !site_map.contains_key(st) {
                    let idx = sites.len() as u16;
                    site_map.insert(st.clone(), idx);
                    sites.push(st.clone());
                }
            }
        }

        // Header: game count (u32), dictionary counts
        raw_buf.write_u32::<BigEndian>(self.games.len() as u32)?;
        raw_buf.write_u16::<BigEndian>(players.len() as u16)?;
        for p in &players {
            let bytes = p.as_bytes();
            raw_buf.write_u16::<BigEndian>(bytes.len() as u16)?;
            raw_buf.write_all(bytes)?;
        }

        raw_buf.write_u16::<BigEndian>(events.len() as u16)?;
        for e in &events {
            let bytes = e.as_bytes();
            raw_buf.write_u16::<BigEndian>(bytes.len() as u16)?;
            raw_buf.write_all(bytes)?;
        }

        raw_buf.write_u16::<BigEndian>(sites.len() as u16)?;
        for s in &sites {
            let bytes = s.as_bytes();
            raw_buf.write_u16::<BigEndian>(bytes.len() as u16)?;
            raw_buf.write_all(bytes)?;
        }

        // Games serialization
        for g in &self.games {
            raw_buf.write_u64::<BigEndian>(g.metadata.id)?;

            let w_idx = *player_map.get(&g.metadata.white).unwrap_or(&0);
            let b_idx = *player_map.get(&g.metadata.black).unwrap_or(&0);
            raw_buf.write_u16::<BigEndian>(w_idx)?;
            raw_buf.write_u16::<BigEndian>(b_idx)?;

            let ev_idx = g
                .metadata
                .event
                .as_ref()
                .and_then(|e| event_map.get(e))
                .copied()
                .unwrap_or(u16::MAX);
            raw_buf.write_u16::<BigEndian>(ev_idx)?;

            let st_idx = g
                .metadata
                .site
                .as_ref()
                .and_then(|s| site_map.get(s))
                .copied()
                .unwrap_or(u16::MAX);
            raw_buf.write_u16::<BigEndian>(st_idx)?;

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

            // Date: 10 bytes ASCII fixed buffer e.g. "2024.01.15" or empty
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

        let game_count = r.read_u32::<BigEndian>()? as usize;

        // Player dictionary
        let player_count = r.read_u16::<BigEndian>()? as usize;
        let mut players = Vec::with_capacity(player_count);
        for _ in 0..player_count {
            let len = r.read_u16::<BigEndian>()? as usize;
            let mut buf = vec![0u8; len];
            r.read_exact(&mut buf)?;
            players.push(String::from_utf8_lossy(&buf).to_string());
        }

        // Event dictionary
        let event_count = r.read_u16::<BigEndian>()? as usize;
        let mut events = Vec::with_capacity(event_count);
        for _ in 0..event_count {
            let len = r.read_u16::<BigEndian>()? as usize;
            let mut buf = vec![0u8; len];
            r.read_exact(&mut buf)?;
            events.push(String::from_utf8_lossy(&buf).to_string());
        }

        // Site dictionary
        let site_count = r.read_u16::<BigEndian>()? as usize;
        let mut sites = Vec::with_capacity(site_count);
        for _ in 0..site_count {
            let len = r.read_u16::<BigEndian>()? as usize;
            let mut buf = vec![0u8; len];
            r.read_exact(&mut buf)?;
            sites.push(String::from_utf8_lossy(&buf).to_string());
        }

        let mut games = Vec::with_capacity(game_count);

        for _ in 0..game_count {
            let id = r.read_u64::<BigEndian>()?;
            let w_idx = r.read_u16::<BigEndian>()? as usize;
            let b_idx = r.read_u16::<BigEndian>()? as usize;
            let ev_idx = r.read_u16::<BigEndian>()?;
            let st_idx = r.read_u16::<BigEndian>()?;

            let white = players.get(w_idx).cloned().unwrap_or_default();
            let black = players.get(b_idx).cloned().unwrap_or_default();

            let event = if ev_idx == u16::MAX {
                None
            } else {
                events.get(ev_idx as usize).cloned()
            };
            let site = if st_idx == u16::MAX {
                None
            } else {
                sites.get(st_idx as usize).cloned()
            };

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
                    round: None,
                    result,
                    eco: None,
                    time_control: None,
                    source_id: None,
                },
                moves,
                fen_start: None,
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
    pgn.push('\n');

    // Replay moves legally to generate standard SAN
    let mut board = Chess::default();
    let mut move_num = 1;

    for (i, &cm) in game.moves.iter().enumerate() {
        let is_white = i % 2 == 0;
        if is_white {
            pgn.push_str(&format!("{}. ", move_num));
        }

        // Find the legal move corresponding to cm
        let from = Square::new(cm.from_sq() as u32);
        let to = Square::new(cm.to_sq() as u32);
        let promo = cm.promotion_piece().map(|p| match p {
            1 => Role::Knight,
            2 => Role::Bishop,
            3 => Role::Rook,
            4 => Role::Queen,
            _ => Role::Queen,
        });

        // Match against legal moves
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
    fn test_storage_block_round_trip() {
        let mut block = GameBlock::new(Collection::MastersOtb);

        // Game 1: 1. e4 e5 2. Nf3 Nc6
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
                round: Some("1".into()),
                result: GameResult::Draw,
                eco: Some("C65".into()),
                time_control: Some("90+30".into()),
                source_id: None,
            },
            moves: vec![
                CompactMove::new(12, 28, None), // e2e4
                CompactMove::new(52, 36, None), // e7e5
                CompactMove::new(6, 21, None),  // g1f3
                CompactMove::new(57, 42, None), // b8c6
            ],
            fen_start: None,
        };

        block.games.push(g1);

        // Compress
        let compressed = block.compress(3).expect("compression failed");
        assert!(!compressed.is_empty());

        // Decompress
        let decoded = GameBlock::decompress(&compressed, Collection::MastersOtb)
            .expect("decompression failed");
        assert_eq!(decoded.len(), 1);

        let recovered = &decoded[0];
        assert_eq!(recovered.metadata.id, 1001);
        assert_eq!(recovered.metadata.white, "Carlsen, Magnus");
        assert_eq!(recovered.metadata.black, "Nakamura, Hikaru");
        assert_eq!(recovered.metadata.white_elo, Some(2850));
        assert_eq!(recovered.metadata.black_elo, Some(2800));
        assert_eq!(recovered.metadata.date.as_deref(), Some("2024.01.15"));
        assert_eq!(recovered.moves.len(), 4);
        assert_eq!(recovered.moves[0].to_uci(), "e2e4");
        assert_eq!(recovered.moves[1].to_uci(), "e7e5");
        assert_eq!(recovered.moves[2].to_uci(), "g1f3");
        assert_eq!(recovered.moves[3].to_uci(), "b8c6");

        // PGN Export Round-trip
        let pgn_text = export_to_pgn(recovered).expect("export to pgn failed");
        assert!(pgn_text.contains("[White \"Carlsen, Magnus\"]"));
        assert!(pgn_text.contains("1. e4 e5 2. Nf3 Nc6 1/2-1/2"));
    }
}
