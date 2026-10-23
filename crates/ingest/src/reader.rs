use codb_core::{Collection, CompactMove, GameMetadata, GameResult, IngestionStats, ParsedGame};
use pgn_reader::{RawTag, Reader, SanPlus, Visitor};
use shakmaty::{Chess, Move, Position, Role, Square};
use std::io::Read;
use std::ops::ControlFlow;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum IngestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Zstd decompression error: {0}")]
    Zstd(String),
    #[error("Move replay error at ply {ply}: {reason}")]
    IllegalMove { ply: usize, reason: String },
    #[error("Unsupported chess variant: {0}")]
    UnsupportedVariant(String),
    #[error("Malformed PGN: {0}")]
    MalformedPgn(String),
}

#[derive(Default, Debug)]
pub struct GameTags {
    pub white: String,
    pub black: String,
    pub white_elo: Option<u16>,
    pub black_elo: Option<u16>,
    pub date: Option<String>,
    pub event: Option<String>,
    pub site: Option<String>,
    pub round: Option<String>,
    pub result: GameResult,
    pub eco: Option<String>,
    pub time_control: Option<String>,
    pub variant: Option<String>,
    pub fen_start: Option<String>,
    pub source_id: Option<String>,
}

pub struct GameMovetext {
    pub tags: GameTags,
    pub board: Chess,
    pub moves: Vec<CompactMove>,
    pub legal_replay_failed: bool,
    pub failure_ply: usize,
    pub failure_reason: String,
    pub ply_count: usize,
}

pub struct ShakmatyGameVisitor {
    pub collection: Collection,
    pub next_id: u64,
}

impl ShakmatyGameVisitor {
    pub fn new(collection: Collection, starting_id: u64) -> Self {
        ShakmatyGameVisitor {
            collection,
            next_id: starting_id,
        }
    }
}

impl Visitor for ShakmatyGameVisitor {
    type Tags = GameTags;
    type Movetext = GameMovetext;
    type Output = Result<ParsedGame, IngestError>;

    fn begin_tags(&mut self) -> ControlFlow<Self::Output, Self::Tags> {
        ControlFlow::Continue(GameTags {
            result: GameResult::Star,
            ..Default::default()
        })
    }

    fn tag(
        &mut self,
        tags: &mut Self::Tags,
        name: &[u8],
        value: RawTag<'_>,
    ) -> ControlFlow<Self::Output> {
        let val_cow = value.decode();
        let val_str = String::from_utf8_lossy(&val_cow);
        match name {
            b"White" => tags.white = val_str.trim().to_string(),
            b"Black" => tags.black = val_str.trim().to_string(),
            b"WhiteElo" => tags.white_elo = val_str.trim().parse::<u16>().ok(),
            b"BlackElo" => tags.black_elo = val_str.trim().parse::<u16>().ok(),
            b"Date" => {
                let d = val_str.trim();
                if !d.is_empty() && d != "?" {
                    tags.date = Some(d.to_string());
                }
            }
            b"Event" => tags.event = Some(val_str.trim().to_string()),
            b"Site" => {
                let s = val_str.trim().to_string();
                if s.starts_with("http") {
                    tags.source_id = Some(s.clone());
                }
                tags.site = Some(s);
            }
            b"Round" => tags.round = Some(val_str.trim().to_string()),
            b"Result" => tags.result = GameResult::from_str_lenient(&val_str),
            b"ECO" => tags.eco = Some(val_str.trim().to_string()),
            b"TimeControl" => tags.time_control = Some(val_str.trim().to_string()),
            b"Variant" => tags.variant = Some(val_str.trim().to_string()),
            b"FEN" => tags.fen_start = Some(val_str.trim().to_string()),
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: Self::Tags) -> ControlFlow<Self::Output, Self::Movetext> {
        if let Some(v) = &tags.variant {
            let v_lower = v.to_ascii_lowercase();
            if v_lower != "standard" && v_lower != "chess" {
                return ControlFlow::Break(Err(IngestError::UnsupportedVariant(v.clone())));
            }
        }

        ControlFlow::Continue(GameMovetext {
            tags,
            board: Chess::default(),
            moves: Vec::with_capacity(64),
            legal_replay_failed: false,
            failure_ply: 0,
            failure_reason: String::new(),
            ply_count: 0,
        })
    }

    fn san(
        &mut self,
        movetext: &mut Self::Movetext,
        san_plus: SanPlus,
    ) -> ControlFlow<Self::Output> {
        if movetext.legal_replay_failed {
            return ControlFlow::Continue(());
        }
        movetext.ply_count += 1;

        match san_plus.san.to_move(&movetext.board) {
            Ok(m) => {
                let compact = match &m {
                    Move::Normal {
                        from,
                        to,
                        promotion,
                        ..
                    } => {
                        let promo_val = match promotion {
                            Some(Role::Knight) => Some(1),
                            Some(Role::Bishop) => Some(2),
                            Some(Role::Rook) => Some(3),
                            Some(Role::Queen) => Some(4),
                            _ => None,
                        };
                        CompactMove::new(u8::from(*from), u8::from(*to), promo_val)
                    }
                    Move::EnPassant { from, to } => {
                        CompactMove::new(u8::from(*from), u8::from(*to), None)
                    }
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
                        CompactMove::new(u8::from(*king), u8::from(to_sq), None)
                    }
                    Move::Put { .. } => {
                        movetext.legal_replay_failed = true;
                        movetext.failure_ply = movetext.ply_count;
                        movetext.failure_reason = "Drop move in standard chess".into();
                        return ControlFlow::Continue(());
                    }
                };

                movetext.moves.push(compact);
                movetext.board.play_unchecked(m);
                ControlFlow::Continue(())
            }
            Err(e) => {
                movetext.legal_replay_failed = true;
                movetext.failure_ply = movetext.ply_count;
                movetext.failure_reason = format!("Illegal SAN move '{}': {}", san_plus, e);
                ControlFlow::Continue(())
            }
        }
    }

