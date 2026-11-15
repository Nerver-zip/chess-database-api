use codb_core::{
    compute_position_key, Collection, CompactMove, GameMetadata, GameResult, IngestionStats,
    ParsedGame,
};
use codb_storage::fault::{self, FaultPoint};
use codb_storage::{
    build_dataset_position_index, recover_dataset, verify_dataset, DatasetIndexConfig,
    DatasetReader, DatasetWriter, DatasetWriterConfig, SourceArchiveMetadata,
};
use shakmaty::{Chess, Move, Position, Role, Square};
use std::fs;
use std::path::Path;

fn make_game(
    id: u64,
    collection: Collection,
    result: GameResult,
    moves: Vec<CompactMove>,
) -> ParsedGame {
    ParsedGame {
        metadata: GameMetadata {
            id,
            collection,
            white: format!("White_{}", id),
            black: format!("Black_{}", id),
            white_elo: Some(2650),
            black_elo: Some(2620),
            date: Some("2025.02.15".into()),
            event: Some("Masters Open".into()),
            site: Some("Zurich".into()),
            round: Some("1".into()),
            result,
            eco: Some("C50".into()),
            time_control: Some("5400+30".into()),
            source_id: Some(format!("https://lichess.org/broadcast/test/{}", id)),
            broadcast_url: None,
            source_attribution: None,
        },
        moves,
        fen_start: None,
    }
}

fn build_multi_shard_dataset(target_dir: &Path, num_games: usize) {
    let config = DatasetWriterConfig {
        collection: Collection::MastersOtb,
        block_capacity: 3,      // Multiple blocks per shard
        max_games_per_shard: 5, // Multiple shards
        zstd_level: 3,
        ..Default::default()
    };

    let mut writer = DatasetWriter::new(target_dir.to_path_buf(), config).unwrap();
    let e2e4 = CompactMove::new(12, 28, None);
    let e7e5 = CompactMove::new(52, 36, None);
    let c7c5 = CompactMove::new(50, 34, None);
    let g1f3 = CompactMove::new(6, 21, None);
    let b8c6 = CompactMove::new(57, 42, None);

    for i in 0..num_games {
        let id = (i as u64) + 100;
        let moves = if i % 2 == 0 {
            vec![e2e4, e7e5, g1f3, b8c6]
        } else {
            vec![e2e4, c7c5, g1f3, b8c6]
        };
        let res = match i % 3 {
            0 => GameResult::WhiteWins,
            1 => GameResult::Draw,
            _ => GameResult::BlackWins,
        };
        writer
            .push_game(make_game(id, Collection::MastersOtb, res, moves))
            .unwrap();
    }

    let stats = IngestionStats {
        total_encountered: num_games as u64,
        accepted_unique: num_games as u64,
        ..Default::default()
    };
    let sources = vec![SourceArchiveMetadata {
        filename: "test.pgn.zst".into(),
        url: None,
        sha256: Some("00".repeat(32)),
        size_bytes: 1024,
        license: "CC0".into(),
        checksum_verified: Some(true),
    }];

    writer.finalize(stats, sources).unwrap();
}

#[test]
fn test_sequential_traversal_multi_shard_multi_block_and_empty() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("multi_shard_ds");
    build_multi_shard_dataset(&ds_dir, 14); // 3 shards (5, 5, 4), multiple blocks each

    let mut reader = DatasetReader::open(&ds_dir).unwrap();
    assert_eq!(reader.manifest().shards.len(), 3);
    assert_eq!(reader.total_games(), 14);

    let streamed: Vec<ParsedGame> = reader
        .iter_games()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(streamed.len(), 14);

    for (idx, g) in streamed.iter().enumerate() {
        let expected_id = (idx as u64) + 100;
        assert_eq!(g.metadata.id, expected_id);
        let fetched = reader.get_game(expected_id).unwrap();
        assert_eq!(g, &fetched);
    }

    // Empty dataset traversal
    let empty_dir = dir.path().join("empty_ds");
    build_multi_shard_dataset(&empty_dir, 0);
    let empty_reader = DatasetReader::open(&empty_dir).unwrap();
    assert_eq!(empty_reader.total_games(), 0);
    let empty_streamed: Vec<ParsedGame> = empty_reader
        .iter_games()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(empty_streamed.is_empty());
}

