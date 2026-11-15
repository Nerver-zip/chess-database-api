use codb_core::{
    compute_stable_game_id, Collection, CompactMove, GameMetadata, GameResult,
    IngestionCompletionStatus, IngestionStats, ParsedGame,
};
use codb_storage::{
    verify_dataset, CurationPolicyMetadata, DatasetReader, DatasetWriter, DatasetWriterConfig,
    GameLookupReader, GameLookupWriter, ShardReader, ShardWriter, SourceArchiveMetadata,
    StorageError,
};
use std::fs::File;
use std::io::Write;
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
            event: Some("Tata Steel Masters 2025".into()),
            site: Some("Wijk aan Zee NED".into()),
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

#[test]
fn test_shard_writer_reader_round_trip() {
    let tmp = tempdir().unwrap();
    let shard_path = tmp.path().join("shard_0000.cgb3");

    let mut writer = ShardWriter::create(shard_path.clone(), 0, Collection::MastersOtb).unwrap();
    assert_eq!(writer.shard_id(), 0);

    // Create 2 blocks with 10 games each
    for b in 0..2 {
        let mut block = codb_storage::GameBlock::new(Collection::MastersOtb);
        for g in 0..10 {
            let game = sample_game(
                (b * 10 + g) as u64,
                Collection::MastersOtb,
                &format!("White {}", g),
                &format!("Black {}", g),
            );
            block.games.push(game);
        }
        let payload = block.compress(3).unwrap();
        let first_id = block.games[0].metadata.id;
        let b_idx = writer
            .append_block(&payload, 10, first_id, (10 * 150) as u32)
            .unwrap();
        assert_eq!(b_idx, b as u32);
    }

    assert_eq!(writer.total_games(), 20);
    assert_eq!(writer.block_count(), 2);
    let (finished_path, descriptors, total_size) = writer.finish().unwrap();
    assert_eq!(finished_path, shard_path);
    assert_eq!(descriptors.len(), 2);
    assert!(total_size > 0);

    // Read back shard
    let mut reader = ShardReader::open(&shard_path).unwrap();
    assert_eq!(reader.shard_id(), 0);
    assert_eq!(reader.collection(), Collection::MastersOtb);
    assert_eq!(reader.total_games(), 20);
    assert_eq!(reader.block_count(), 2);

    let b0_games = reader.decompress_block(0).unwrap();
    assert_eq!(b0_games.len(), 10);
    assert_eq!(b0_games[0].metadata.white, "White 0");

    let b1_games = reader.decompress_block(1).unwrap();
    assert_eq!(b1_games.len(), 10);
    assert_eq!(b1_games[9].metadata.white, "White 9");

    let single_game = reader.get_game(1, 5).unwrap();
    assert_eq!(single_game.metadata.white, "White 5");
}

#[test]
fn test_lookup_writer_reader_binary_search() {
    let tmp = tempdir().unwrap();
    let lookup_path = tmp.path().join("games.idx");

    let mut writer = GameLookupWriter::new(lookup_path.clone(), Collection::MastersOtb);
    writer.add_entry(500, codb_core::GameLocation::new(0, 1, 3));
    writer.add_entry(100, codb_core::GameLocation::new(0, 0, 0));
    writer.add_entry(300, codb_core::GameLocation::new(0, 0, 5));
    writer.add_entry(700, codb_core::GameLocation::new(1, 0, 2));

    // Collision case: identical game_id with different location
    writer.add_entry(300, codb_core::GameLocation::new(0, 0, 6));

    let (path, size, sha) = writer.finish().unwrap();
    assert_eq!(path, lookup_path);
    assert!(size > 0);
    assert_eq!(sha.len(), 64);

    let reader = GameLookupReader::open(&lookup_path).unwrap();
    assert_eq!(reader.entry_count(), 5);
    assert!(reader.verify_checksum().unwrap());

    // Binary search queries
    let res100 = reader.find_locations(100);
    assert_eq!(res100.len(), 1);
    assert_eq!(res100[0].shard_id, 0);
    assert_eq!(res100[0].game_index_in_block, 0);

    let res300 = reader.find_locations(300);
    assert_eq!(res300.len(), 2);

    let res_missing = reader.find_locations(999999);
    assert!(res_missing.is_empty());
}

