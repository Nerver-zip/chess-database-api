use codb_core::{Collection, CompactMove, GameMetadata, GameResult, IngestionStats, ParsedGame};
use codb_storage::{
    build_dataset_position_index, recover_dataset, verify_dataset, DatasetIndexConfig, DatasetLock,
    DatasetReader, DatasetWriter, DatasetWriterConfig, SourceArchiveMetadata, StagingLock,
    StorageError,
};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn make_game(id: u64, collection: Collection, moves: Vec<CompactMove>) -> ParsedGame {
    ParsedGame {
        metadata: GameMetadata {
            id,
            collection,
            white: format!("White_{}", id),
            black: format!("Black_{}", id),
            white_elo: Some(2600),
            black_elo: Some(2550),
            date: Some("2025.01.01".into()),
            event: Some("Masters Tournament".into()),
            site: Some("Zurich".into()),
            round: Some("1".into()),
            result: GameResult::WhiteWins,
            eco: Some("C50".into()),
            time_control: Some("5400+30".into()),
            source_id: Some(format!("https://example.com/games/{}", id)),
            broadcast_url: None,
            source_attribution: None,
        },
        moves,
        fen_start: None,
    }
}

fn build_test_dataset(target_dir: &Path, num_games: usize) {
    let config = DatasetWriterConfig {
        collection: Collection::MastersOtb,
        block_capacity: 5,
        max_games_per_shard: 10,
        zstd_level: 3,
        ..Default::default()
    };

    let mut writer = DatasetWriter::new(target_dir.to_path_buf(), config).unwrap();
    let e2e4 = CompactMove::new(12, 28, None);
    let e7e5 = CompactMove::new(52, 36, None);

    for i in 0..num_games {
        let id = (i as u64) + 1;
        let game = make_game(id, Collection::MastersOtb, vec![e2e4, e7e5]);
        writer.push_game(game).unwrap();
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
fn test_concurrent_index_build_and_recovery_prevented_by_lock() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("dataset");
    build_test_dataset(&ds_dir, 20);

    // Acquire dataset lock simulating an active indexing/mutation process
    let lock = DatasetLock::try_acquire_exclusive(&ds_dir).unwrap();

    // Concurrent recovery must fail closed with StorageError::Lock
    let recovery_res = recover_dataset(&ds_dir);
    match recovery_res {
        Err(StorageError::Lock(msg)) => {
            assert!(
                msg.contains("is locked by another active process"),
                "Expected lock collision error, got: {}",
                msg
            );
        }
        other => panic!("Expected StorageError::Lock, got: {:?}", other),
    }

    drop(lock);

    // After lock is released, recovery succeeds
    let recovered = recover_dataset(&ds_dir).unwrap();
    assert!(recovered.exists());
}

#[test]
fn test_two_simultaneous_index_build_attempts_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("dataset");
    build_test_dataset(&ds_dir, 20);

    // Process 1 holds dataset lock
    let lock1 = DatasetLock::try_acquire_exclusive(&ds_dir).unwrap();

    // Process 2 attempts to build position index concurrently
    let build2_res = build_dataset_position_index(
        &ds_dir,
        DatasetIndexConfig {
            depth_plies: Some(40),
            memory_limit_bytes: 16 * 1024 * 1024,
            max_merge_fan_in: 4,
            positions_per_block: 16,
            zstd_level: 3,
            expected_collection: Some(Collection::MastersOtb),
            build_id_override: None,
        },
    );

    match build2_res {
        Err(StorageError::Lock(msg)) => {
            assert!(
                msg.contains("is locked by another active process"),
                "Expected lock collision message, got: {}",
                msg
            );
        }
        other => panic!("Expected StorageError::Lock, got: {:?}", other),
    }

    drop(lock1);

    // Now process 2 succeeds without conflict
    let report = build_dataset_position_index(
        &ds_dir,
        DatasetIndexConfig {
            depth_plies: Some(40),
            memory_limit_bytes: 16 * 1024 * 1024,
            max_merge_fan_in: 4,
            positions_per_block: 16,
            zstd_level: 3,
            expected_collection: Some(Collection::MastersOtb),
            build_id_override: None,
        },
    )
    .unwrap();
    assert_eq!(report.stats.games_processed, 20);
}

#[test]
fn test_active_staging_directory_protected_against_unrelated_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("dataset");
    build_test_dataset(&ds_dir, 10);

    // Simulate an active staging directory with a live StagingLock
    let versions_dir = ds_dir.join("versions");
    let staging_dir = versions_dir.join(".staging_index_99999_123456789_abcdef");
    fs::create_dir_all(&staging_dir).unwrap();
    let staging_lock = StagingLock::acquire(&staging_dir).unwrap();

    // Verify staging is recognized as locked
    assert!(StagingLock::is_staging_locked(&staging_dir));

    // Also create a stale staging directory without a lock
    let stale_dir = versions_dir.join(".staging_index_99998_123456788_deadbeef");
    fs::create_dir_all(&stale_dir).unwrap();
    let stale_lock = StagingLock::acquire(&stale_dir).unwrap();
    drop(stale_lock); // Lock dropped -> stale
    assert!(!StagingLock::is_staging_locked(&stale_dir));

    // Recovery runs (with dataset lock available)
    recover_dataset(&ds_dir).unwrap();

    // The active staging directory MUST STILL EXIST!
    assert!(
        staging_dir.exists(),
        "Active staging directory protected by StagingLock must NOT be removed by recovery!"
    );

    // The stale staging directory MUST HAVE BEEN CLEANED UP!
    assert!(
        !stale_dir.exists(),
        "Stale staging directory without lock must be cleaned up by recovery!"
    );

    drop(staging_lock);

    // Now that staging_lock is dropped, another recovery will safely remove it
    recover_dataset(&ds_dir).unwrap();
    assert!(!staging_dir.exists());
}