#[test]
fn test_sequential_traversal_respects_supersession_canonical_only() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("supersede_ds");

    let config = DatasetWriterConfig {
        collection: Collection::MastersOtb,
        block_capacity: 2,
        max_games_per_shard: 4,
        zstd_level: 3,
        ..Default::default()
    };
    let mut writer = DatasetWriter::new(ds_dir.clone(), config).unwrap();
    let e2e4 = CompactMove::new(12, 28, None);
    let e7e5 = CompactMove::new(52, 36, None);
    let c7c5 = CompactMove::new(50, 34, None);

    // Append game 10 (will be superseded)
    writer
        .push_game(make_game(
            10,
            Collection::MastersOtb,
            GameResult::BlackWins,
            vec![e2e4, c7c5],
        ))
        .unwrap();
    // Append game 20 (remains active)
    writer
        .push_game(make_game(
            20,
            Collection::MastersOtb,
            GameResult::Draw,
            vec![e2e4, e7e5],
        ))
        .unwrap();
    // Supersede game 10 with game 30 (higher-fidelity canonical record)
    writer
        .handle_superseded_game(
            10,
            make_game(
                30,
                Collection::MastersOtb,
                GameResult::WhiteWins,
                vec![e2e4, e7e5],
            ),
        )
        .unwrap();

    let stats = IngestionStats {
        total_encountered: 3,
        accepted_unique: 2,
        superseded_prior: 1,
        ..Default::default()
    };
    writer.finalize(stats, vec![]).unwrap();

    let reader = DatasetReader::open(&ds_dir).unwrap();
    let games: Vec<ParsedGame> = reader
        .iter_games()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(games.len(), 2);
    let ids: Vec<u64> = games.iter().map(|g| g.metadata.id).collect();
    assert_eq!(ids, vec![20, 30]);

    // Build CPI index and confirm superseded game 10 (1.e4 c5, 0-1) is completely absent from CPI
    build_dataset_position_index(
        &ds_dir,
        DatasetIndexConfig {
            depth_plies: Some(40),
            memory_limit_bytes: 1024,
            ..Default::default()
        },
    )
    .unwrap();

    let indexed_reader = DatasetReader::open(&ds_dir).unwrap();
    let mut b = Chess::default();
    b.play_unchecked(Move::Normal {
        role: Role::Pawn,
        from: Square::E2,
        to: Square::E4,
        capture: None,
        promotion: None,
    });
    let after_e4 = indexed_reader
        .query_position(compute_position_key(&b))
        .unwrap()
        .unwrap();
    assert_eq!(after_e4.total_games, 2);
    assert_eq!(after_e4.white_wins, 1);
    assert_eq!(after_e4.draws, 1);
    assert_eq!(after_e4.black_wins, 0);
    assert_eq!(after_e4.moves.len(), 1);
    assert_eq!(after_e4.moves[0].uci, "e7e5");
}

#[test]
fn test_sequential_traversal_corrupted_or_missing_shard_and_snapshot_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("snapshot_iso_ds");
    build_multi_shard_dataset(&ds_dir, 6);

    // Open reader on initial version V1 (6 games, no CPI)
    let reader_v1 = DatasetReader::open(&ds_dir).unwrap();
    let mut iter_v1 = reader_v1.iter_games().unwrap();
    let first_game = iter_v1.next().unwrap().unwrap();
    assert_eq!(first_game.metadata.id, 100);

    // Publish a new dataset version V2 with 10 games while iter_v1 is mid-traversal
    build_multi_shard_dataset(&ds_dir, 10);
    let reader_v2 = DatasetReader::open(&ds_dir).unwrap();
    assert_eq!(reader_v2.total_games(), 10);

    // iter_v1 must remain pinned to V1 and yield the remaining 5 games of V1
    let rem_v1: Vec<ParsedGame> = iter_v1.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rem_v1.len(), 5);

    // Now test missing shard fail-closed behavior on an isolated copy
    let broken_dir = dir.path().join("broken_ds");
    build_multi_shard_dataset(&broken_dir, 6);
    let broken_reader = DatasetReader::open(&broken_dir).unwrap();
    let shard0_path = broken_reader
        .snapshot_dir()
        .join("shards")
        .join(&broken_reader.manifest().shards[0].filename);
    fs::remove_file(&shard0_path).unwrap();

    let mut broken_iter = broken_reader.iter_games().unwrap();
    assert!(
        broken_iter.next().unwrap().is_err(),
        "Missing shard must fail closed"
    );
    assert!(
        broken_iter.next().is_none(),
        "Iterator must terminate after error"
    );
}

