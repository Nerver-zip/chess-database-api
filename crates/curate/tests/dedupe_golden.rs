use opening_explorer_core::{Collection, CompactMove, GameMetadata, GameResult, ParsedGame};
use opening_explorer_curate::dedupe::{DedupeError, Deduplicator, DuplicateVerdict};
use serde::Deserialize;
use std::{collections::HashMap, fs, path::PathBuf};

#[derive(Deserialize)]
struct GoldenFile {
    schema_version: u32,
    cases: Vec<GoldenCase>,
}

#[derive(Deserialize)]
struct GoldenCase {
    case_id: String,
    records: Vec<GoldenRecord>,
}

#[derive(Deserialize)]
struct GoldenRecord {
    stable_id_ref: String,
    white: String,
    black: String,
    date: String,
    event: String,
    round: String,
    moves: Vec<[u8; 2]>,
    expected_verdict: String,
}

#[test]
fn gigabase_dedupe_golden_cases_match_disk_backed_policy() {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmarks/gigabase/DEDUPE_GOLDEN.json");
    let bytes = fs::read(&path).expect("dedupe golden fixture should be readable");
    let fixture: GoldenFile =
        serde_json::from_slice(&bytes).expect("dedupe golden fixture should be valid JSON");
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.cases.len(), 6);

    for case in fixture.cases {
        let mut dedupe = Deduplicator::new_temp_disk().expect("temporary disk dedupe store");
        let mut ids = HashMap::<String, u64>::new();

        for record in case.records {
            let next_id = ids.len() as u64 + 1;
            let id = *ids.entry(record.stable_id_ref).or_insert(next_id);
            let game = ParsedGame {
                metadata: GameMetadata {
                    id,
                    collection: Collection::MastersOtb,
                    white: record.white,
                    black: record.black,
                    white_elo: Some(2500),
                    black_elo: Some(2480),
                    date: Some(record.date),
                    event: Some(record.event),
                    site: None,
                    round: Some(record.round),
                    result: GameResult::Draw,
                    eco: None,
                    time_control: None,
                    source_id: None,
                    broadcast_url: None,
                    source_attribution: None,
                },
                moves: record
                    .moves
                    .into_iter()
                    .map(|[from, to]| CompactMove::new(from, to, None))
                    .collect(),
                fen_start: None,
            };

            let actual = dedupe.evaluate(&game);
            match record.expected_verdict.as_str() {
                "Unique" => assert_eq!(
                    actual.unwrap(),
                    DuplicateVerdict::Unique,
                    "{}",
                    case.case_id
                ),
                "ConfirmedDuplicate" => assert!(
                    matches!(&actual, Ok(DuplicateVerdict::ConfirmedDuplicate { .. })),
                    "{}: expected confirmed duplicate, got {actual:?}",
                    case.case_id
                ),
                "SupersedesPrevious" => assert!(
                    matches!(&actual, Ok(DuplicateVerdict::SupersedesPrevious { .. })),
                    "{}: expected supersession, got {actual:?}",
                    case.case_id
                ),
                "TranspositionDifferentGame" => assert_eq!(
                    actual.unwrap(),
                    DuplicateVerdict::TranspositionDifferentGame,
                    "{}",
                    case.case_id
                ),
                "QuarantinedStableIdCollision" => assert!(
                    matches!(&actual, Err(DedupeError::IdCollision { .. })),
                    "{}: expected stable-ID collision, got {actual:?}",
                    case.case_id
                ),
                other => panic!("{}: unknown expected verdict {other}", case.case_id),
            }
        }
    }
}
