use chess_database_core::{
    compute_stable_game_id, Collection, CompactMove, GameLocation, GameMetadata, GameResult,
    IngestionCompletionStatus, IngestionStats, ParsedGame,
};
use chess_database_storage::{
    compute_file_sha256, verify_dataset, DatasetWriter, DatasetWriterConfig, GameLookupEntry,
    GameLookupReader, GameLookupWriter, SourceArchiveMetadata, StagingCatalog, StorageError,
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
            source_attribution: None,
        },
        moves,
        fen_start: None,
    }
}

fn build_test_dataset(target_dir: &Path, count: usize) -> Result<(), StorageError> {
    let config = DatasetWriterConfig {
        block_capacity: 2,
        max_games_per_shard: 4,
        ..DatasetWriterConfig::default()
    };

    let mut writer = DatasetWriter::new(target_dir.to_path_buf(), config)?;

    for i in 0..count {
        let g = sample_game(
            i as u64,
            Collection::MastersOtb,
            &format!("W{}", i),
            &format!("B{}", i),
        );
        writer.push_game(g)?;
    }

    let stats = IngestionStats {
        total_encountered: count as u64,
        accepted_unique: count as u64,
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
        checksum_verified: None,
    };

    writer.finalize(stats, vec![source])?;
    Ok(())
}

#[test]
fn test_deep_verification_passes_on_valid_dataset() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset");
    build_test_dataset(&target, 8).unwrap();

    let rep = verify_dataset(&target, true).unwrap();
    assert!(rep.checksums_valid);
    assert!(rep.accounting_reconciled);
    assert!(rep.deep_verification_passed);
}

#[test]
fn test_deep_verification_detects_swapped_game_ids_with_valid_checksum() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset");
    build_test_dataset(&target, 4).unwrap();

    // Read lookup entries
    let lookup_path = target.join("games.idx");
    let reader = chess_database_storage::GameLookupReader::open(&lookup_path).unwrap();
    let e0 = reader.get_entry(0).unwrap();
    let e1 = reader.get_entry(1).unwrap();
    let e2 = reader.get_entry(2).unwrap();
    let e3 = reader.get_entry(3).unwrap();
    drop(reader);

    // Reconstruct games.idx with swapped locations:
    // e0 points to e1's location, e1 points to e0's location
    // Since we re-write the lookup file using GameLookupWriter, its internal checksum and header will be valid!
    let mut entries = vec![
        GameLookupEntry::new(
            e0.game_id,
            e1.shard_id,
            e1.block_index,
            e1.game_index_in_block,
        ),
        GameLookupEntry::new(
            e1.game_id,
            e0.shard_id,
            e0.block_index,
            e0.game_index_in_block,
        ),
        e2,
        e3,
    ];
    entries.sort_unstable();

    let mut lw = GameLookupWriter::new(lookup_path.clone(), Collection::MastersOtb);
    for e in entries {
        lw.add_entry(
            e.game_id,
            GameLocation::new(e.shard_id, e.block_index, e.game_index_in_block),
        );
    }
    lw.finish().unwrap();

    // Also update manifest.json with the new lookup checksum so quick verification passes
    let manifest_path = target.join("manifest.json");
    let manifest_str = fs::read_to_string(&manifest_path).unwrap();
    let mut manifest: chess_database_storage::DatasetManifest =
        serde_json::from_str(&manifest_str).unwrap();
    manifest.lookup_table.sha256 = compute_file_sha256(&lookup_path).unwrap();
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    // Quick verification passes file checksums
    let quick_res = verify_dataset(&target, false);
    assert!(
        quick_res.is_ok(),
        "Quick verification should pass because file checksums match"
    );

    // Deep verification MUST catch that games point to wrong physical locations
    let deep_res = verify_dataset(&target, true);
    assert!(
        deep_res.is_err(),
        "Deep verification must fail when lookup locations are swapped"
    );
    let err_msg = format!("{}", deep_res.unwrap_err());
    assert!(
        err_msg.contains("Deep verification failed") || err_msg.contains("points elsewhere"),
        "Expected location mismatch error, got: {}",
        err_msg
    );
}

#[test]
fn test_deep_verification_detects_corrupted_lookup_pointing_to_nonexistent_block() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset");
    build_test_dataset(&target, 4).unwrap();

    let lookup_path = target.join("games.idx");
    let reader = chess_database_storage::GameLookupReader::open(&lookup_path).unwrap();
    let e0 = reader.get_entry(0).unwrap();
    let e1 = reader.get_entry(1).unwrap();
    let e2 = reader.get_entry(2).unwrap();
    let e3 = reader.get_entry(3).unwrap();
    drop(reader);

    // Make entry 0 point to nonexistent block 9999
    let entries = vec![
        GameLookupEntry::new(e0.game_id, e0.shard_id, 9999, e0.game_index_in_block),
        e1,
        e2,
        e3,
    ];

    let mut lw = GameLookupWriter::new(lookup_path.clone(), Collection::MastersOtb);
    for e in entries {
        lw.add_entry(
            e.game_id,
            GameLocation::new(e.shard_id, e.block_index, e.game_index_in_block),
        );
    }
    lw.finish().unwrap();

    // Update manifest with new lookup checksum
    let manifest_path = target.join("manifest.json");
    let manifest_str = fs::read_to_string(&manifest_path).unwrap();
    let mut manifest: chess_database_storage::DatasetManifest =
        serde_json::from_str(&manifest_str).unwrap();
    manifest.lookup_table.sha256 = compute_file_sha256(&lookup_path).unwrap();
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let deep_res = verify_dataset(&target, true);
    assert!(deep_res.is_err());
    let err_msg = format!("{}", deep_res.unwrap_err());
    assert!(
        err_msg.contains("points to block") || err_msg.contains("block count"),
        "Expected invalid block index error, got: {}",
        err_msg
    );
}

