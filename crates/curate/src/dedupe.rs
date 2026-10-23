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
    pub moves: Vec<CompactMove>,
    pub metadata_key: NormalizedMetadataKey,
    pub has_full_date: bool,
    pub has_ratings: bool,
}

pub struct Deduplicator {
    /// Maps normalized metadata key -> list of seen games with this metadata
    by_metadata: HashMap<NormalizedMetadataKey, Vec<SeenGameRecord>>,
    /// Maps move sequence hash -> list of seen games with this move hash
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

        // 1. Check for duplicate or prefix relationships within the same metadata
        if let Some(existing_games) = self.by_metadata.get_mut(&meta_key) {
            for existing in existing_games.iter_mut() {
                // Exact move match (verified by actual move sequence, not hash alone)
                if existing.moves == game.moves {
                    return DuplicateVerdict::ConfirmedDuplicate {
                        canonical_id: existing.id,
                        reason: "Exact metadata and verified move sequence match".into(),
                    };
                }

                // Check for verified prefix relationship
                if ply_count > existing.moves.len() {
                    // Check if existing shorter game is a strict prefix of candidate game
                    if existing.moves == game.moves[..existing.moves.len()] {
                        let old_id = existing.id;
                        let old_hash = existing.move_hash;

                        // Update metadata record to point to the new fuller game
                        existing.id = game.metadata.id;
                        existing.move_hash = move_hash;
                        existing.moves = game.moves.clone();
                        existing.has_full_date = has_full_date;
                        existing.has_ratings = has_ratings;

                        // Ensure index consistency in by_moves: remove old reference, add new
                        if let Some(list) = self.by_moves.get_mut(&old_hash) {
                            list.retain(|r| r.id != old_id);
                        }
                        self.by_moves
                            .entry(move_hash)
                            .or_default()
                            .push(existing.clone());

                        return DuplicateVerdict::SupersedesPrevious {
                            old_id,
                            reason: "Verified prefix relationship: complete game replaces earlier truncated relay transmission"
                                .into(),
                        };
                    }
                    // Moves diverge: even with matching metadata, DIFFERENT MOVES = DISTINCT GAME!
                } else if ply_count < existing.moves.len() {
                    // Check if candidate game is a strict prefix of existing longer game
                    if game.moves == existing.moves[..ply_count] {
                        return DuplicateVerdict::ConfirmedDuplicate {
                            canonical_id: existing.id,
                            reason: "Verified prefix relationship: truncated candidate discarded in favor of existing longer transmission"
                                .into(),
                        };
                    }
                    // Moves diverge: distinct game!
                }
            }
        }

