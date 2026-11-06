use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Collection {
    #[serde(rename = "masters-otb")]
    MastersOtb,
    #[serde(rename = "elite-online")]
    EliteOnline,
}

impl Collection {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Collection::MastersOtb => "masters-otb",
            Collection::EliteOnline => "elite-online",
        }
    }
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

    /// Resolves this CompactMove into a legal Shakmaty Move on the provided position.
    pub fn to_shakmaty_move(self, board: &shakmaty::Chess) -> Option<shakmaty::Move> {
        use shakmaty::{Move, Position, Role, Square};
        let from = Square::new(self.from_sq() as u32);
        let to = Square::new(self.to_sq() as u32);
        let promo = self.promotion_piece().map(|p| match p {
            1 => Role::Knight,
            2 => Role::Bishop,
            3 => Role::Rook,
            4 => Role::Queen,
            _ => Role::Queen,
        });

        board.legal_moves().into_iter().find(|cand| match *cand {
            Move::Normal {
                from: f,
                to: t,
                promotion,
                ..
            } => f == from && t == to && promotion == promo,
            Move::EnPassant { from: f, to: t } => f == from && t == to,
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
                king == from && to_sq == to
            }
            _ => false,
        })
    }
}

/// Computes a canonical, 128-bit collision-resistant `PositionKey` from a Shakmaty Chess position.
pub fn compute_position_key(pos: &shakmaty::Chess) -> PositionKey {
    use shakmaty::zobrist::Zobrist128;
    use shakmaty::{EnPassantMode, Position};
    let zobrist: Zobrist128 = pos.zobrist_hash(EnPassantMode::Legal);
    PositionKey(zobrist.0)
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

/// Identifies the physical location of a stored game within a persistent sharded dataset:
/// shard index, compressed block index within that shard, and game offset within the block.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct GameLocation {
    pub shard_id: u16,
    pub block_index: u32,
    pub game_index_in_block: u16,
}

impl GameLocation {
    pub fn new(shard_id: u16, block_index: u32, game_index_in_block: u16) -> Self {
        GameLocation {
            shard_id,
            block_index,
            game_index_in_block,
        }
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
    /// Game-specific identifier or canonical game URL (e.g. `GameURL` or per-game `Site` URL).
    pub source_id: Option<String>,
    /// Tournament or broadcast-level URL provenance (e.g. `BroadcastURL`), never conflated with per-game `source_id`.
    #[serde(default)]
    pub broadcast_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedGame {
    pub metadata: GameMetadata,
    pub moves: Vec<CompactMove>,
    pub fen_start: Option<String>,
}

/// Computes a stable, deterministic 64-bit game ID from its provenance and canonical content.
pub fn compute_stable_game_id(
    collection: Collection,
    source_id: Option<&str>,
    white: &str,
    black: &str,
    date: Option<&str>,
    round: Option<&str>,
    moves: &[CompactMove],
) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let col_byte = match collection {
        Collection::MastersOtb => 1u8,
        Collection::EliteOnline => 2u8,
    };
    hash ^= col_byte as u64;
    hash = hash.wrapping_mul(0x100000001b3);

    let mix_str = |h: &mut u64, s: &str| {
        for b in s.trim().to_ascii_lowercase().bytes() {
            *h ^= b as u64;
            *h = h.wrapping_mul(0x100000001b3);
        }
        *h ^= 0xFF;
        *h = h.wrapping_mul(0x100000001b3);
    };

    if let Some(src) = source_id {
        mix_str(&mut hash, src);
    }
    mix_str(&mut hash, white);
    mix_str(&mut hash, black);
    if let Some(d) = date {
        mix_str(&mut hash, d);
    }
    if let Some(r) = round {
        mix_str(&mut hash, r);
    }

    for m in moves {
        hash ^= (m.0 & 0xFF) as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        hash ^= ((m.0 >> 8) & 0xFF) as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }

    hash
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum IngestionCompletionStatus {
    #[default]
    CompleteEof,
    PartialLimitReached,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestionStats {
    pub total_encountered: u64,
    pub accepted_unique: u64,
    pub confirmed_duplicates: u64,
    pub superseded_prior: u64,
    pub rejected_by_policy: u64,
    pub rejected_variant: u64,
    pub rejected_illegal: u64,
    pub rejected_malformed: u64,
    pub quarantined: u64,
    #[serde(default)]
    pub completion_status: IngestionCompletionStatus,
}

impl IngestionStats {
    /// Invariant: all mutually exclusive categories must sum exactly to total_encountered.
    pub fn is_reconciled(&self) -> bool {
        self.total_encountered == self.reconciled_sum()
    }

    /// Invariant: retained canonical games count must equal `accepted_unique`
    /// (superseding replacements replace an existing accepted game in-place without incrementing `accepted_unique`).
    pub fn verify_retained_count(&self, retained_len: usize) -> bool {
        self.accepted_unique == retained_len as u64
    }

    /// Full accounting consistency check across category reconciliation and retained canonical games.
    pub fn is_fully_consistent(&self, retained_len: usize) -> bool {
        self.is_reconciled() && self.verify_retained_count(retained_len)
    }

    pub fn reconciled_sum(&self) -> u64 {
        self.accepted_unique
            + self.confirmed_duplicates
            + self.superseded_prior
            + self.rejected_by_policy
            + self.rejected_variant
            + self.rejected_illegal
            + self.rejected_malformed
            + self.quarantined
    }
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

    #[test]
    fn test_stable_game_id_determinism() {
        let moves = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
        ];
        let id1 = compute_stable_game_id(
            Collection::MastersOtb,
            Some("broadcast-101"),
            "Carlsen, M",
            "Nakamura, H",
            Some("2024.01.15"),
            Some("1"),
            &moves,
        );
        let id2 = compute_stable_game_id(
            Collection::MastersOtb,
            Some("broadcast-101"),
            "Carlsen, M",
            "Nakamura, H",
            Some("2024.01.15"),
            Some("1"),
            &moves,
        );
        assert_eq!(id1, id2);

        // Test GameLocation ordering and construction
        let loc1 = GameLocation::new(0, 5, 12);
        let loc2 = GameLocation::new(0, 5, 13);
        assert!(loc1 < loc2);

        // Different collection or moves produces different ID
        let id3 = compute_stable_game_id(
            Collection::EliteOnline,
            Some("broadcast-101"),
            "Carlsen, M",
            "Nakamura, H",
            Some("2024.01.15"),
            Some("1"),
            &moves,
        );
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_ingestion_stats_reconciliation() {
        let stats = IngestionStats {
            total_encountered: 100,
            accepted_unique: 70,
            confirmed_duplicates: 10,
            superseded_prior: 2,
            rejected_by_policy: 10,
            rejected_variant: 3,
            rejected_illegal: 2,
            rejected_malformed: 2,
            quarantined: 1,
            completion_status: IngestionCompletionStatus::CompleteEof,
        };
        assert!(stats.is_reconciled());
        assert!(stats.verify_retained_count(70));
        assert!(!stats.verify_retained_count(72));
        assert!(stats.is_fully_consistent(70));
    }
}