#[test]
fn test_dataset_version_collision_fails_closed_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("dataset");
    build_test_dataset(&ds_dir, 10);

    // Run a first real index build that publishes an immutable version with a known ID.
    let colliding_id = "codb-collision-regression-version".to_string();
    let first = build_dataset_position_index(
        &ds_dir,
        DatasetIndexConfig {
            depth_plies: Some(10),
            build_id_override: Some(colliding_id.clone()),
            ..Default::default()
        },
    )
    .expect("first publication must succeed");
    assert_eq!(first.published_build_id, colliding_id);

    let versions_dir = ds_dir.join("versions");
    let version_dir = versions_dir.join(&colliding_id);
    assert!(version_dir.is_dir());

    // Fingerprint every file of the immutable published version.
    let fingerprint = |root: &Path| -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = fs::read_dir(root)
            .unwrap()
            .filter_map(|e| {
                let e = e.unwrap();
                let p = e.path();
                if p.is_file() {
                    Some((
                        e.file_name().to_string_lossy().into_owned(),
                        fs::read(&p).unwrap(),
                    ))
                } else {
                    None
                }
            })
            .collect();
        out.sort();
        out
    };
    let before = fingerprint(&version_dir);
    let current_before = fs::read_link(ds_dir.join("current")).unwrap();

    // Second real publication forced to produce the SAME build ID must fail closed.
    let second = build_dataset_position_index(
        &ds_dir,
        DatasetIndexConfig {
            depth_plies: Some(10),
            build_id_override: Some(colliding_id.clone()),
            ..Default::default()
        },
    );
    match second {
        Err(StorageError::Integrity(msg)) => {
            assert!(
                msg.contains("refusing to overwrite immutable version"),
                "unexpected integrity message: {}",
                msg
            );
        }
        other => panic!("Expected StorageError::Integrity, got: {:?}", other),
    }

    // Published version is byte-identical, `current` is unchanged, dataset still verifies.
    assert_eq!(fingerprint(&version_dir), before);
    assert_eq!(
        fs::read_link(ds_dir.join("current")).unwrap(),
        current_before
    );
    verify_dataset(&ds_dir, true).expect("dataset must remain valid after rejected collision");

    // Recoverability: recovery cleans the abandoned staging directory and keeps the version.
    recover_dataset(&ds_dir).unwrap();
    assert!(version_dir.is_dir());
    let leftovers: Vec<_> = fs::read_dir(&versions_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".staging_"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "stale staging not cleaned: {:?}",
        leftovers
    );
    verify_dataset(&ds_dir, true).unwrap();
}

#[test]
fn test_snapshot_readers_operate_concurrently_during_build_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("dataset");
    build_test_dataset(&ds_dir, 30);

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_reader = Arc::clone(&stop_flag);
    let ds_dir_clone = ds_dir.clone();

    // Background thread continuously reading from published snapshot
    let reader_handle = thread::spawn(move || {
        let mut read_cycles = 0;
        while !stop_reader.load(Ordering::Relaxed) {
            let reader = DatasetReader::open(&ds_dir_clone).unwrap();
            let manifest = reader.manifest();
            assert_eq!(manifest.total_games, 30);
            let mut count = 0;
            for g_res in reader.iter_games().unwrap() {
                let g = g_res.unwrap();
                assert!(g.metadata.id > 0);
                count += 1;
            }
            assert_eq!(count, 30);
            read_cycles += 1;
            thread::sleep(Duration::from_millis(5));
        }
        read_cycles
    });

    // While reader is reading, build a position index and publish a new version
    let report = build_dataset_position_index(
        &ds_dir,
        DatasetIndexConfig {
            depth_plies: Some(40),
            memory_limit_bytes: 16 * 1024 * 1024,
            max_merge_fan_in: 4,
            positions_per_block: 8,
            zstd_level: 3,
            expected_collection: Some(Collection::MastersOtb),
            build_id_override: None,
        },
    )
    .unwrap();

    assert_eq!(report.stats.games_processed, 30);
    assert!(verify_dataset(&ds_dir, true).is_ok());

    // Stop reader thread and verify it encountered zero errors
    stop_flag.store(true, Ordering::Relaxed);
    let cycles = reader_handle.join().unwrap();
    assert!(cycles > 0, "Reader thread completed {} cycles", cycles);
}

#[test]
fn test_repeated_recovery_idempotency() {
    let dir = tempfile::tempdir().unwrap();
    let ds_dir = dir.path().join("dataset");
    build_test_dataset(&ds_dir, 10);

    // Call recovery multiple times consecutively
    for _ in 0..5 {
        let cur = recover_dataset(&ds_dir).unwrap();
        assert!(cur.exists());
        assert!(verify_dataset(&ds_dir, false).is_ok());
    }
}