        // 2. Check for matches by move hash across all games (transpositions & book openings)
        if let Some(existing_games) = self.by_moves.get(&move_hash) {
            for existing in existing_games {
                // Verify move equality to protect against hash collisions
                if existing.moves == game.moves && existing.metadata_key != meta_key {
                    // Legitimate transposition: same moves, different players/events
                    let record = SeenGameRecord {
                        id: game.metadata.id,
                        move_hash,
                        moves: game.moves.clone(),
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

        // 3. Entirely unique game
        let record = SeenGameRecord {
            id: game.metadata.id,
            move_hash,
            moves: game.moves.clone(),
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
        round: Option<&str>,
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
                round: round.map(|r| r.into()),
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
            Some("1"),
            moves.clone(),
        );
        let g2 = make_test_game(
            2,
            "Carlsen, Magnus",
            "Nakamura, Hikaru",
            "tata steel",
            "2023",
            Some("1"),
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
            Some("1"),
            moves.clone(),
        );
        let g2 = make_test_game(
            2,
            "Player C",
            "Player D",
            "Event 2024",
            "2024",
            Some("1"),
            moves,
        );

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(
            dedupe.evaluate(&g2),
            DuplicateVerdict::TranspositionDifferentGame
        );
    }

    #[test]
    fn test_verified_prefix_superseding() {
        let mut dedupe = Deduplicator::new();
        let short_moves = vec![CompactMove::new(12, 28, None)];
        let full_moves = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
            CompactMove::new(6, 21, None),
        ];

        let g_truncated = make_test_game(
            1,
            "Carlsen",
            "Nepo",
            "World Champ",
            "2021",
            Some("6"),
            short_moves,
        );
        let g_complete = make_test_game(
            2,
            "Carlsen",
            "Nepo",
            "World Champ",
            "2021",
            Some("6"),
            full_moves,
        );

        assert_eq!(dedupe.evaluate(&g_truncated), DuplicateVerdict::Unique);
        match dedupe.evaluate(&g_complete) {
            DuplicateVerdict::SupersedesPrevious { old_id, .. } => {
                assert_eq!(old_id, 1);
            }
            other => panic!("Expected SupersedesPrevious, got {:?}", other),
        }
    }

    #[test]
    fn test_divergent_moves_do_not_supersede() {
        // Critical regression test for Finding 1.1:
        // Same players, same event, same year, but DIFFERENT move sequences!
        // E.g. Blitz playoff game 1 vs game 2.
        let mut dedupe = Deduplicator::new();
        let g1_moves = vec![
            CompactMove::new(12, 28, None), // 1. e4
            CompactMove::new(52, 36, None), // 1... e5
        ];
        let g2_moves = vec![
            CompactMove::new(11, 27, None), // 1. d4 (different opening!)
            CompactMove::new(51, 35, None), // 1... d5
            CompactMove::new(10, 26, None), // 2. c4
        ];

        let g1 = make_test_game(
            1,
            "Carlsen, M",
            "Nakamura, H",
            "Speed Chess",
            "2023",
            None,
            g1_moves,
        );
        let g2 = make_test_game(
            2,
            "Carlsen, M",
            "Nakamura, H",
            "Speed Chess",
            "2023",
            None,
            g2_moves,
        );

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        // G2 must NOT supersede G1! G2 is a distinct unique game!
        assert_eq!(dedupe.evaluate(&g2), DuplicateVerdict::Unique);
    }

    #[test]
    fn test_hash_collision_protection() {
        // Regression test for Finding 1.3:
        // Even if move hash were forced to match, different actual moves must NOT be treated as duplicates!
        let mut dedupe = Deduplicator::new();
        let moves_a = vec![CompactMove::new(12, 28, None)];
        let moves_b = vec![CompactMove::new(11, 27, None)];

        let g1 = make_test_game(
            1,
            "Player A",
            "Player B",
            "Event",
            "2024",
            Some("1"),
            moves_a,
        );
        let g2 = make_test_game(
            2,
            "Player A",
            "Player B",
            "Event",
            "2024",
            Some("1"),
            moves_b,
        );

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        assert_eq!(dedupe.evaluate(&g2), DuplicateVerdict::Unique);
    }

    #[test]
    fn test_multi_game_superseding_chain() {
        // 3 transmissions in a chain:
        // Part 1 (ply 2) -> Part 2 (ply 4) -> Complete (ply 6)
        let mut dedupe = Deduplicator::new();
        let p1 = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
        ];
        let p2 = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
            CompactMove::new(6, 21, None),
            CompactMove::new(57, 42, None),
        ];
        let p3 = vec![
            CompactMove::new(12, 28, None),
            CompactMove::new(52, 36, None),
            CompactMove::new(6, 21, None),
            CompactMove::new(57, 42, None),
            CompactMove::new(5, 26, None),
            CompactMove::new(61, 34, None),
        ];

        let g1 = make_test_game(1, "Fischer", "Spassky", "Reykjavik", "1972", Some("6"), p1);
        let g2 = make_test_game(2, "Fischer", "Spassky", "Reykjavik", "1972", Some("6"), p2);
        let g3 = make_test_game(3, "Fischer", "Spassky", "Reykjavik", "1972", Some("6"), p3);

        assert_eq!(dedupe.evaluate(&g1), DuplicateVerdict::Unique);
        match dedupe.evaluate(&g2) {
            DuplicateVerdict::SupersedesPrevious { old_id, .. } => assert_eq!(old_id, 1),
            other => panic!("Expected SupersedesPrevious, got {:?}", other),
        }
        match dedupe.evaluate(&g3) {
            DuplicateVerdict::SupersedesPrevious { old_id, .. } => assert_eq!(old_id, 2),
            other => panic!("Expected SupersedesPrevious, got {:?}", other),
        }
    }
}