    fn end_game(&mut self, movetext: Self::Movetext) -> Self::Output {
        if movetext.legal_replay_failed {
            return Err(IngestError::IllegalMove {
                ply: movetext.failure_ply,
                reason: movetext.failure_reason,
            });
        }

        let id = self.next_id;
        self.next_id += 1;

        let meta = GameMetadata {
            id,
            collection: self.collection,
            white: movetext.tags.white,
            black: movetext.tags.black,
            white_elo: movetext.tags.white_elo,
            black_elo: movetext.tags.black_elo,
            date: movetext.tags.date,
            event: movetext.tags.event,
            site: movetext.tags.site,
            round: movetext.tags.round,
            result: movetext.tags.result,
            eco: movetext.tags.eco,
            time_control: movetext.tags.time_control,
            source_id: movetext.tags.source_id,
        };

        Ok(ParsedGame {
            metadata: meta,
            moves: movetext.moves,
            fen_start: movetext.tags.fen_start,
        })
    }
}

/// Helper function to parse all games from a PGN reader with tracking statistics.
pub fn parse_pgn_stream<R: Read>(
    reader: R,
    collection: Collection,
    stats: &mut IngestionStats,
) -> Vec<ParsedGame> {
    let mut pgn_reader = Reader::new(reader);
    let mut visitor = ShakmatyGameVisitor::new(collection, stats.accepted_games + 1);
    let mut games = Vec::new();

    while let Ok(Some(result)) = pgn_reader.read_game(&mut visitor) {
        stats.total_encountered += 1;
        match result {
            Ok(game) => {
                stats.parsed_games += 1;
                stats.accepted_games += 1;
                games.push(game);
            }
            Err(IngestError::UnsupportedVariant(_)) => {
                stats.rejected_variant += 1;
            }
            Err(IngestError::IllegalMove { ply, reason }) => {
                eprintln!("Rejected illegal move at ply {}: {}", ply, reason);
                stats.rejected_illegal += 1;
            }
            Err(_) => {
                stats.quarantined += 1;
            }
        }
    }

    games
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PGN: &str = r#"
[Event "FIDE World Championship 2021"]
[Site "Dubai UAE"]
[Date "2021.12.03"]
[Round "6"]
[White "Carlsen, Magnus"]
[Black "Nepomniachtchi, Ian"]
[Result "1-0"]
[WhiteElo "2855"]
[BlackElo "2782"]
[ECO "D02"]

1. d4 Nf6 2. Nf3 d5 3. g3 e6 4. Bg2 Be7 5. O-O O-O 6. b3 c5 7. dxc5 Bxc5 1-0

[Event "Illegal Move Test"]
[Site "Test"]
[Date "2024.01.01"]
[White "Player1"]
[Black "Player2"]
[Result "*"]

1. e4 e5 2. Qh5 Ke7 3. Qxe5# *

[Event "Invalid Game"]
[Site "Test"]
[Date "2024.01.01"]
[White "Cheater"]
[Black "Victim"]
[Result "*"]

1. e4 e5 2. Nf3 Ke7 3. Nxe5 Ke6 4. d4 Ba3 5. Nxa3 Nh6 6. Bxh6 g6 7. Bc4+ d5 8. Bxd5+ Ke7 9. Qf3 Kd6 10. Nxf7+ Ke7 11. Bg5+ Kd7 12. Qg4+ Ke8 13. Nd6+ Qxd6 14. Qxc8+ Qd8 15. Qxd8# 1-0
"#;

    #[test]
    fn test_parse_pgn_stream() {
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(SAMPLE_PGN.as_bytes(), Collection::MastersOtb, &mut stats);

        assert_eq!(stats.total_encountered, 3);
        assert_eq!(stats.accepted_games, 3);
        assert_eq!(games.len(), 3);

        // Verify Carlsen vs Nepo game
        let g1 = &games[0];
        assert_eq!(g1.metadata.white, "Carlsen, Magnus");
        assert_eq!(g1.metadata.black, "Nepomniachtchi, Ian");
        assert_eq!(g1.metadata.white_elo, Some(2855));
        assert_eq!(g1.metadata.result, GameResult::WhiteWins);
        assert_eq!(g1.moves.len(), 14); // 7 full moves = 14 plies
    }

    #[test]
    fn test_illegal_move_rejection() {
        let bad_pgn = r#"
[Event "Illegal Knight Teleport"]
[White "Bad"]
[Black "Good"]
[Result "*"]

1. e4 e5 2. Na6 0-1
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(bad_pgn.as_bytes(), Collection::MastersOtb, &mut stats);

        assert_eq!(stats.total_encountered, 1);
        assert_eq!(stats.rejected_illegal, 1);
        assert_eq!(games.len(), 0);
    }

    #[test]
    fn test_unsupported_variant_rejection() {
        let variant_pgn = r#"
[Event "Crazyhouse"]
[Variant "Crazyhouse"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 *
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(variant_pgn.as_bytes(), Collection::MastersOtb, &mut stats);

        assert_eq!(stats.total_encountered, 1);
        assert_eq!(stats.rejected_variant, 1);
        assert_eq!(games.len(), 0);
    }

    #[test]
    fn test_castling_both_sides() {
        let pgn = r#"
[Event "Castling Kingside Test"]
[White "White"]
[Black "Black"]
[Result "1/2-1/2"]

1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. O-O Nf6 5. d3 O-O 1/2-1/2

[Event "Castling Queenside Test"]
[White "White"]
[Black "Black"]
[Result "1/2-1/2"]

1. d4 d5 2. Nc3 Nc6 3. Bf4 Bf5 4. Qd2 Qd7 5. O-O-O O-O-O 1/2-1/2
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats);

        assert_eq!(stats.accepted_games, 2);
        // Game 1: O-O
        let g1 = &games[0];
        assert_eq!(g1.moves[6].to_uci(), "e1g1");
        assert_eq!(g1.moves[9].to_uci(), "e8g8");

        // Game 2: O-O-O
        let g2 = &games[1];
        assert_eq!(g2.moves[8].to_uci(), "e1c1");
        assert_eq!(g2.moves[9].to_uci(), "e8c8");
    }

    #[test]
    fn test_en_passant() {
        let pgn = r#"
[Event "En Passant Test"]
[White "White"]
[Black "Black"]
[Result "*"]

1. e4 c5 2. e5 d5 3. exd6 *
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats);

        assert_eq!(stats.accepted_games, 1);
        let g = &games[0];
        assert_eq!(g.moves.len(), 5);
        // Move 5: exd6 -> from e5 (sq 36) to d6 (sq 43)
        assert_eq!(g.moves[4].to_uci(), "e5d6");
    }

    #[test]
    fn test_underpromotion() {
        // Construct standard promotion sequence:
        // 1. e4 d5 2. exd5 c6 3. dxc6 Nf6 4. cxb7 e5 5. bxa8=N
        let pgn = r#"
[Event "Promotion Test"]
[White "White"]
[Black "Black"]
[Result "*"]

1. e4 d5 2. exd5 c6 3. dxc6 Nf6 4. cxb7 e5 5. bxa8=N *
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats);

        assert_eq!(stats.accepted_games, 1);
        let g = &games[0];
        let last_move = g.moves.last().unwrap();
        assert_eq!(last_move.to_uci(), "b7a8n");
        assert_eq!(last_move.promotion_piece(), Some(1)); // 1 = Knight
    }

    #[test]
    fn test_unicode_and_escaped_headers() {
        let pgn = r#"
[Event "Torneo Internacional \"Memorial Capablanca\""]
[Site "La Habana CUB"]
[Date "1962.05.01"]
[Round "1"]
[White "Najdorf, Miguel"]
[Black "Spassky, Boris V"]
[Result "1/2-1/2"]
[WhiteElo "2580"]
[BlackElo "2640"]

1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 1/2-1/2
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats);

        assert_eq!(stats.accepted_games, 1);
        let g = &games[0];
        assert_eq!(g.metadata.white, "Najdorf, Miguel");
        assert_eq!(g.metadata.black, "Spassky, Boris V");
        assert_eq!(
            g.metadata.event.as_deref(),
            Some("Torneo Internacional \"Memorial Capablanca\"")
        );
    }
}
