use codb_core::{CompactMove, ParsedGame};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateVerdict {
    Unique,
    ConfirmedDuplicate { canonical_id: u64, reason: String },
    SupersedesPrevious { old_id: u64, reason: String },
    TranspositionDifferentGame,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedMetadataKey {
    pub white_norm: String,
    pub black_norm: String,
    pub year: Option<u16>,
    pub event_norm: String,
    pub round: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SeenGameRecord {
    pub id: u64,
    pub move_hash: u64,
    pub ply_count: usize,
    pub metadata_key: NormalizedMetadataKey,
    pub has_full_date: bool,
    pub has_ratings: bool,
}

pub struct Deduplicator {
    /// Maps normalized metadata key -> list of seen games with this metadata
    by_metadata: HashMap<NormalizedMetadataKey, Vec<SeenGameRecord>>,
    /// Maps move sequence hash -> list of seen games with this exact move sequence
    by_moves: HashMap<u64, Vec<SeenGameRecord>>,
}

impl Default for Deduplicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Deduplicator {
    pub fn new() -> Self {
        Deduplicator {
            by_metadata: HashMap::new(),
            by_moves: HashMap::new(),
        }
    }

    /// Check if a newly parsed game is a duplicate, supersedes an earlier incomplete record,
    /// or is unique.
    pub fn evaluate(&mut self, game: &ParsedGame) -> DuplicateVerdict {
        let move_hash = compute_move_hash(&game.moves);
        let meta_key = normalize_metadata(game);
        let ply_count = game.moves.len();
        let has_full_date = game
            .metadata
            .date
            .as_ref()
            .is_some_and(|d| !d.contains('?'));
        let has_ratings = game.metadata.white_elo.is_some() || game.metadata.black_elo.is_some();

        // Check for matches by metadata
        if let Some(existing_games) = self.by_metadata.get_mut(&meta_key) {
            for existing in existing_games.iter_mut() {
                // Same metadata + same moves = Exact duplicate
                if existing.move_hash == move_hash {
                    return DuplicateVerdict::ConfirmedDuplicate {
                        canonical_id: existing.id,
                        reason: "Exact metadata and move sequence match".into(),
                    };
                }

                // Same metadata + one is prefix of the other (relay disconnect / live correction)
                if ply_count > existing.ply_count {
                    let old_id = existing.id;
                    // Update existing record with the fuller game
                    existing.id = game.metadata.id;
                    existing.move_hash = move_hash;
                    existing.ply_count = ply_count;
                    return DuplicateVerdict::SupersedesPrevious {
                        old_id,
                        reason: "Complete game replaces earlier truncated relay transmission"
                            .into(),
                    };
                } else if ply_count < existing.ply_count {
                    return DuplicateVerdict::ConfirmedDuplicate {
                        canonical_id: existing.id,
                        reason: "Truncated game discarded in favor of existing longer transmission"
                            .into(),
                    };
                }
            }
        }

        // Check for matches by exact move hash (e.g. grandmaster short draws or book openings)
        if let Some(existing_games) = self.by_moves.get(&move_hash) {
            for existing in existing_games {
                // If moves are identical but players/events differ:
                if existing.metadata_key != meta_key {
                    // This is a legitimate transposition / identical game in different tournament
                    // Record in moves index and accept as unique!
                    let record = SeenGameRecord {
                        id: game.metadata.id,
                        move_hash,
                        ply_count,
                        metadata_key: meta_key.clone(),
                        has_full_date,
                        has_ratings,
                    };
                    self.by_metadata
                        .entry(meta_key)
                        .or_default()
                        .push(record.clone());
                    self.by_moves.entry(move_hash).or_default().push(record);
                    return DuplicateVerdict::TranspositionDifferentGame;
                }
            }
        }

        // Entirely unique
        let record = SeenGameRecord {
            id: game.metadata.id,
            move_hash,
            ply_count,
            metadata_key: meta_key.clone(),
            has_full_date,
            has_ratings,
        };

        self.by_metadata
            .entry(meta_key)
            .or_default()
            .push(record.clone());
        self.by_moves.entry(move_hash).or_default().push(record);

        DuplicateVerdict::Unique
    }
}

pub fn compute_move_hash(moves: &[CompactMove]) -> u64 {
    // 64-bit FNV-1a hash over compact 16-bit moves
    let mut hash: u64 = 0xcbf29ce484222325;
    for m in moves {
        let b1 = (m.0 & 0xFF) as u8;
        let b2 = ((m.0 >> 8) & 0xFF) as u8;
        hash ^= b1 as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        hash ^= b2 as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn normalize_player_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some((last, first)) = trimmed.split_once(',') {
        let last_clean: String = last
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        let first_initial = first
            .trim()
            .to_ascii_lowercase()
            .chars()
            .find(|c| c.is_alphabetic())
            .unwrap_or('_');
        format!("{},{}", last_clean, first_initial)
    } else {
        // Space separated e.g. "Magnus Carlsen"
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 {
            let last_clean: String = parts
                .last()
                .unwrap()
                .to_ascii_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            let first_initial = parts
                .first()
                .unwrap()
                .to_ascii_lowercase()
                .chars()
                .find(|c| c.is_alphabetic())
                .unwrap_or('_');
            format!("{},{}", last_clean, first_initial)
        } else {
            trimmed
                .to_ascii_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect()
        }
    }
}

pub fn normalize_metadata(game: &ParsedGame) -> NormalizedMetadataKey {
    let white_norm = normalize_player_name(&game.metadata.white);
    let black_norm = normalize_player_name(&game.metadata.black);

    let year = game.metadata.date.as_ref().and_then(|d| {
        let parts: Vec<&str> = d.split('.').collect();
        parts.first().and_then(|y| y.parse::<u16>().ok())
    });

    let event_norm = game
        .metadata
        .event
        .as_ref()
        .map(|e| e.trim().to_ascii_lowercase())
        .unwrap_or_default();

    let round = game.metadata.round.as_ref().map(|r| r.trim().to_string());

    NormalizedMetadataKey {
        white_norm,
        black_norm,
        year,
        event_norm,
        round,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codb_core::{Collection, GameMetadata, GameResult};

    fn make_test_game(
        id: u64,
        white: &str,
        black: &str,
        event: &str,
        year: &str,
        moves: Vec<CompactMove>,
    ) -> ParsedGame {
        ParsedGame {
            metadata: GameMetadata {
                id,
                collection: Collection::MastersOtb,
                white: white.into(),
                black: black.into(),
                white_elo: Some(2700),
                black_elo: Some(2680),
                date: Some(format!("{}.01.01", year)),
                event: Some(event.into()),
                site: Some("Test Site".into()),
                round: Some("1".into()),
                result: GameResult::Draw,
                eco: Some("C65".into()),
                time_control: Some("90+30".into()),
                source_id: None,
            },
            moves,
            fen_start: None,
        }
    }

    #[test]
    fn test_exact_duplicate_detection() {
        let mut dedupe = Deduplicator::new();
        let moves = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
        ];

        let g1 = make_test_game(
            1,
            "Carlsen, M",
            "Nakamura, H",
            "Tata Steel",
            "2023",
            moves.clone(),
        );
        let g2 = make_test_game(
            2,
            "Carlsen, Magnus",
            "Nakamura, Hikaru",
            "tata steel",
            "2023",
            moves,
        );

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        match dedupe.evaluate(&g2) {
            DuplicateVerdict::ConfirmedDuplicate { canonical_id, .. } => {
                assert_eq!(canonical_id, 1);
            }
            other => panic!("Expected ConfirmedDuplicate, got {:?}", other),
        }
    }

    #[test]
    fn test_transposition_in_different_events() {
        let mut dedupe = Deduplicator::new();
        let moves = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
        ];

        let g1 = make_test_game(
            1,
            "Player A",
            "Player B",
            "Event 2021",
            "2021",
            moves.clone(),
        );
        let g2 = make_test_game(2, "Player C", "Player D", "Event 2024", "2024", moves);

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g2),
            DuplicateVerdict::TranspositionDifferentGame
        );
    }

    #[test]
    fn test_relay_prefix_superseding() {
        let mut dedupe = Deduplicator::new();
        let short_moves = vec![CompactMove::new(12, 28, None)];
        let full_moves = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
            CompactMove::new(6, 21, None),
        ];

        let g_truncated = make_test_game(1, "Carlsen", "Nepo", "World Champ", "2021", short_moves);
        let g_complete = make_test_game(2, "Carlsen", "Nepo", "World Champ", "2021", full_moves);

        assert_eq!(dedupe.evaluate(&g_truncated), DuplicateVerdict::Unique);
        match dedupe.evaluate(&g_complete) {
            DuplicateVerdict::SupersedesPrevious { old_id, .. } => {
                assert_eq!(old_id, 1);
            }
            other => panic!("Expected SupersedesPrevious, got {:?}", other),
        }
    }
}
