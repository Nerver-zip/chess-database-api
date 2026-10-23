use codb_core::{Collection, IngestionCompletionStatus};
use codb_curate::dedupe::Deduplicator;
use codb_ingest::pipeline::{CuratedPipeline, MemoryGameConsumer};
use std::io::Cursor;

const PGN_FILE_1: &str = r#"[Event "Broadcast 1"]
[Site "https://lichess.org/broadcast/tourney-1/round-1/12345"]
[Date "2024.01.10"]
[Round "1"]
[White "Carlsen, Magnus"]
[Black "Nakamura, Hikaru"]
[Result "1-0"]
[WhiteElo "2830"]
[BlackElo "2780"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Broadcast 1"]
[Site "https://lichess.org/broadcast/tourney-1/round-1/12346"]
[Date "2024.01.10"]
[Round "1"]
[White "Caruana, Fabiano"]
[Black "So, Wesley"]
[Result "1/2-1/2"]
[WhiteElo "2790"]
[BlackElo "2750"]

1. d4 Nf6 2. c4 e6 1/2-1/2
"#;

// Contains duplicate of Carlsen-Nakamura, and a superseding extended version of Caruana-So
const PGN_FILE_2: &str = r#"[Event "Broadcast 1"]
[Site "https://lichess.org/broadcast/tourney-1/round-1/12345"]
[Date "2024.01.10"]
[Round "1"]
[White "Carlsen, M."]
[Black "Nakamura, H."]
[Result "1-0"]
[WhiteElo "2830"]
[BlackElo "2780"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Broadcast 1"]
[Site "https://lichess.org/broadcast/tourney-1/round-1/12346"]
[Date "2024.01.10"]
[Round "1"]
[White "Caruana, Fabiano"]
[Black "So, Wesley"]
[Result "1/2-1/2"]
[WhiteElo "2790"]
[BlackElo "2750"]

1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 1/2-1/2
"#;

#[test]
fn test_curated_pipeline_cross_file_deduplication_and_supersession() {
    let deduplicator = Deduplicator::new();
    let consumer = MemoryGameConsumer::new();
    let mut pipeline = CuratedPipeline::new(Collection::MastersOtb, deduplicator, consumer);

    // Process File 1
    pipeline
        .process_stream(Cursor::new(PGN_FILE_1))
        .expect("Stream 1 must process successfully");
    assert_eq!(pipeline.stats.total_encountered, 2);
    assert_eq!(pipeline.stats.accepted_unique, 2);
    assert_eq!(pipeline.consumer.games.len(), 2);

    // Process File 2
    pipeline
        .process_stream(Cursor::new(PGN_FILE_2))
        .expect("Stream 2 must process successfully");
    assert_eq!(pipeline.stats.total_encountered, 4);
    assert_eq!(pipeline.stats.accepted_unique, 2);
    assert_eq!(pipeline.stats.confirmed_duplicates, 1);
    assert_eq!(pipeline.stats.superseded_prior, 1);

    // Exactly 2 retained canonical games
    assert_eq!(pipeline.consumer.games.len(), 2);
    assert!(pipeline.stats.is_reconciled());
    assert!(pipeline.stats.verify_retained_count(2));

    // Caruana-So must be the extended version (4 moves / 8 plies)
    let caruana_game = pipeline
        .consumer
        .games
        .iter()
        .find(|g| g.metadata.white.contains("Caruana"))
        .unwrap();
    assert_eq!(caruana_game.moves.len(), 6); // 1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 (6 plies)
}

#[test]
fn test_curated_pipeline_max_games_limit() {
    let deduplicator = Deduplicator::new();
    let consumer = MemoryGameConsumer::new();
    let mut pipeline = CuratedPipeline::new(Collection::MastersOtb, deduplicator, consumer)
        .with_max_games(Some(1));

    pipeline
        .process_stream(Cursor::new(PGN_FILE_1))
        .expect("Stream must stop cleanly at limit");

    assert_eq!(pipeline.stats.accepted_unique, 1);
    assert_eq!(pipeline.consumer.games.len(), 1);
    assert_eq!(
        pipeline.stats.completion_status,
        IngestionCompletionStatus::PartialLimitReached
    );
}
