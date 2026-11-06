//! Reusable chess position validation, move parsing, and SAN notation generation.

use crate::error::ApiError;
use codb_core::PositionKey;
use codb_index::position::compute_position_key;
use shakmaty::fen::Fen;
use shakmaty::san::{San, SanPlus};
use shakmaty::uci::UciMove;
use shakmaty::{CastlingMode, Chess, EnPassantMode, Position};

/// Parses a FEN string and constructs a validated [`Chess`] position.
pub fn parse_fen(fen_str: &str) -> Result<Chess, ApiError> {
    let parsed: Fen = fen_str
        .parse()
        .map_err(|e| ApiError::InvalidFen(format!("Syntax error: {}", e)))?;

    parsed
        .into_position(CastlingMode::Standard)
        .map_err(|e| ApiError::InvalidFen(format!("Illegal chess position: {:?}", e)))
}

/// Applies a sequence of moves (separated by commas or whitespace, in either SAN or UCI notation)
/// to the given board starting from its current position.
pub fn parse_and_apply_moves(
    board: &mut Chess,
    moves_str: &str,
    max_plies: usize,
) -> Result<(), ApiError> {
    let tokens: Vec<&str> = moves_str
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if tokens.len() > max_plies {
        return Err(ApiError::MoveSequenceTooLong(tokens.len(), max_plies));
    }

    for token in tokens {
        // Try parsing as SAN first, then SAN Plus, then UCI
        let played_move = if let Ok(san) = San::from_ascii(token.as_bytes()) {
            san.to_move(board).ok()
        } else if let Ok(san_plus) = SanPlus::from_ascii(token.as_bytes()) {
            san_plus.san.to_move(board).ok()
        } else {
            None
        };

        let m = match played_move {
            Some(m) => m,
            None => {
                let uci: UciMove = token.parse().map_err(|e| {
                    ApiError::InvalidMoves(format!("Invalid move token '{}': {}", token, e))
                })?;
                uci.to_move(board).map_err(|e| {
                    ApiError::InvalidMoves(format!("Illegal move '{}': {:?}", token, e))
                })?
            }
        };

        board.play_unchecked(m);
    }

    Ok(())
}

/// Computes the canonical 128-bit [`PositionKey`] for the board.
pub fn compute_key(board: &Chess) -> PositionKey {
    compute_position_key(board)
}

/// Formats the board into a canonical FEN string.
pub fn format_fen(board: &Chess) -> String {
    Fen::from_position(board, EnPassantMode::Legal).to_string()
}

/// Derives standard Algebraic Notation (SAN) for a UCI candidate move on the given board.
pub fn derive_san(board: &Chess, uci_str: &str) -> String {
    if let Ok(uci_m) = uci_str.parse::<UciMove>() {
        if let Ok(legal_m) = uci_m.to_move(board) {
            return SanPlus::from_move(board.clone(), legal_m).to_string();
        }
    }
    uci_str.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_position() {
        let board = Chess::default();
        let key = compute_key(&board);
        assert_ne!(key.0, 0);
        assert_eq!(
            format_fen(&board),
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
        );
    }

    #[test]
    fn test_move_parsing_san_and_uci() {
        let mut board = Chess::default();
        parse_and_apply_moves(&mut board, "e4, e5, Nf3, b8c6", 200).expect("Valid moves");
        assert_eq!(
            format_fen(&board),
            "r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3"
        );
    }

    #[test]
    fn test_move_limit_enforcement() {
        let mut board = Chess::default();
        let err = parse_and_apply_moves(&mut board, "e4, e5, Nf3", 2).unwrap_err();
        assert_eq!(err.error_code(), "move_sequence_too_long");
    }

    #[test]
    fn test_illegal_move_rejection() {
        let mut board = Chess::default();
        let err = parse_and_apply_moves(&mut board, "e4, e4", 200).unwrap_err();
        assert_eq!(err.error_code(), "invalid_moves");
    }

    #[test]
    fn test_invalid_fen_rejection() {
        let err = parse_fen("invalid_fen_string").unwrap_err();
        assert_eq!(err.error_code(), "invalid_fen");
    }
}
