use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Collection {
    #[serde(rename = "masters-otb")]
    MastersOtb,
    #[serde(rename = "elite-online")]
    EliteOnline,
}

impl std::fmt::Display for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Collection::MastersOtb => write!(f, "masters-otb"),
            Collection::EliteOnline => write!(f, "elite-online"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum GameResult {
    WhiteWins,
    Draw,
    BlackWins,
    #[default]
    Star,
}

impl GameResult {
    pub fn from_str_lenient(s: &str) -> Self {
        match s.trim() {
            "1-0" => GameResult::WhiteWins,
            "0-1" => GameResult::BlackWins,
            "1/2-1/2" => GameResult::Draw,
            _ => GameResult::Star,
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            GameResult::WhiteWins => "1-0",
            GameResult::BlackWins => "0-1",
            GameResult::Draw => "1/2-1/2",
            GameResult::Star => "*",
        }
    }
}

/// 16-bit packed move representation.
/// Bits 0..5: From square (0..63)
/// Bits 6..11: To square (0..63)
/// Bits 12..15: Promotion flags:
///   0 = None
///   1 = Knight
///   2 = Bishop
///   3 = Rook
///   4 = Queen
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct CompactMove(pub u16);

impl CompactMove {
    #[inline]
    pub fn new(from_sq: u8, to_sq: u8, promotion: Option<u8>) -> Self {
        let promo_bits = match promotion {
            Some(1) => 1, // Knight
            Some(2) => 2, // Bishop
            Some(3) => 3, // Rook
            Some(4) => 4, // Queen
            _ => 0,
        };
        let val = (from_sq as u16 & 0x3F)
            | ((to_sq as u16 & 0x3F) << 6)
            | ((promo_bits as u16 & 0x0F) << 12);
        CompactMove(val)
    }

    #[inline]
    pub fn from_sq(self) -> u8 {
        (self.0 & 0x3F) as u8
    }

    #[inline]
    pub fn to_sq(self) -> u8 {
        ((self.0 >> 6) & 0x3F) as u8
    }

    #[inline]
    pub fn promotion_piece(self) -> Option<u8> {
        match (self.0 >> 12) & 0x0F {
            1 => Some(1), // Knight
            2 => Some(2), // Bishop
            3 => Some(3), // Rook
            4 => Some(4), // Queen
            _ => None,
        }
    }

    /// Convert to UCI string e.g. "e2e4", "e7e8q"
    pub fn to_uci(self) -> String {
        let from = sq_to_str(self.from_sq());
        let to = sq_to_str(self.to_sq());
        let promo = match self.promotion_piece() {
            Some(1) => "n",
            Some(2) => "b",
            Some(3) => "r",
            Some(4) => "q",
            _ => "",
        };
        format!("{}{}{}", from, to, promo)
    }
}

fn sq_to_str(sq: u8) -> String {
    let file = (sq % 8) + b'a';
    let rank = (sq / 8) + b'1';
    format!("{}{}", file as char, rank as char)
}

/// 128-bit collision-resistant position key.
/// High 64 bits: Primary Zobrist hash.
/// Low 64 bits: Secondary orthogonal polynomial hash of canonical position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PositionKey(pub u128);

impl PositionKey {
    pub fn new(primary: u64, secondary: u64) -> Self {
        PositionKey(((primary as u128) << 64) | (secondary as u128))
    }

    pub fn primary(self) -> u64 {
        (self.0 >> 64) as u64
    }

    pub fn secondary(self) -> u64 {
        self.0 as u64
    }

    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameMetadata {
    pub id: u64,
    pub collection: Collection,
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
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedGame {
    pub metadata: GameMetadata,
    pub moves: Vec<CompactMove>,
    pub fen_start: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IngestionStatus {
    Accepted,
    RejectedVariant,
    RejectedFilter,
    RejectedIllegal,
    Quarantined,
    DuplicateSuperseded,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct IngestionStats {
    pub total_encountered: u64,
    pub parsed_games: u64,
    pub accepted_games: u64,
    pub rejected_variant: u64,
    pub rejected_filter: u64,
    pub rejected_illegal: u64,
    pub quarantined: u64,
    pub duplicate_candidates: u64,
    pub confirmed_duplicates: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_move_encoding() {
        // e2 (sq 12) -> e4 (sq 28)
        let m = CompactMove::new(12, 28, None);
        assert_eq!(m.from_sq(), 12);
        assert_eq!(m.to_sq(), 28);
        assert_eq!(m.promotion_piece(), None);
        assert_eq!(m.to_uci(), "e2e4");

        // e7 (sq 52) -> e8 (sq 60), promotion to Queen (4)
        let promo = CompactMove::new(52, 60, Some(4));
        assert_eq!(promo.from_sq(), 52);
        assert_eq!(promo.to_sq(), 60);
        assert_eq!(promo.promotion_piece(), Some(4));
        assert_eq!(promo.to_uci(), "e7e8q");
    }

    #[test]
    fn test_position_key_parts() {
        let key = PositionKey::new(0x123456789ABCDEF0, 0xFEDCBA9876543210);
        assert_eq!(key.primary(), 0x123456789ABCDEF0);
        assert_eq!(key.secondary(), 0xFEDCBA9876543210);
        assert_eq!(key.to_hex(), "123456789abcdef0fedcba9876543210");
    }

    #[test]
    fn test_game_result() {
        assert_eq!(GameResult::from_str_lenient("1-0"), GameResult::WhiteWins);
        assert_eq!(GameResult::from_str_lenient("0-1"), GameResult::BlackWins);
        assert_eq!(GameResult::from_str_lenient("1/2-1/2"), GameResult::Draw);
        assert_eq!(GameResult::from_str_lenient("*"), GameResult::Star);
    }
}