#[test]
fn test_dataset_writer_reader_multi_block_multi_shard_and_retrieval() {
    let tmp = tempdir().unwrap();
    let target_dir = tmp.path().join("masters_dataset");

    // Set small block capacity (5 games) and small max games per shard (10 games)
    // to force multiple blocks and multiple shards
    let config = DatasetWriterConfig {
        collection: Collection::MastersOtb,
        block_capacity: 5,
        max_games_per_shard: 10,
        zstd_level: 3,
        curation_policy: CurationPolicyMetadata {
            min_rating_both: 2200,
            min_rating_one: 2400,
            min_rating_other: 2000,
            min_plies: 4,
            extra_criteria: "None".into(),
        },
        deduplication_policy_version: "v2".into(),
    };

    let mut writer = DatasetWriter::new(target_dir.clone(), config).unwrap();
    let mut game_ids = Vec::new();

    // Ingest 25 games -> 5 blocks across 3 shards
    for i in 0..25 {
        let game = sample_game(
            i,
            Collection::MastersOtb,
            &format!("Magnus {}", i),
            &format!("Hikaru {}", i),
        );
        game_ids.push(game.metadata.id);
        writer.push_game(game).unwrap();
    }

    let stats = IngestionStats {
        total_encountered: 25,
        accepted_unique: 25,
        confirmed_duplicates: 0,
        superseded_prior: 0,
        rejected_by_policy: 0,
        rejected_variant: 0,
        rejected_illegal: 0,
        rejected_malformed: 0,
        quarantined: 0,
        completion_status: IngestionCompletionStatus::CompleteEof,
    };

    let sources = vec![SourceArchiveMetadata {
        filename: "test_masters.pgn".into(),
        url: None,
        sha256: None,
        size_bytes: 5000,
        license: "CC0".into(),
        checksum_verified: None,
    }];

    let manifest = writer.finalize(stats, sources).unwrap();
    assert_eq!(manifest.total_games, 25);
    assert_eq!(manifest.shards.len(), 3); // 10 + 10 + 5 games = 3 shards
    assert!(target_dir.join("manifest.json").exists());
    assert!(target_dir.join("games.idx").exists());

    // Open dataset and retrieve all 25 games by ID
    let mut reader = DatasetReader::open(&target_dir).unwrap();
    assert_eq!(reader.total_games(), 25);
    assert_eq!(reader.collection(), Collection::MastersOtb);

    for (i, &id) in game_ids.iter().enumerate() {
        let game = reader.get_game(id).unwrap();
        assert_eq!(game.metadata.id, id);
        assert_eq!(game.metadata.white, format!("Magnus {}", i));

        // Test PGN export round-trip
        let pgn = reader.get_game_pgn(id).unwrap();
        assert!(pgn.contains(&format!("Magnus {}", i)));
        assert!(pgn.contains("1. e4 e5 2. Nf3 Nc6"));
    }

    // Verify missing game returns GameNotFound
    match reader.get_game(999999999) {
        Err(StorageError::GameNotFound(missing_id)) => assert_eq!(missing_id, 999999999),
        other => panic!("Expected GameNotFound, got {:?}", other),
    }

    // Run verification tool
    let report = verify_dataset(&target_dir, true).unwrap();
    assert_eq!(report.status, "PASS");
    assert!(report.checksums_valid);
    assert!(report.accounting_reconciled);
    assert!(report.deep_verification_passed);
}

