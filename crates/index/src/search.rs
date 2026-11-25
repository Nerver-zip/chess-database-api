//! Canonical occurrence extraction for positional game discovery.

pub mod aggregate;
pub mod catalog;
pub mod control;
pub mod metadata;
pub mod postings;
pub mod query;
pub mod spill;

use crate::position::IndexError;
use opening_explorer_core::{compute_position_key, CompactMove, ParsedGame, PositionKey};
use shakmaty::{fen::Fen, uci::UciMove, CastlingMode, Chess, Move, Position, Role, Square};

/// A position reached in the canonical mainline. The starting position is at ply zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositionOccurrence {
    pub key: PositionKey,
    pub ply: u16,
    pub played: Option<CompactMove>,
}

/// Visit every legal occurrence up to and including the configured ply.
/// Full coverage is selected with `None`. Repetitions, unknown results, and a
/// game's final recorded board are included. A position at the coverage boundary
/// retains its actual recorded continuation when one exists.
pub fn visit_position_occurrences(
    game: &ParsedGame,
    depth: Option<u16>,
    mut visit: impl FnMut(PositionOccurrence) -> Result<(), IndexError>,
) -> Result<(), IndexError> {
    let id = game.metadata.id;
    let length = u16::try_from(game.moves.len())
        .map_err(|_| IndexError::Overflow(format!("game {id} exceeds 65535 plies")))?;
    let last = depth.unwrap_or(length).min(length);
    let mut board: Chess = match &game.fen_start {
        Some(text) => {
            let fen = text.parse::<Fen>().map_err(|error| {
                IndexError::Reconstruction(format!("Invalid starting FEN in game {id}: {error}"))
            })?;
            fen.into_position(CastlingMode::Standard).map_err(|error| {
                IndexError::Reconstruction(format!("Illegal starting FEN in game {id}: {error}"))
            })?
        }
        None => Chess::default(),
    };

    for ply in 0..=last {
        let key = compute_position_key(&board);
        let played = game.moves.get(usize::from(ply)).copied();
        if let Some(compact) = played {
            let next = resolve_compact(compact, &board).ok_or_else(|| {
                IndexError::Reconstruction(format!(
                    "Illegal move at ply {} ({}) in game {id}",
                    u32::from(ply) + 1,
                    compact.to_uci()
                ))
            })?;
            board.play_unchecked(next);
        }
        visit(PositionOccurrence { key, ply, played })?;
    }
    Ok(())
}

fn resolve_compact(compact: CompactMove, board: &Chess) -> Option<Move> {
    let promotion = match compact.0 >> 12 {
        0 => None,
        1 => Some(Role::Knight),
        2 => Some(Role::Bishop),
        3 => Some(Role::Rook),
        4 => Some(Role::Queen),
        _ => return None,
    };
    let uci = UciMove::Normal {
        from: Square::new(u32::from(compact.from_sq())),
        to: Square::new(u32::from(compact.to_sq())),
        promotion,
    };
    let next = uci.to_move(board).ok()?;
    (UciMove::from_standard(next) == uci).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::extract_game_positions_detailed;
    use opening_explorer_core::{Collection, GameMetadata, GameResult};

    fn game(fen_start: Option<String>, moves: Vec<CompactMove>) -> ParsedGame {
        ParsedGame {
            metadata: GameMetadata {
                id: u64::MAX,
                collection: Collection::MastersOtb,
                white: "White".into(),
                black: "Black".into(),
                white_elo: None,
                black_elo: None,
                date: None,
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
            moves,
            fen_start,
        }
    }

    fn occurrences(game: &ParsedGame, depth: Option<u16>) -> Vec<PositionOccurrence> {
        let mut observed = Vec::new();
        visit_position_occurrences(game, depth, |occurrence| {
            observed.push(occurrence);
            Ok(())
        })
        .unwrap();
        observed
    }

    #[test]
    fn repetitions_keep_distinct_continuations_even_for_unknown_results() {
        let moves = vec![
            CompactMove::new(6, 21, None),
            CompactMove::new(62, 45, None),
            CompactMove::new(21, 6, None),
            CompactMove::new(45, 62, None),
            CompactMove::new(12, 28, None),
        ];
        let mut game = game(None, moves.clone());
        let all = occurrences(&game, None);
        assert_eq!(all.len(), 6);
        assert_eq!(all[0].key, all[4].key);
        assert_eq!(all[0].played, Some(moves[0]));
        assert_eq!(all[4].played, Some(moves[4]));
        assert_eq!((all[5].ply, all[5].played), (5, None));
        assert_eq!(occurrences(&game, Some(4)), all[..5]);
        assert_eq!(occurrences(&game, Some(2)), all[..3]);

        let legacy = extract_game_positions_detailed(&game, 5).unwrap();
        assert!(legacy.entries.is_empty());
        game.metadata.result = GameResult::WhiteWins;
        let legacy = extract_game_positions_detailed(&game, 5).unwrap();
        assert_eq!(legacy.repeated_positions_skipped, 1);
        assert_eq!(legacy.entries[0].1, moves[0]);
        assert!(!legacy.entries.iter().any(|entry| entry.1 == moves[4]));
    }

    #[test]
    fn compact_legal_resolution_agrees_with_existing_replay_on_special_moves() {
        for fen in [
            "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1",
            "4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1",
            "4k3/P7/8/8/8/8/8/4K3 w - - 0 1",
        ] {
            let board: Chess = fen
                .parse::<Fen>()
                .unwrap()
                .into_position(CastlingMode::Standard)
                .unwrap();
            for from in 0..64 {
                for to in 0..64 {
                    for promotion in [None, Some(1), Some(2), Some(3), Some(4)] {
                        let compact = CompactMove::new(from, to, promotion);
                        let expected = compact.to_shakmaty_move(&board).filter(|played| {
                            promotion.is_none()
                                || matches!(
                                    played,
                                    Move::Normal {
                                        promotion: Some(_),
                                        ..
                                    }
                                )
                        });
                        assert_eq!(resolve_compact(compact, &board), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn custom_starting_board_and_illegal_boundary_continuation() {
        let fen = "4k3/8/8/8/8/8/8/4K3 w - - 0 1";
        let mut game = game(Some(fen.into()), vec![CompactMove::new(4, 12, None)]);
        let all = occurrences(&game, None);
        let board: Chess = fen
            .parse::<Fen>()
            .unwrap()
            .into_position(CastlingMode::Standard)
            .unwrap();
        assert_eq!(all[0].key, compute_position_key(&board));
        assert_eq!(all[1].played, None);

        game.moves[0] = CompactMove::new(4, 60, None);
        assert!(visit_position_occurrences(&game, Some(0), |_| Ok(())).is_err());
    }
}
