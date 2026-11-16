//! Reusable chess position validation, move parsing, and SAN notation generation.

use crate::error::ApiError;
use opening_explorer_core::PositionKey;
use opening_explorer_index::position::compute_position_key;
use sha2::{Digest, Sha256};
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

    for (index, token) in tokens.into_iter().enumerate() {
        let ply = index + 1;
        // A token is read as SAN and as UCI. It is accepted if either reading is legal here.
        let san = San::from_ascii(token.as_bytes()).ok().or_else(|| {
            SanPlus::from_ascii(token.as_bytes())
                .ok()
                .map(|san_plus| san_plus.san)
        });
        let uci = token.parse::<UciMove>().ok();
        let played_move = san
            .as_ref()
            .and_then(|san| san.to_move(board).ok())
            .or_else(|| uci.as_ref().and_then(|uci| uci.to_move(board).ok()));

        match played_move {
            Some(m) => board.play_unchecked(m),
            // Notation that parses but is not legal in this position.
            None if san.is_some() || uci.is_some() => {
                return Err(ApiError::InvalidMoves(format!(
                    "Illegal move '{}' at ply {}.",
                    token, ply
                )));
            }
            None => {
                return Err(ApiError::InvalidMoves(format!(
                    "Invalid move token '{}' at ply {}: expected SAN or UCI notation.",
                    token, ply
                )));
            }
        }
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

/// Computes an RFC 7232 compliant strong ETag for a position response representation.
///
/// Incorporates collection identity, dataset build ID, canonical 128-bit position key,
/// and the complete FEN string (including move clock counters) to guarantee that
/// distinct response representations never share an ETag.
pub fn compute_position_etag(
    collection: &str,
    dataset_build_id: &str,
    key_hex: &str,
    fen: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(collection.as_bytes());
    hasher.update(b":");
    hasher.update(dataset_build_id.as_bytes());
    hasher.update(b":");
    hasher.update(key_hex.as_bytes());
    hasher.update(b":");
    hasher.update(fen.as_bytes());
    let fen_digest = format!("{:x}", hasher.finalize());
    format!(
        "\"{}-{}-{}-{}\"",
        collection,
        dataset_build_id,
        key_hex,
        &fen_digest[..16]
    )
}

/// Matches an `If-None-Match` header value against a target ETag per RFC 7232.
/// Supports wildcard `*`, weak comparison prefixes (`W/`), and comma-separated lists of ETags.
pub fn if_none_match_matches(header_value: &str, target_etag: &str) -> bool {
    let clean_target = target_etag.trim().trim_start_matches("W/");
    for token in header_value.split(',') {
        let token = token.trim();
        if token == "*" {
            return true;
        }
        let clean_token = token.trim_start_matches("W/");
        if clean_token == clean_target {
            return true;
        }
    }
    false
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

    #[test]
    fn test_if_none_match_matching() {
        let etag = "\"build_01-100\"";
        assert!(if_none_match_matches("*", etag));
        assert!(if_none_match_matches("\"build_01-100\"", etag));
        assert!(if_none_match_matches("W/\"build_01-100\"", etag));
        assert!(if_none_match_matches("\"other\", \"build_01-100\"", etag));
        assert!(if_none_match_matches("\"other\", W/\"build_01-100\"", etag));
        assert!(!if_none_match_matches("\"other\"", etag));
        assert!(!if_none_match_matches("\"build_01-200\"", etag));
    }
}