#[test]
fn test_dataset_supersession_compaction() {
    let tmp = tempdir().unwrap();
    let target_dir = tmp.path().join("superseded_dataset");

    let config = DatasetWriterConfig {
        collection: Collection::MastersOtb,
        block_capacity: 4,
        max_games_per_shard: 8,
        zstd_level: 3,
        curation_policy: CurationPolicyMetadata {
            min_rating_both: 2200,
            min_rating_one: 2400,
            min_rating_other: 2000,
            min_plies: 4,
            extra_criteria: "None".into(),
        },
        deduplication_policy_version: "v2".into(),
    };

    let mut writer = DatasetWriter::new(target_dir.clone(), config).unwrap();

    // Ingest 8 games (fills Shard 0 completely)
    let mut initial_games = Vec::new();
    for i in 0..8 {
        let game = sample_game(
            i,
            Collection::MastersOtb,
            &format!("White {}", i),
            &format!("Black {}", i),
        );
        initial_games.push(game.clone());
        writer.push_game(game).unwrap();
    }

    // Game 2 (which is in Shard 0, Block 0) gets superseded by a longer transmission
    let old_id = initial_games[2].metadata.id;
    let mut extended_game = initial_games[2].clone();
    extended_game.moves.push(CompactMove::new(25, 41, None)); // extra ply
    let new_id = compute_stable_game_id(
        Collection::MastersOtb,
        extended_game.metadata.source_id.as_deref(),
        &extended_game.metadata.white,
        &extended_game.metadata.black,
        extended_game.metadata.date.as_deref(),
        extended_game.metadata.round.as_deref(),
        &extended_game.moves,
    );
    extended_game.metadata.id = new_id;

    // Handle superseding
    writer
        .handle_superseded_game(old_id, extended_game)
        .unwrap();

    let stats = IngestionStats {
        total_encountered: 9,
        accepted_unique: 8,
        confirmed_duplicates: 0,
        superseded_prior: 1,
        rejected_by_policy: 0,
        rejected_variant: 0,
        rejected_illegal: 0,
        rejected_malformed: 0,
        quarantined: 0,
        completion_status: IngestionCompletionStatus::CompleteEof,
    };

    let manifest = writer.finalize(stats, vec![]).unwrap();
    assert_eq!(manifest.total_games, 8);

    // Verify finalized dataset: old_id must NOT be found, new_id must be found!
    let mut reader = DatasetReader::open(&target_dir).unwrap();
    assert!(matches!(
        reader.get_game(old_id),
        Err(StorageError::GameNotFound(_))
    ));

    let retrieved_new = reader.get_game(new_id).unwrap();
    assert_eq!(retrieved_new.metadata.id, new_id);
    assert_eq!(retrieved_new.moves.len(), 5); // has extended ply

    // Verification check passes 100%
    let report = verify_dataset(&target_dir, true).unwrap();
    assert_eq!(report.status, "PASS");
}

#[test]
fn test_missing_lookup_file_error_names_path_and_preserves_io_kind() {
    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("games.idx");
    let error = match GameLookupReader::open(&missing) {
        Err(error) => error,
        Ok(_) => panic!("missing lookup file unexpectedly opened"),
    };

    assert!(matches!(error, StorageError::Io(_)));
    assert!(error
        .to_string()
        .contains(missing.to_string_lossy().as_ref()));
}

#[test]
fn test_corruption_safety_and_invalid_magic() {
    let tmp = tempdir().unwrap();
    let corrupt_shard = tmp.path().join("corrupt.cgb3");

    // Write bogus bytes
    let mut f = File::create(&corrupt_shard).unwrap();
    f.write_all(b"BOGUS_CONTAINER_BYTES_HEADER").unwrap();

    // ShardReader::open must fail safely without panicking
    let res = ShardReader::open(&corrupt_shard);
    assert!(res.is_err());

    // Truncated lookup table
    let corrupt_lookup = tmp.path().join("corrupt.idx");
    let mut fl = File::create(&corrupt_lookup).unwrap();
    fl.write_all(b"CGL1").unwrap();
    let res_lookup = GameLookupReader::open(&corrupt_lookup);
    assert!(res_lookup.is_err());
}