#[test]
fn test_cpi_publication_verification_and_fault_injection_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("cpi_pub_ds");
    build_multi_shard_dataset(&ds_dir, 12);

    let initial_reader = DatasetReader::open(&ds_dir).unwrap();
    let initial_build_id = initial_reader.manifest().dataset_build_id.clone();
    assert!(initial_reader.manifest().position_index.is_none());

    // Collection mismatch must fail closed without mutating dataset
    let mismatch_res = build_dataset_position_index(
        &ds_dir,
        DatasetIndexConfig {
            expected_collection: Some(Collection::EliteOnline),
            ..Default::default()
        },
    );
    assert!(mismatch_res.is_err());

    // Test fault injection at every indexing/publication stage before activation:
    // 1. IndexDuringRunGeneration
    // 2. IndexDuringExternalMerge
    // 3. IndexBeforeCpiCompletion
    // 4. IndexAfterCpiBeforeVerify
    // 5. IndexAfterVerifyBeforeActivation
    for fp in [
        FaultPoint::IndexDuringRunGeneration,
        FaultPoint::IndexDuringExternalMerge,
        FaultPoint::IndexBeforeCpiCompletion,
        FaultPoint::IndexAfterCpiBeforeVerify,
        FaultPoint::IndexAfterVerifyBeforeActivation,
    ] {
        fault::set_fault_point(Some(fp));
        let res = build_dataset_position_index(
            &ds_dir,
            DatasetIndexConfig {
                depth_plies: Some(40),
                memory_limit_bytes: 128,
                max_merge_fan_in: 2,
                positions_per_block: 4,
                zstd_level: 3,
                expected_collection: Some(Collection::MastersOtb),
                build_id_override: None,
            },
        );
        fault::set_fault_point(None);
        assert!(res.is_err(), "Expected failure at {:?}", fp);

        // Active dataset must still be the original valid version
        let active_reader = DatasetReader::open(&ds_dir).unwrap();
        assert_eq!(active_reader.manifest().dataset_build_id, initial_build_id);
        assert!(active_reader.manifest().position_index.is_none());

        // recover_dataset must clean up all `.staging_index_*` directories idempotently
        recover_dataset(&ds_dir).unwrap();
        recover_dataset(&ds_dir).unwrap();
        let versions_entries: Vec<_> = fs::read_dir(ds_dir.join("versions"))
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        for entry in versions_entries {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with(".staging_"),
                "Stale staging dir left behind after recovery: {}",
                name
            );
        }
        assert!(verify_dataset(&ds_dir, true).is_ok());
    }

    // 6. Test fault injection at IndexAfterCurrentSymlinkSwitch (post-commit)
    fault::set_fault_point(Some(FaultPoint::IndexAfterCurrentSymlinkSwitch));
    let res = build_dataset_position_index(
        &ds_dir,
        DatasetIndexConfig {
            depth_plies: Some(40),
            memory_limit_bytes: 256,
            max_merge_fan_in: 2,
            positions_per_block: 4,
            zstd_level: 3,
            expected_collection: Some(Collection::MastersOtb),
            build_id_override: None,
        },
    );
    fault::set_fault_point(None);
    assert!(res.is_err());

    // Recovery after post-commit interruption must complete root symlink setup (`positions.cpi`)
    recover_dataset(&ds_dir).unwrap();
    assert!(ds_dir.join("positions.cpi").exists());
    let report = verify_dataset(&ds_dir, true).unwrap();
    assert_eq!(report.position_index_present, Some(true));
    assert_eq!(report.position_index_verified, Some(true));

    let post_reader = DatasetReader::open(&ds_dir).unwrap();
    assert_ne!(post_reader.manifest().dataset_build_id, initial_build_id);
    let cpi_meta = post_reader.manifest().position_index.as_ref().unwrap();
    assert_eq!(cpi_meta.games_processed, 12);
    assert_eq!(cpi_meta.games_indexed, 12);
    assert_eq!(cpi_meta.excluded_unknown_result_games, 0);
    assert_eq!(cpi_meta.indexed_depth_plies, 40);

    let start_key = compute_position_key(&Chess::default());
    let start_stats = post_reader.query_position(start_key).unwrap().unwrap();
    assert_eq!(start_stats.total_games, 12);
    assert_eq!(start_stats.white_wins, 4);
    assert_eq!(start_stats.draws, 4);
    assert_eq!(start_stats.black_wins, 4);
    assert_eq!(start_stats.moves.len(), 1);
    assert_eq!(start_stats.moves[0].uci, "e2e4");
}

#[test]
fn test_verify_dataset_detects_missing_or_corrupted_cpi() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("cpi_corrupt_ds");
    build_multi_shard_dataset(&ds_dir, 8);

    build_dataset_position_index(&ds_dir, DatasetIndexConfig::default()).unwrap();
    assert!(verify_dataset(&ds_dir, true).is_ok());

    let reader = DatasetReader::open(&ds_dir).unwrap();
    let cpi_path = reader.snapshot_dir().join("positions.cpi");
    let orig_bytes = fs::read(&cpi_path).unwrap();

    // Corrupt a byte in CPI -> verify_dataset must fail closed
    let mut bad_bytes = orig_bytes.clone();
    bad_bytes[10] ^= 0xFF;
    fs::write(&cpi_path, &bad_bytes).unwrap();
    assert!(verify_dataset(&ds_dir, false).is_err());

    // Remove positions.cpi -> verify_dataset must fail closed
    fs::remove_file(&cpi_path).unwrap();
    assert!(verify_dataset(&ds_dir, false).is_err());

    // Restore original CPI -> verify_dataset passes again
    fs::write(&cpi_path, &orig_bytes).unwrap();
    assert!(verify_dataset(&ds_dir, true).is_ok());
}