#[test]
fn test_deep_verification_detects_count_mismatch_between_manifest_and_lookup() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset");
    build_test_dataset(&target, 4).unwrap();

    // Tamper with manifest total_games
    let manifest_path = target.join("manifest.json");
    let manifest_str = fs::read_to_string(&manifest_path).unwrap();
    let mut manifest: chess_database_storage::DatasetManifest =
        serde_json::from_str(&manifest_str).unwrap();
    manifest.total_games = 5;
    manifest.accounting.total_encountered = 5;
    manifest.accounting.accepted_unique = 5;
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let deep_res = verify_dataset(&target, true);
    assert!(deep_res.is_err());
    let err_msg = format!("{}", deep_res.unwrap_err());
    assert!(
        err_msg.contains("Lookup entry count") || err_msg.contains("does not match manifest"),
        "Expected entry count mismatch error, got: {}",
        err_msg
    );
}

#[test]
fn test_staging_catalog_query_plans_have_zero_temp_btree() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("catalog.sqlite");
    let catalog = StagingCatalog::create(db_path).unwrap();

    let plans = catalog.explain_lookup_query_plans().unwrap();
    assert_eq!(
        plans.len(),
        2,
        "Expected 2 query plans for the two-pass scan"
    );

    for (idx, plan) in plans.iter().enumerate() {
        println!("Plan {}: {}", idx + 1, plan);
        // Assert that the plan uses the primary key index
        assert!(
            plan.contains("USING INTEGER PRIMARY KEY") || plan.contains("PRIMARY KEY"),
            "Pass {} query plan must use INTEGER PRIMARY KEY, got: {}",
            idx + 1,
            plan
        );
        // Assert that NO in-memory sort or temp b-tree is used
        assert!(
            !plan.contains("TEMP B-TREE") && !plan.contains("USE TEMP"),
            "Pass {} query plan must NOT use temporary b-tree, got: {}",
            idx + 1,
            plan
        );
        assert!(
            !plan.contains("SCAN TABLE"),
            "Pass {} query plan must NOT perform full unindexed table scan, got: {}",
            idx + 1,
            plan
        );
    }
}

#[test]
fn test_lookup_unsigned_ordering_across_signed_i64_boundary() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("catalog.sqlite");
    let mut catalog = StagingCatalog::create(db_path).unwrap();

    // Critical boundary values spanning both sides of signed 64-bit integer
    let test_ids: Vec<u64> = vec![
        0,
        1,
        42,
        (i64::MAX as u64) - 1,
        i64::MAX as u64,
        (i64::MAX as u64) + 1, // i64::MIN in SQLite signed representation
        (i64::MAX as u64) + 2,
        u64::MAX - 100,
        u64::MAX - 1,
        u64::MAX,
    ];

    // Insert out of order to verify sorting is strictly enforced
    let insert_order = vec![
        (i64::MAX as u64) + 1,
        0,
        u64::MAX,
        42,
        (i64::MAX as u64) - 1,
        1,
        u64::MAX - 1,
        i64::MAX as u64,
        u64::MAX - 100,
        (i64::MAX as u64) + 2,
    ];

    for (idx, &id) in insert_order.iter().enumerate() {
        catalog.insert_entry(id, 0, 0, idx as u16).unwrap();
    }

    assert_eq!(catalog.count_active_games().unwrap(), test_ids.len() as u64);

    let idx_path = tmp.path().join("games.idx");
    let (_, file_size_bytes, sha256) = catalog
        .write_lookup_file(idx_path.clone(), Collection::MastersOtb)
        .unwrap();

    assert_eq!(
        file_size_bytes,
        32 + (test_ids.len() as u64 * 16) + 32,
        "File size must equal header (32) + entries (N*16) + footer (32)"
    );
    assert!(!sha256.is_empty());

    // Open with GameLookupReader and verify strictly increasing unsigned order
    let reader = GameLookupReader::open(&idx_path).unwrap();
    assert_eq!(reader.entry_count(), test_ids.len() as u64);

    for (i, &expected_id) in test_ids.iter().enumerate() {
        let entry = reader.get_entry(i).unwrap();
        assert_eq!(
            entry.game_id, expected_id,
            "Entry at index {} must match strictly sorted unsigned value",
            i
        );

        // Verify random-access binary search finds every boundary ID
        let locs = reader.find_locations(expected_id);
        assert_eq!(
            locs.len(),
            1,
            "Binary search must find exact match for ID {}",
            expected_id
        );
    }
}

#[test]
fn test_staging_catalog_rejects_duplicate_game_id() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("catalog.sqlite");
    let mut catalog = StagingCatalog::create(db_path).unwrap();

    catalog.insert_entry(12345, 0, 0, 0).unwrap();
    let duplicate_res = catalog.insert_entry(12345, 0, 0, 1);
    assert!(duplicate_res.is_err());
    let err_msg = format!("{}", duplicate_res.unwrap_err());
    assert!(
        err_msg.contains("Game ID collision") || err_msg.contains("already exists"),
        "Expected collision error, got: {}",
        err_msg
    );
}
