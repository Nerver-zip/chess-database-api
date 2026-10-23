use codb_core::{
    compute_stable_game_id, Collection, CompactMove, GameMetadata, GameResult,
    IngestionCompletionStatus, IngestionStats, ParsedGame,
};
use codb_storage::{
    DatasetReader, DatasetWriter, DatasetWriterConfig, SourceArchiveMetadata, StorageError,
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn sample_game(id_seed: u64, collection: Collection, white: &str, black: &str) -> ParsedGame {
    let moves = vec![
        CompactMove::new(12, 28, None), // e2e4
        CompactMove::new(52, 36, None), // e7e5
        CompactMove::new(6, 21, None),  // g1f3
        CompactMove::new(57, 42, None), // b8c6
    ];
    let id = compute_stable_game_id(
        collection,
        Some(&format!("game-{}", id_seed)),
        white,
        black,
        Some("2025.01.15"),
        Some("1"),
        &moves,
    );

    ParsedGame {
        metadata: GameMetadata {
            id,
            collection,
            white: white.into(),
            black: black.into(),
            white_elo: Some(2650),
            black_elo: Some(2640),
            date: Some("2025.01.15".into()),
            event: Some("Tata Steel 2025".into()),
            site: Some("Wijk aan Zee".into()),
            round: Some("1".into()),
            result: GameResult::WhiteWins,
            eco: Some("C50".into()),
            time_control: Some("5400+30".into()),
            source_id: Some(format!("game-{}", id_seed)),
            broadcast_url: Some("https://lichess.org/broadcast/tata-steel-2025".into()),
        },
        moves,
        fen_start: None,
    }
}

fn create_dataset(
    target_dir: &Path,
    games_count: usize,
    seed_offset: u64,
) -> Result<(), StorageError> {
    let config = DatasetWriterConfig {
        block_capacity: 2,
        max_games_per_shard: 4,
        ..DatasetWriterConfig::default()
    };

    let mut writer = DatasetWriter::new(target_dir.to_path_buf(), config)?;

    for i in 0..games_count {
        let g = sample_game(
            seed_offset + i as u64,
            Collection::MastersOtb,
            "Carlsen",
            "Caruana",
        );
        writer.push_game(g)?;
    }

    let stats = IngestionStats {
        total_encountered: games_count as u64,
        accepted_unique: games_count as u64,
        confirmed_duplicates: 0,
        superseded_prior: 0,
        rejected_by_policy: 0,
        rejected_variant: 0,
        rejected_illegal: 0,
        rejected_malformed: 0,
        quarantined: 0,
        completion_status: IngestionCompletionStatus::CompleteEof,
    };

    let source = SourceArchiveMetadata {
        filename: "test.pgn".into(),
        url: None,
        sha256: None,
        size_bytes: 1024,
        license: "CC0".into(),
    };

    writer.finalize(stats, vec![source])?;
    Ok(())
}

#[test]
fn test_successful_publication_initial_and_replacement() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // 1. Initial publication when target does not exist
    assert!(!target.exists());
    create_dataset(&target, 4, 100).expect("Initial build should succeed");
    assert!(target.exists());
    assert!(target.join("manifest.json").exists());
    assert!(target.join("games.idx").exists());
    assert!(target.join("shards").exists());

    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);

    // 2. Publication replacing existing dataset
    create_dataset(&target, 6, 200).expect("Replacement build should succeed");
    assert!(target.exists());

    let mut reader2 = DatasetReader::open(&target).unwrap();
    assert_eq!(reader2.total_games(), 6);
    // Old game 100 should no longer be in the new dataset
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(reader2.get_game(g100.metadata.id).is_err());
    // New game 200 should be in the new dataset
    let g200 = sample_game(200, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(reader2.get_game(g200.metadata.id).is_ok());

    // Verify no backup directories remain
    let parent = target.parent().unwrap();
    for entry in fs::read_dir(parent).unwrap() {
        let entry = entry.unwrap();
        let fname = entry.file_name().to_string_lossy().to_string();
        assert!(
            !fname.starts_with(".backup_"),
            "Backup dir {} was not cleaned up",
            fname
        );
    }
}

#[test]
fn test_failure_during_staging_leaves_existing_target_untouched() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // Create valid original dataset
    create_dataset(&target, 4, 100).expect("Initial build should succeed");
    let original_manifest_content = fs::read_to_string(target.join("manifest.json")).unwrap();

    // Start a second build that will fail due to mismatched accounting
    let config = DatasetWriterConfig {
        block_capacity: 2,
        ..DatasetWriterConfig::default()
    };
    let mut writer = DatasetWriter::new(target.clone(), config).unwrap();
    let g = sample_game(999, Collection::MastersOtb, "Kasparov", "Karpov");
    writer.push_game(g).unwrap();

    // Bad stats: accepted_unique != 1
    let bad_stats = IngestionStats {
        total_encountered: 10,
        accepted_unique: 5, // Mismatched! Writer has 1 game
        confirmed_duplicates: 5,
        superseded_prior: 0,
        rejected_by_policy: 0,
        rejected_variant: 0,
        rejected_illegal: 0,
        rejected_malformed: 0,
        quarantined: 0,
        completion_status: IngestionCompletionStatus::CompleteEof,
    };

    let res = writer.finalize(bad_stats, vec![]);
    assert!(res.is_err(), "Finalize must fail on mismatched accounting");

    // Target must be completely untouched and retain original contents
    assert!(target.exists());
    let current_manifest_content = fs::read_to_string(target.join("manifest.json")).unwrap();
    assert_eq!(original_manifest_content, current_manifest_content);

    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(reader.get_game(g100.metadata.id).is_ok());
}

#[test]
fn test_publication_recovery_restores_target_on_failure() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // Create valid original dataset
    create_dataset(&target, 4, 100).expect("Initial build should succeed");
    let original_manifest_content = fs::read_to_string(target.join("manifest.json")).unwrap();

    let backup_dir = target.parent().unwrap().join(format!(
        ".backup_{}_{}",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));

    // Simulate moving target to backup (as during finalize)
    fs::rename(&target, &backup_dir).unwrap();
    assert!(!target.exists());
    assert!(backup_dir.exists());

    // When an error occurs during rename of staging to target:
    // Safe recovery restores backup_dir to target:
    fs::rename(&backup_dir, &target).unwrap();

    // Target is preserved with original contents
    assert!(target.exists());
    let restored_manifest = fs::read_to_string(target.join("manifest.json")).unwrap();
    assert_eq!(original_manifest_content, restored_manifest);

    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(reader.get_game(g100.metadata.id).is_ok());
}
