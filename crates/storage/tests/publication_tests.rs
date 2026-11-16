use opening_explorer_core::{
    compute_stable_game_id, Collection, CompactMove, GameMetadata, GameResult,
    IngestionCompletionStatus, IngestionStats, ParsedGame,
};
use opening_explorer_storage::fault::{self, FaultPoint};
use opening_explorer_storage::{
    cleanup_old_versions, publish_prebuilt_dataset, recover_dataset, resolve_dataset_snapshot,
    verify_dataset, DatasetReader, DatasetWriter, DatasetWriterConfig, SourceArchiveMetadata,
    StorageError,
};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
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
        checksum_verified: None,
    };

    writer.finalize(stats, vec![source])?;
    Ok(())
}

fn create_legacy_flat_dataset(
    target_dir: &Path,
    games_count: usize,
    seed_offset: u64,
) -> Result<(), StorageError> {
    create_dataset(target_dir, games_count, seed_offset)?;
    let active_version = resolve_dataset_snapshot(target_dir)?;
    let temp_staging = target_dir
        .parent()
        .unwrap()
        .join(format!("legacy_flat_tmp_{}", std::process::id()));
    if temp_staging.exists() {
        let _ = fs::remove_dir_all(&temp_staging);
    }
    fs::create_dir_all(&temp_staging)?;
    for item in ["manifest.json", "games.idx", "shards"] {
        let src = active_version.join(item);
        if src.exists() {
            if src.is_dir() {
                let dst = temp_staging.join(item);
                fs::create_dir_all(&dst)?;
                for entry in fs::read_dir(&src)? {
                    let entry = entry?;
                    fs::copy(entry.path(), dst.join(entry.file_name()))?;
                }
            } else {
                fs::copy(&src, temp_staging.join(item))?;
            }
        }
    }
    let _ = fs::remove_dir_all(target_dir);
    fs::create_dir_all(target_dir)?;
    for item in ["manifest.json", "games.idx", "shards"] {
        let src = temp_staging.join(item);
        if src.exists() {
            fs::rename(src, target_dir.join(item))?;
        }
    }
    let _ = fs::remove_dir_all(&temp_staging);
    Ok(())
}

#[test]
fn test_prebuilt_publication_keeps_pinned_readers_valid_during_atomic_transition() {
    let tmp = tempdir().unwrap();
    let candidate_a = tmp.path().join("candidate-a");
    let candidate_b = tmp.path().join("candidate-b");
    let target = tmp.path().join("stable-dataset");

    create_dataset(&candidate_a, 4, 100).unwrap();
    publish_prebuilt_dataset(&candidate_a, &target, Collection::MastersOtb).unwrap();
    let old_snapshot = resolve_dataset_snapshot(&target).unwrap();
    let pinned_reader = DatasetReader::open(&target).unwrap();
    assert_eq!(pinned_reader.total_games(), 4);

    create_dataset(&candidate_b, 7, 500).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let reader_target = target.clone();
    let reader_barrier = barrier.clone();
    let reader_observed = observed.clone();
    let reader_failures = failures.clone();
    let reader_thread = thread::spawn(move || {
        reader_barrier.wait();
        for _ in 0..256 {
            match DatasetReader::open(&reader_target) {
                Ok(reader) => reader_observed.lock().unwrap().push(reader.total_games()),
                Err(error) => reader_failures.lock().unwrap().push(error.to_string()),
            }
            thread::yield_now();
        }
    });

    barrier.wait();
    publish_prebuilt_dataset(&candidate_b, &target, Collection::MastersOtb).unwrap();
    reader_thread.join().unwrap();

    assert!(
        failures.lock().unwrap().is_empty(),
        "concurrent opens failed"
    );
    let counts = observed.lock().unwrap();
    assert!(!counts.is_empty(), "concurrent reader loop did not execute");
    assert!(
        counts.iter().all(|count| *count == 4 || *count == 7),
        "readers observed an incomplete or mixed snapshot: {counts:?}"
    );
    assert_eq!(pinned_reader.total_games(), 4);
    assert!(
        old_snapshot.is_dir(),
        "the old immutable version must remain available"
    );
    assert!(
        verify_dataset(&old_snapshot, true)
            .unwrap()
            .deep_verification_passed
    );
    assert_eq!(DatasetReader::open(&target).unwrap().total_games(), 7);
}

#[test]
fn test_prebuilt_publication_preserves_known_good_active_version_after_candidate_failure() {
    let tmp = tempdir().unwrap();
    let candidate_a = tmp.path().join("candidate-a");
    let candidate_b = tmp.path().join("candidate-b");
    let target = tmp.path().join("stable-dataset");

    create_dataset(&candidate_a, 4, 100).unwrap();
    publish_prebuilt_dataset(&candidate_a, &target, Collection::MastersOtb).unwrap();
    let previous_snapshot = resolve_dataset_snapshot(&target).unwrap();

    create_dataset(&candidate_b, 7, 500).unwrap();
    let candidate_snapshot = resolve_dataset_snapshot(&candidate_b).unwrap();
    let shard = fs::read_dir(candidate_snapshot.join("shards"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(&shard, b"corrupt candidate shard").unwrap();

    let error =
        publish_prebuilt_dataset(&candidate_b, &target, Collection::MastersOtb).unwrap_err();
    assert!(error.to_string().contains("checksum mismatch"));
    assert_eq!(
        resolve_dataset_snapshot(&target).unwrap(),
        previous_snapshot
    );
    assert!(previous_snapshot.is_dir());
    assert_eq!(DatasetReader::open(&target).unwrap().total_games(), 4);
    assert!(
        verify_dataset(&target, true)
            .unwrap()
            .deep_verification_passed
    );
}

#[test]
fn test_prebuilt_publication_rejects_wrong_collection_without_target_mutation() {
    let tmp = tempdir().unwrap();
    let candidate = tmp.path().join("candidate");
    let target = tmp.path().join("stable-dataset");
    create_dataset(&candidate, 2, 200).unwrap();

    let error = publish_prebuilt_dataset(&candidate, &target, Collection::EliteOnline).unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match requested collection"));
    assert!(!target.join("current").exists());
}

#[test]
fn test_successful_publication_versioned_layout_and_replacement() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // 1. Initial publication when target does not exist
    assert!(!target.exists());
    create_dataset(&target, 4, 100).expect("Initial build should succeed");
    assert!(target.exists());

    // Verify versioned layout structure: versions/<build_id>, current -> versions/<build_id>
    let versions_dir = target.join("versions");
    assert!(versions_dir.is_dir());
    let v_entries: Vec<_> = fs::read_dir(&versions_dir)
        .unwrap()
        .map(|r| r.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        v_entries.len(),
        1,
        "Expected exactly 1 version after initial build"
    );

    let current_link = target.join("current");
    assert!(current_link.exists());
    let link_target = fs::read_link(&current_link).unwrap();
    assert!(link_target.to_string_lossy().contains(&v_entries[0]));

    // Convenience root symlinks
    assert!(target.join("manifest.json").exists());
    assert!(target.join("games.idx").exists());
    assert!(target.join("shards").exists());

    // Reader accesses dataset cleanly via target root and current
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
    let v_report = verify_dataset(&target, true).unwrap();
    assert!(v_report.deep_verification_passed);

    // 2. Publication replacing existing dataset
    create_dataset(&target, 6, 200).expect("Replacement build should succeed");
    assert!(target.exists());

    // Previous version was NOT deleted: versions directory now contains 2 immutable versions!
    let v_entries_2: Vec<_> = fs::read_dir(&versions_dir)
        .unwrap()
        .map(|r| r.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        v_entries_2.len(),
        2,
        "Previous version must be preserved in versions/"
    );

    let mut reader2 = DatasetReader::open(&target).unwrap();
    assert_eq!(reader2.total_games(), 6);
    // Old game 100 is no longer active in the new dataset
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(reader2.get_game(g100.metadata.id).is_err());
    // New game 200 is present
    let g200 = sample_game(200, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(reader2.get_game(g200.metadata.id).is_ok());

    let v_report2 = verify_dataset(&target, true).unwrap();
    assert!(v_report2.deep_verification_passed);
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
        accepted_unique: 5,
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
fn test_failure_before_staging_verification_preserves_target() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");
    create_dataset(&target, 4, 100).expect("Initial build should succeed");
    let original_manifest = fs::read_to_string(target.join("manifest.json")).unwrap();

    // Inject fault before staging verify
    fault::set_fault_point(Some(FaultPoint::BeforeStagingVerify));
    let fail_res = create_dataset(&target, 6, 200);
    fault::set_fault_point(None);

    assert!(fail_res.is_err());
    let err_msg = format!("{}", fail_res.unwrap_err());
    assert!(err_msg.contains("Injected publication fault at BeforeStagingVerify"));

    // Target is untouched
    assert_eq!(
        fs::read_to_string(target.join("manifest.json")).unwrap(),
        original_manifest
    );
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
}

#[test]
fn test_failure_after_staging_verification_before_version_move_preserves_target() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");
    create_dataset(&target, 4, 100).expect("Initial build should succeed");
    let original_manifest = fs::read_to_string(target.join("manifest.json")).unwrap();

    // Inject fault after staging verify, before version directory move
    fault::set_fault_point(Some(FaultPoint::AfterStagingVerifyBeforeVersionMove));
    let fail_res = create_dataset(&target, 6, 200);
    fault::set_fault_point(None);

    assert!(fail_res.is_err());
    assert!(format!("{}", fail_res.unwrap_err())
        .contains("Injected publication fault at AfterStagingVerifyBeforeVersionMove"));

    // Target dataset is untouched
    assert_eq!(
        fs::read_to_string(target.join("manifest.json")).unwrap(),
        original_manifest
    );
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
}

#[test]
fn test_failure_during_active_version_transition_leaves_old_version_active() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");
    create_dataset(&target, 4, 100).expect("Initial build should succeed");

    // Inject fault before atomic current symlink switch
    fault::set_fault_point(Some(FaultPoint::BeforeCurrentSymlinkSwitch));
    let fail_res = create_dataset(&target, 6, 200);
    fault::set_fault_point(None);

    assert!(fail_res.is_err());
    assert!(format!("{}", fail_res.unwrap_err())
        .contains("Injected publication fault at BeforeCurrentSymlinkSwitch"));

    // Old version remains active in current! Readers continue reading without error.
    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(reader.get_game(g100.metadata.id).is_ok());

    let v_report = verify_dataset(&target, true).unwrap();
    assert!(v_report.deep_verification_passed);
}

#[test]
fn test_synchronization_failure_propagates_error() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // Test fail during sync_dir_contents
    fault::set_fault_point(Some(FaultPoint::FailSyncDirContents));
    let res1 = create_dataset(&target, 2, 10);
    fault::set_fault_point(None);
    assert!(res1.is_err(), "Sync dir contents failure must propagate");

    // Test fail during sync_dir_itself
    fault::set_fault_point(Some(FaultPoint::FailSyncDirItself));
    let res2 = create_dataset(&target, 2, 20);
    fault::set_fault_point(None);
    assert!(res2.is_err(), "Sync dir itself failure must propagate");
}

#[test]
fn test_recovery_with_existing_valid_dataset() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");
    create_dataset(&target, 4, 100).unwrap();

    let recovered_path = recover_dataset(&target).expect("Recovery on valid dataset must succeed");
    assert_eq!(recovered_path, target.join("current"));

    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
}

#[test]
fn test_recovery_with_both_old_and_new_versions_when_current_broken() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");
    create_dataset(&target, 4, 100).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    create_dataset(&target, 6, 200).unwrap();

    // Verify 2 versions in target/versions
    let versions_dir = target.join("versions");
    let v_count = fs::read_dir(&versions_dir).unwrap().count();
    assert_eq!(v_count, 2);

    // Break current symlink
    let current_path = target.join("current");
    fs::remove_file(&current_path).unwrap();
    assert!(!current_path.exists());

    // Run recovery: should restore current pointing to latest verified version (6 games)
    let recovered = recover_dataset(&target).unwrap();
    assert_eq!(recovered, target.join("current"));
    assert!(current_path.exists());

    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 6);
    let g200 = sample_game(200, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(reader.get_game(g200.metadata.id).is_ok());
}

#[test]
fn test_recovery_falls_back_to_old_version_if_newest_is_corrupt() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");
    create_dataset(&target, 4, 100).unwrap();

    // Create a newer corrupt version directory in versions/
    std::thread::sleep(std::time::Duration::from_millis(50));
    let corrupt_version = target
        .join("versions")
        .join("opening-explorer-corrupt-newer");
    fs::create_dir_all(&corrupt_version).unwrap();
    fs::write(corrupt_version.join("manifest.json"), "{ invalid json }").unwrap();

    // Break current symlink
    fs::remove_file(target.join("current")).unwrap();

    // Recovery must skip the corrupt newer version and safely fall back to the valid prior version
    recover_dataset(&target).expect("Recovery must succeed by falling back to valid version");

    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
}

#[test]
fn test_repeated_recovery_calls_idempotent() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");
    create_dataset(&target, 4, 100).unwrap();

    for _ in 0..5 {
        let p = recover_dataset(&target).unwrap();
        assert_eq!(p, target.join("current"));
        let reader = DatasetReader::open(&target).unwrap();
        assert_eq!(reader.total_games(), 4);
    }
}

#[test]
fn test_preexisting_staging_and_temp_symlink_cleanup() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");
    create_dataset(&target, 4, 100).unwrap();

    // Inject leftover staging directory and temp symlink
    let junk_staging = target.join(".staging_junk_12345");
    fs::create_dir_all(&junk_staging).unwrap();
    fs::write(junk_staging.join("junk.txt"), "leftover").unwrap();

    let junk_tmp_symlink = target.join(".current_tmp_99999");
    fs::write(&junk_tmp_symlink, "broken").unwrap();

    assert!(junk_staging.exists());
    assert!(junk_tmp_symlink.exists());

    // Recover dataset
    recover_dataset(&target).unwrap();

    // Artifacts must be safely cleaned up
    assert!(
        !junk_staging.exists(),
        "Leftover staging dir was not cleaned"
    );
    assert!(
        !junk_tmp_symlink.exists(),
        "Leftover temp symlink was not cleaned"
    );

    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
}

#[test]
fn test_cleanup_old_versions_never_removes_active_version() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // Build 3 versions
    create_dataset(&target, 2, 10).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    create_dataset(&target, 4, 20).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    create_dataset(&target, 6, 30).unwrap();

    let versions_dir = target.join("versions");
    assert_eq!(fs::read_dir(&versions_dir).unwrap().count(), 3);

    // Keep 1 previous version: should retain active (v3) + most recent previous (v2), deleting v1
    let removed = cleanup_old_versions(&target, 1).unwrap();
    assert_eq!(removed, 1);
    assert_eq!(fs::read_dir(&versions_dir).unwrap().count(), 2);

    // Active version must remain functional
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 6);

    // Keep 0 previous versions: should retain ONLY active (v3), deleting v2
    let removed2 = cleanup_old_versions(&target, 0).unwrap();
    assert_eq!(removed2, 1);
    assert_eq!(fs::read_dir(&versions_dir).unwrap().count(), 1);

    // Active version STILL intact
    let reader2 = DatasetReader::open(&target).unwrap();
    assert_eq!(reader2.total_games(), 6);
    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
}

#[test]
fn test_crash_simulation_subprocess_termination() {
    // If running as the child process:
    if std::env::var("OPENING_EXPLORER_TEST_SUBPROCESS_CRASH").as_deref() == Ok("1") {
        let child_target =
            std::path::PathBuf::from(std::env::var("OPENING_EXPLORER_TEST_CHILD_TARGET").unwrap());
        create_dataset(&child_target, 8, 500).unwrap();
        return;
    }

    // Parent process test harness:
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // 1. Create initial valid dataset v1 (4 games)
    create_dataset(&target, 4, 100).expect("Initial build must succeed");
    let original_manifest = fs::read_to_string(target.join("manifest.json")).unwrap();

    // 2. Spawn child process that crashes via SIGKILL/exit(137) during publication
    let current_exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(current_exe)
        .arg("test_crash_simulation_subprocess_termination")
        .arg("--exact")
        .env("OPENING_EXPLORER_TEST_SUBPROCESS_CRASH", "1")
        .env("OPENING_EXPLORER_TEST_CHILD_TARGET", &target)
        .env("OPENING_EXPLORER_CRASH_AT", "BeforeCurrentSymlinkSwitch")
        .status()
        .expect("Failed to spawn child crash test");

    // Child should have abruptly terminated with code 137
    assert!(!status.success());
    assert_eq!(
        status.code(),
        Some(137),
        "Child must terminate abruptly with code 137"
    );

    // 3. Parent verifies target was not corrupted by the child's abrupt termination
    assert!(target.exists());
    assert_eq!(
        fs::read_to_string(target.join("manifest.json")).unwrap(),
        original_manifest,
        "Target manifest must remain unchanged after child crash"
    );

    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(
        reader.total_games(),
        4,
        "Active dataset must remain v1 with 4 games"
    );

    // 4. Run recovery to clean up any abandoned staging artifacts
    let recovered = recover_dataset(&target).unwrap();
    assert_eq!(recovered, target.join("current"));

    let v_report = verify_dataset(&target, true).unwrap();
    assert!(v_report.deep_verification_passed);
}

#[test]
fn test_live_reader_pinned_to_snapshot_across_publication() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // 1. Publish dataset A with 4 games (seeds 100..104, block_cap=2, max_shard=4)
    create_dataset(&target, 4, 100).expect("Dataset A build should succeed");
    let mut reader_a = DatasetReader::open(&target).expect("Reader A should open target");
    assert_eq!(reader_a.total_games(), 4);
    let snap_a_dir = reader_a.snapshot_dir().to_path_buf();

    // Retrieve game 100 from dataset A
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    let retrieved_100 = reader_a.get_game(g100.metadata.id).unwrap();
    assert_eq!(retrieved_100.metadata.id, g100.metadata.id);

    // 2. Publish dataset B with 6 games and different seeds (500..506)
    std::thread::sleep(std::time::Duration::from_millis(50));
    create_dataset(&target, 6, 500).expect("Dataset B build should succeed");

    // 3. Reader A must STILL observe dataset A snapshot, even though 'current' now points to B!
    assert_eq!(reader_a.snapshot_dir(), &snap_a_dir);
    assert_eq!(reader_a.total_games(), 4);

    // Retrieve game 103 from dataset A through reader_a (triggers lazy shard decompression from A)
    let g103 = sample_game(103, Collection::MastersOtb, "Carlsen", "Caruana");
    let retrieved_103 = reader_a.get_game(g103.metadata.id).unwrap();
    assert_eq!(retrieved_103.metadata.id, g103.metadata.id);

    // Reader A cannot see games from dataset B
    let g500 = sample_game(500, Collection::MastersOtb, "Carlsen", "Caruana");
    assert!(matches!(
        reader_a.get_game(g500.metadata.id),
        Err(StorageError::GameNotFound(_))
    ));

    // 4. A newly opened reader B observes dataset B
    let mut reader_b = DatasetReader::open(&target).expect("Reader B should open target");
    assert_eq!(reader_b.total_games(), 6);
    assert_ne!(reader_b.snapshot_dir(), &snap_a_dir);

    // Reader B retrieves game 500
    let retrieved_500 = reader_b.get_game(g500.metadata.id).unwrap();
    assert_eq!(retrieved_500.metadata.id, g500.metadata.id);

    // Reader B cannot see games from dataset A
    assert!(matches!(
        reader_b.get_game(g100.metadata.id),
        Err(StorageError::GameNotFound(_))
    ));
}

#[test]
fn test_verify_dataset_uses_single_snapshot_during_publication() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // Publish dataset A (4 games)
    create_dataset(&target, 4, 100).expect("Dataset A build should succeed");
    let snap_a = resolve_dataset_snapshot(&target).expect("Snapshot A should resolve");

    // Publish dataset B (6 games)
    std::thread::sleep(std::time::Duration::from_millis(50));
    create_dataset(&target, 6, 500).expect("Dataset B build should succeed");

    // Verify snapshot A directly: operates strictly on snapshot A
    let rep_a = verify_dataset(&snap_a, true).expect("Snapshot A verification should pass");
    assert_eq!(rep_a.total_games, 4);
    assert!(rep_a.deep_verification_passed);

    // Verify target (which is now active version B): operates strictly on snapshot B
    let rep_b = verify_dataset(&target, true).expect("Dataset target verification should pass");
    assert_eq!(rep_b.total_games, 6);
    assert!(rep_b.deep_verification_passed);
}

#[test]
fn test_repeated_transitions_readers_retain_fidelity() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // Publish version A (4 games)
    create_dataset(&target, 4, 100).unwrap();
    let mut reader_a = DatasetReader::open(&target).unwrap();
    let snap_a = reader_a.snapshot_dir().to_path_buf();

    std::thread::sleep(std::time::Duration::from_millis(50));
    // Publish version B (6 games)
    create_dataset(&target, 6, 200).unwrap();
    let mut reader_b = DatasetReader::open(&target).unwrap();
    let snap_b = reader_b.snapshot_dir().to_path_buf();

    std::thread::sleep(std::time::Duration::from_millis(50));
    // Publish version C (8 games)
    create_dataset(&target, 8, 300).unwrap();
    let mut reader_c = DatasetReader::open(&target).unwrap();
    let snap_c = reader_c.snapshot_dir().to_path_buf();

    // Verify all 3 snapshots are physically distinct
    assert_ne!(snap_a, snap_b);
    assert_ne!(snap_b, snap_c);
    assert_ne!(snap_a, snap_c);

    // Verify reader A retains fidelity to version A
    assert_eq!(reader_a.total_games(), 4);
    let g103 = sample_game(103, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader_a.get_game(g103.metadata.id).unwrap().metadata.id,
        g103.metadata.id
    );

    // Verify reader B retains fidelity to version B
    assert_eq!(reader_b.total_games(), 6);
    let g205 = sample_game(205, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader_b.get_game(g205.metadata.id).unwrap().metadata.id,
        g205.metadata.id
    );

    // Verify reader C retains fidelity to version C
    assert_eq!(reader_c.total_games(), 8);
    let g307 = sample_game(307, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader_c.get_game(g307.metadata.id).unwrap().metadata.id,
        g307.metadata.id
    );
}

#[test]
fn test_retention_contract_cleanup_with_live_reader() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("dataset_published");

    // 1. Publish version A (4 games, seed 100)
    create_dataset(&target, 4, 100).unwrap();
    let mut reader_a = DatasetReader::open(&target).unwrap();

    // 2. Publish version B (6 games, seed 200)
    std::thread::sleep(std::time::Duration::from_millis(50));
    create_dataset(&target, 6, 200).unwrap();

    // 3. Cleanup with keep_versions = 1: keeps active (B) + 1 previous (A)
    let removed = cleanup_old_versions(&target, 1).unwrap();
    assert_eq!(removed, 0); // Both A and B are retained

    // Reader A remains 100% functional
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader_a.get_game(g100.metadata.id).unwrap().metadata.id,
        g100.metadata.id
    );

    // 4. Publish versions C and D
    std::thread::sleep(std::time::Duration::from_millis(50));
    create_dataset(&target, 6, 300).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    create_dataset(&target, 6, 400).unwrap();

    // 5. Cleanup with keep_versions = 1: keeps active (D) + 1 previous (C).
    // Older versions A and B are now pruned!
    let removed2 = cleanup_old_versions(&target, 1).unwrap();
    assert_eq!(removed2, 2); // Versions A and B pruned

    // If reader_a now attempts to open a shard that was not previously opened,
    // it fails closed with an explicit I/O error (not found), rather than reading corrupt/wrong data
    let snap_a = reader_a.snapshot_dir().to_path_buf();
    assert!(
        !snap_a.exists(),
        "Snapshot A directory must have been pruned"
    );
}

#[test]
fn test_legacy_dataset_compatibility() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("legacy_dataset");

    // Create an authentic legacy flat dataset (manifest.json, games.idx, shards in root)
    create_legacy_flat_dataset(&target, 4, 100).unwrap();

    // Verify it is recognized and readable
    assert!(target.join("manifest.json").is_file());
    assert!(!target.join("current").exists());
    assert!(!target.join("versions").exists());

    let mut reader = DatasetReader::open(&target).expect("Legacy flat dataset should open");
    assert_eq!(reader.total_games(), 4);
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    let retrieved = reader.get_game(g100.metadata.id).unwrap();
    assert_eq!(retrieved.metadata.id, g100.metadata.id);

    // Deep verification succeeds on legacy flat dataset
    let v_rep = verify_dataset(&target, true).expect("Legacy flat dataset should pass deep verify");
    assert!(v_rep.deep_verification_passed);
}

#[test]
fn test_legacy_migration_fault_injection_across_all_steps() {
    let fault_points = [
        FaultPoint::BeforeLegacyMigrationPrep,
        FaultPoint::AfterLegacyMigrationDestCreated,
        FaultPoint::AfterLegacyFirstFileTransfer,
        FaultPoint::BetweenLegacyFileTransfers,
        FaultPoint::AfterLegacyCandidateVerify,
        FaultPoint::BeforeLegacySymlinkSwitch,
        FaultPoint::AfterLegacySymlinkSwitch,
        FaultPoint::DuringLegacyCleanup,
    ];

    for fault in fault_points {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join(format!("legacy_migration_{:?}", fault));

        // Create authentic legacy flat dataset
        create_legacy_flat_dataset(&target, 4, 100).unwrap();

        // Inject fault
        fault::set_fault_point(Some(fault));

        // Attempt recovery or operation triggering migration: must fail at the fault point
        let res = recover_dataset(&target);
        assert!(res.is_err(), "Must fail at fault point {:?}", fault);

        // Reset fault
        fault::set_fault_point(None);

        // Recover dataset after failure: must recover without data loss!
        let recovered = recover_dataset(&target)
            .unwrap_or_else(|e| panic!("Failed to recover at fault point {:?}: {}", fault, e));
        assert_eq!(recovered, target.join("current"));

        // Deep verification must pass
        let v_rep = verify_dataset(&target, true).unwrap();
        assert!(v_rep.deep_verification_passed);

        // Reader must retrieve all games accurately
        let mut reader = DatasetReader::open(&target).unwrap();
        assert_eq!(reader.total_games(), 4);
        let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
        let g103 = sample_game(103, Collection::MastersOtb, "Carlsen", "Caruana");
        assert_eq!(
            reader.get_game(g100.metadata.id).unwrap().metadata.id,
            g100.metadata.id
        );
        assert_eq!(
            reader.get_game(g103.metadata.id).unwrap().metadata.id,
            g103.metadata.id
        );

        // Repeated recovery must be idempotent
        let rec2 = recover_dataset(&target).unwrap();
        assert_eq!(rec2, target.join("current"));
    }
}

#[test]
fn test_legacy_migration_split_state_recovery() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("split_state_dataset");

    // Create authentic legacy flat dataset
    create_legacy_flat_dataset(&target, 4, 100).unwrap();

    // Artificially simulate an interrupted move where manifest.json was moved to versions/legacy_v0
    // but games.idx and shards/ were left behind in the root target_dir
    let versions_dir = target.join("versions");
    let legacy_v0 = versions_dir.join("legacy_v0");
    fs::create_dir_all(&legacy_v0).unwrap();
    fs::rename(
        target.join("manifest.json"),
        legacy_v0.join("manifest.json"),
    )
    .unwrap();

    // Assert split state
    assert!(!target.join("manifest.json").exists());
    assert!(target.join("games.idx").exists());
    assert!(target.join("shards").exists());
    assert!(legacy_v0.join("manifest.json").exists());
    assert!(!legacy_v0.join("games.idx").exists());
    assert!(!target.join("current").exists());

    // Recover dataset: must consolidate split state, activate legacy_v0, and restore full functionality
    let recovered = recover_dataset(&target).expect("Split state recovery must succeed");
    assert_eq!(recovered, target.join("current"));

    // Deep verification must pass
    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);

    // Retrieve games
    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader.get_game(g100.metadata.id).unwrap().metadata.id,
        g100.metadata.id
    );
}

#[test]
fn test_recovery_preserves_sole_surviving_manifest_on_copy_failure() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("sole_manifest_dataset");

    // 1. Create authentic legacy flat dataset
    create_legacy_flat_dataset(&target, 4, 100).unwrap();

    // 2. Set up the exact reproduction scenario:
    // dataset/
    //   games.idx
    //   shards/
    //   versions/
    //     legacy_v0/
    //       manifest.json
    let versions_dir = target.join("versions");
    let legacy_v0 = versions_dir.join("legacy_v0");
    fs::create_dir_all(&legacy_v0).unwrap();
    fs::rename(
        target.join("manifest.json"),
        legacy_v0.join("manifest.json"),
    )
    .unwrap();

    let manifest_bytes = fs::read(legacy_v0.join("manifest.json")).unwrap();
    assert!(!target.join("manifest.json").exists());
    assert!(target.join("games.idx").exists());
    assert!(target.join("shards").exists());
    assert!(legacy_v0.join("manifest.json").exists());
    assert!(!legacy_v0.join("games.idx").exists());
    assert!(!target.join("current").exists());

    // 3. Inject fault: failure copying manifest back to root during recovery
    fault::set_fault_point(Some(FaultPoint::FailRecoveryCopyManifest));

    // 4. Run recovery: MUST fail cleanly
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailRecoveryCopyManifest"),
        "Unexpected error: {}",
        err
    );

    // 5. CRITICAL INVARIANT: The sole surviving manifest MUST NOT be deleted!
    assert!(
        legacy_v0.join("manifest.json").exists(),
        "CRITICAL BUG: Sole surviving manifest in legacy_v0 was deleted by recovery failure!"
    );
    assert_eq!(
        fs::read(legacy_v0.join("manifest.json")).unwrap(),
        manifest_bytes,
        "Sole surviving manifest content was altered or corrupted"
    );
    assert!(
        target.join("games.idx").exists(),
        "games.idx was deleted by recovery failure"
    );
    assert!(
        target.join("shards").exists(),
        "shards directory was deleted by recovery failure"
    );

    // 6. Clear fault injection
    fault::set_fault_point(None);

    // 7. Retry recovery: MUST succeed and restore the dataset completely
    let recovered = recover_dataset(&target).expect("Recovery after fault clearance must succeed");
    assert_eq!(recovered, target.join("current"));

    // 8. Deep verification must pass
    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);

    // 9. DatasetReader retrieves all games accurately
    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    let g103 = sample_game(103, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader.get_game(g100.metadata.id).unwrap().metadata.id,
        g100.metadata.id
    );
    assert_eq!(
        reader.get_game(g103.metadata.id).unwrap().metadata.id,
        g103.metadata.id
    );

    // 10. Repeated recovery must be idempotent
    let rec2 = recover_dataset(&target).unwrap();
    assert_eq!(rec2, target.join("current"));
}

#[test]
fn test_recovery_filesystem_failure_injection_suite() {
    let fault_points = [
        FaultPoint::FailRecoveryCreateDestDir,
        FaultPoint::FailRecoveryCopyLookup,
        FaultPoint::FailRecoveryCopyShard,
        FaultPoint::FailRecoverySyncReconstructed,
        FaultPoint::FailCreateRootSymlink,
    ];

    for fault in fault_points {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join(format!("fault_{:?}", fault));

        // Create authentic legacy flat dataset
        create_legacy_flat_dataset(&target, 4, 100).unwrap();

        // Create interrupted split state
        let versions_dir = target.join("versions");
        let legacy_v0 = versions_dir.join("legacy_v0");
        fs::create_dir_all(&legacy_v0).unwrap();
        fs::rename(
            target.join("manifest.json"),
            legacy_v0.join("manifest.json"),
        )
        .unwrap();

        let manifest_bytes = fs::read(legacy_v0.join("manifest.json")).unwrap();

        // Inject fault
        fault::set_fault_point(Some(fault));

        // Recovery must fail and propagate error
        let err = match recover_dataset(&target) {
            Err(e) => e,
            Ok(_) => panic!("Fault {:?} must fail recovery", fault),
        };
        assert!(
            err.to_string().contains(&format!("{:?}", fault)),
            "Error for {:?} was: {}",
            fault,
            err
        );

        // Crucial invariant: artifacts are never destroyed on failure
        assert!(
            legacy_v0.join("manifest.json").exists(),
            "Manifest was destroyed at fault {:?}",
            fault
        );
        assert_eq!(
            fs::read(legacy_v0.join("manifest.json")).unwrap(),
            manifest_bytes
        );
        assert!(
            target.join("games.idx").exists() || legacy_v0.join("games.idx").exists(),
            "games.idx was lost completely at fault {:?}",
            fault
        );

        // Clear fault
        fault::set_fault_point(None);

        // Retry recovery: must succeed
        let recovered = recover_dataset(&target)
            .unwrap_or_else(|e| panic!("Recovery failed after clearing {:?}: {}", fault, e));
        assert_eq!(recovered, target.join("current"));

        // Deep verification
        let v_rep = verify_dataset(&target, true).unwrap();
        assert!(v_rep.deep_verification_passed);

        // Verify game retrieval
        let mut reader = DatasetReader::open(&target).unwrap();
        assert_eq!(reader.total_games(), 4);
        let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
        assert_eq!(
            reader.get_game(g100.metadata.id).unwrap().metadata.id,
            g100.metadata.id
        );

        // Idempotency
        let rec2 = recover_dataset(&target).unwrap();
        assert_eq!(rec2, target.join("current"));
    }
}

#[test]
fn test_recovery_preserves_artifacts_on_verification_failure() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("corrupted_recovery_dataset");

    // 1. Create flat dataset
    create_legacy_flat_dataset(&target, 4, 100).unwrap();

    // 2. Corrupt a shard in root so verification will fail
    let shards_dir = target.join("shards");
    let mut shard_entries: Vec<_> = fs::read_dir(&shards_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    shard_entries.sort();
    let corrupted_shard = &shard_entries[0];
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .open(corrupted_shard)
        .unwrap();
    f.write_all(b"GARBAGE_HEADER_CORRUPT").unwrap();
    drop(f);

    // 3. Move manifest to legacy_v0
    let versions_dir = target.join("versions");
    let legacy_v0 = versions_dir.join("legacy_v0");
    fs::create_dir_all(&legacy_v0).unwrap();
    fs::rename(
        target.join("manifest.json"),
        legacy_v0.join("manifest.json"),
    )
    .unwrap();

    // 4. Run recovery: verification fails
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("failed verification"),
        "Unexpected error: {}",
        err
    );
    assert!(
        err.to_string()
            .contains("artifacts preserved safely without deletion"),
        "Unexpected error: {}",
        err
    );

    // 5. Assert legacy_v0 was NOT deleted!
    assert!(
        legacy_v0.exists(),
        "CRITICAL: legacy_v0 was deleted despite containing recovery data!"
    );
    assert!(
        legacy_v0.join("manifest.json").exists(),
        "manifest.json was destroyed!"
    );
}

// =========================================================================
// Test Group A: Atomic Recovery Against Partial File Copies
// =========================================================================

#[test]
fn test_recovery_partial_manifest_copy_interrupted() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("partial_manifest_dataset");

    // 1. Create authentic legacy flat dataset
    create_legacy_flat_dataset(&target, 4, 100).unwrap();

    // 2. Set up split state where manifest only exists in legacy_v0
    let versions_dir = target.join("versions");
    let legacy_v0 = versions_dir.join("legacy_v0");
    fs::create_dir_all(&legacy_v0).unwrap();
    fs::rename(
        target.join("manifest.json"),
        legacy_v0.join("manifest.json"),
    )
    .unwrap();
    let original_manifest_bytes = fs::read(legacy_v0.join("manifest.json")).unwrap();

    // 3. Inject fault: copy of manifest interrupted after partial output
    fault::set_fault_point(Some(FaultPoint::FailPartialCopyManifest));

    // 4. Run recovery: must fail cleanly
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailPartialCopyManifest"),
        "Unexpected error: {}",
        err
    );

    // 5. Assert source manifest is completely intact and root manifest was NOT partially populated
    assert_eq!(
        fs::read(legacy_v0.join("manifest.json")).unwrap(),
        original_manifest_bytes,
        "Source manifest was corrupted"
    );
    assert!(
        !target.join("manifest.json").exists(),
        "Destination manifest must not exist as partial completed artifact"
    );

    // 6. Clear fault and retry recovery: must complete automatically
    fault::set_fault_point(None);
    let recovered = recover_dataset(&target).expect("Recovery after fault clearance must succeed");
    assert_eq!(recovered, target.join("current"));

    // 7. Verify deep integrity and game retrieval
    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader.get_game(g100.metadata.id).unwrap().metadata.id,
        g100.metadata.id
    );

    // 8. Idempotency
    let rec2 = recover_dataset(&target).unwrap();
    assert_eq!(rec2, target.join("current"));
}

#[test]
fn test_recovery_partial_lookup_copy_interrupted() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("partial_lookup_dataset");

    create_legacy_flat_dataset(&target, 4, 100).unwrap();

    let versions_dir = target.join("versions");
    let legacy_v0 = versions_dir.join("legacy_v0");
    fs::create_dir_all(&legacy_v0).unwrap();
    fs::rename(
        target.join("manifest.json"),
        legacy_v0.join("manifest.json"),
    )
    .unwrap();
    let original_lookup_bytes = fs::read(target.join("games.idx")).unwrap();

    // Inject lookup partial copy failure
    fault::set_fault_point(Some(FaultPoint::FailPartialCopyLookup));

    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailPartialCopyLookup"),
        "Unexpected error: {}",
        err
    );

    // Assert source lookup is intact and legacy_v0 lookup was NOT masqueraded as completed
    assert_eq!(
        fs::read(target.join("games.idx")).unwrap(),
        original_lookup_bytes
    );
    assert!(
        !legacy_v0.join("games.idx").exists(),
        "Partial lookup destination must not masquerade as completed artifact"
    );

    fault::set_fault_point(None);
    let recovered = recover_dataset(&target).expect("Recovery after fault clearance must succeed");
    assert_eq!(recovered, target.join("current"));

    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);

    let rec2 = recover_dataset(&target).unwrap();
    assert_eq!(rec2, target.join("current"));
}

#[test]
fn test_recovery_partial_shard_copy_interrupted() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("partial_shard_dataset");

    create_legacy_flat_dataset(&target, 4, 100).unwrap();

    let versions_dir = target.join("versions");
    let legacy_v0 = versions_dir.join("legacy_v0");
    fs::create_dir_all(&legacy_v0).unwrap();
    fs::rename(
        target.join("manifest.json"),
        legacy_v0.join("manifest.json"),
    )
    .unwrap();

    let src_shard = target.join("shards").join("shard_0000.cgb3");
    let original_shard_bytes = fs::read(&src_shard).unwrap();

    // Inject shard partial copy failure
    fault::set_fault_point(Some(FaultPoint::FailPartialCopyShard));

    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailPartialCopyShard"),
        "Unexpected error: {}",
        err
    );

    assert_eq!(fs::read(&src_shard).unwrap(), original_shard_bytes);
    assert!(
        !legacy_v0.join("shards").join("shard_0000.cgb3").exists(),
        "Partial shard destination must not masquerade as completed artifact"
    );

    fault::set_fault_point(None);
    let recovered = recover_dataset(&target).expect("Recovery after fault clearance must succeed");
    assert_eq!(recovered, target.join("current"));

    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);

    let rec2 = recover_dataset(&target).unwrap();
    assert_eq!(rec2, target.join("current"));
}

#[test]
fn test_recovery_two_opposite_partial_copies_reconstruction() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("two_partial_dataset");

    // 1. Create a deterministic multi-shard dataset with 8 games (4 games/shard = 2 shards: shard_0000 & shard_0001)
    create_legacy_flat_dataset(&target, 8, 100).unwrap();

    let versions_dir = target.join("versions");
    let legacy_v0 = versions_dir.join("legacy_v0");
    let leg_shards = legacy_v0.join("shards");
    fs::create_dir_all(&leg_shards).unwrap();

    let root_shards = target.join("shards");

    // Distribute shards between root and legacy_v0:
    // Shard 0 exists correctly in root.
    // Shard 1 exists correctly in legacy_v0.
    fs::rename(
        root_shards.join("shard_0001.cgb3"),
        leg_shards.join("shard_0001.cgb3"),
    )
    .unwrap();
    fs::copy(
        target.join("manifest.json"),
        legacy_v0.join("manifest.json"),
    )
    .unwrap();

    let orig_shard0_bytes = fs::read(root_shards.join("shard_0000.cgb3")).unwrap();
    let orig_shard1_bytes = fs::read(leg_shards.join("shard_0001.cgb3")).unwrap();

    // 2. Simulate prior failed recoveries that left TRUNCATED / PARTIAL files at opposite destinations:
    // Destination in legacy_v0 for Shard 0: write 16 bytes of truncated data
    use std::io::Write;
    let mut f_part0 = fs::File::create(leg_shards.join("shard_0000.cgb3")).unwrap();
    f_part0.write_all(b"TRUNCATED_SHARD0").unwrap();
    drop(f_part0);

    // Destination in root for Shard 1: write 16 bytes of truncated data
    let mut f_part1 = fs::File::create(root_shards.join("shard_0001.cgb3")).unwrap();
    f_part1.write_all(b"TRUNCATED_SHARD1").unwrap();
    drop(f_part1);

    // Confirm that partial destinations exist on disk
    assert!(leg_shards.join("shard_0000.cgb3").exists());
    assert_ne!(
        fs::read(leg_shards.join("shard_0000.cgb3")).unwrap(),
        orig_shard0_bytes
    );
    assert!(root_shards.join("shard_0001.cgb3").exists());
    assert_ne!(
        fs::read(root_shards.join("shard_0001.cgb3")).unwrap(),
        orig_shard1_bytes
    );

    // 3. Run recovery: must detect partial destinations as invalid, reconstruct them from valid sources, and succeed!
    let recovered = recover_dataset(&target)
        .expect("Recovery must resolve two opposite partial-copy destinations automatically");
    assert_eq!(recovered, target.join("current"));

    // 4. Assert that both destinations now contain the complete, valid bytes
    assert_eq!(
        fs::read(leg_shards.join("shard_0000.cgb3")).unwrap(),
        orig_shard0_bytes,
        "Shard 0 in legacy_v0 was not properly reconstructed from root"
    );
    assert_eq!(
        fs::read(root_shards.join("shard_0001.cgb3")).unwrap(),
        orig_shard1_bytes,
        "Shard 1 in root was not properly reconstructed from legacy_v0"
    );

    // 5. Deep dataset verification must pass
    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);

    // 6. All 8 games (100..107 across both shards) remain fully retrievable
    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 8);
    for id_seed in 100..108 {
        let g = sample_game(id_seed, Collection::MastersOtb, "Carlsen", "Caruana");
        assert_eq!(
            reader.get_game(g.metadata.id).unwrap().metadata.id,
            g.metadata.id
        );
    }

    // 7. Repeated recovery produces the same valid result
    let rec2 = recover_dataset(&target).unwrap();
    assert_eq!(rec2, target.join("current"));
}

// =========================================================================
// Test Group B: Safe Backup Retention and Symlink Handling
// =========================================================================

#[test]
fn test_symlink_failure_after_backup_restores_cleanly() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("backup_symlink_dataset");

    create_legacy_flat_dataset(&target, 4, 100).unwrap();
    let orig_shards_bytes = fs::read(target.join("shards").join("shard_0000.cgb3")).unwrap();

    // Inject failure after root shards directory is moved to backup
    fault::set_fault_point(Some(FaultPoint::FailAfterBackupBeforeSymlink));

    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailAfterBackupBeforeSymlink"),
        "Unexpected error: {}",
        err
    );

    // The rollback must have restored the backup back to root 'shards'
    assert!(
        target.join("shards").is_dir(),
        "Root shards directory was not restored after symlink installation failure"
    );
    assert_eq!(
        fs::read(target.join("shards").join("shard_0000.cgb3")).unwrap(),
        orig_shards_bytes
    );

    // Clear fault and retry
    fault::set_fault_point(None);
    let recovered = recover_dataset(&target).unwrap();
    assert_eq!(recovered, target.join("current"));

    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
}

#[test]
fn test_symlink_replacement_failure_restores_backup() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("symlink_replacement_dataset");

    create_legacy_flat_dataset(&target, 4, 100).unwrap();
    let orig_shards_bytes = fs::read(target.join("shards").join("shard_0000.cgb3")).unwrap();

    // Inject failure during rename of symlink over root directory
    fault::set_fault_point(Some(FaultPoint::FailRenameSymlinkOverRoot));

    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailRenameSymlinkOverRoot"),
        "Unexpected error: {}",
        err
    );

    // Rollback restored root shards
    assert!(target.join("shards").is_dir());
    assert_eq!(
        fs::read(target.join("shards").join("shard_0000.cgb3")).unwrap(),
        orig_shards_bytes
    );

    fault::set_fault_point(None);
    let recovered = recover_dataset(&target).unwrap();
    assert_eq!(recovered, target.join("current"));

    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
}

#[test]
fn test_backup_restoration_failure_preserves_backup_and_recovers_on_retry() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("backup_restoration_dataset");

    create_legacy_flat_dataset(&target, 4, 100).unwrap();
    let orig_shards_bytes = fs::read(target.join("shards").join("shard_0000.cgb3")).unwrap();

    // Inject both symlink failure and backup restoration failure
    // First: fail rename symlink over root, and during rollback, fail restore backup!
    fault::set_fault_point(Some(FaultPoint::FailRestoreBackup));

    // Artificially trigger the failure scenario:
    // Move shards to .backup_shards_123_456
    let backup_dir = target.join(".backup_shards_123_456");
    fs::rename(target.join("shards"), &backup_dir).unwrap();

    // Run recovery: reconcile_and_recover_backups will attempt to restore the backup.
    // With FailRestoreBackup active, it must propagate the error and PRESERVE the backup!
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailRestoreBackup"),
        "Unexpected error: {}",
        err
    );

    // CRITICAL: The backup directory MUST NOT be deleted!
    assert!(
        backup_dir.exists(),
        "CRITICAL BUG: Backup directory was deleted when restoration failed!"
    );
    assert_eq!(
        fs::read(backup_dir.join("shard_0000.cgb3")).unwrap(),
        orig_shards_bytes
    );

    // Clear fault and retry recovery: recovery must restore the preserved backup and complete!
    fault::set_fault_point(None);
    let recovered =
        recover_dataset(&target).expect("Recovery must resolve preserved backup on retry");
    assert_eq!(recovered, target.join("current"));

    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);

    let rec2 = recover_dataset(&target).unwrap();
    assert_eq!(rec2, target.join("current"));
}

#[test]
fn test_live_reader_during_backup_symlink_replacement() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("live_reader_symlink_dataset");

    // 1. Create versioned published dataset
    create_dataset(&target, 4, 100).unwrap();

    // 2. Open live reader pinned to snapshot
    let mut reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);

    // 3. Delete or tamper with root convenience symlinks (e.g. replace root shards with a temp backup or broken link)
    let root_shards = target.join("shards");
    if root_shards.exists() || fs::symlink_metadata(&root_shards).is_ok() {
        let _ = fs::remove_file(&root_shards);
    }

    // 4. Reader continues retrieving games without any error because it is pinned to its canonicalized snapshot directory!
    let g100 = sample_game(100, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader.get_game(g100.metadata.id).unwrap().metadata.id,
        g100.metadata.id
    );

    // 5. Recovery re-installs root symlinks safely
    let recovered = recover_dataset(&target).unwrap();
    assert_eq!(recovered, target.join("current"));

    // 6. Reader still retrieves games flawlessly
    let g103 = sample_game(103, Collection::MastersOtb, "Carlsen", "Caruana");
    assert_eq!(
        reader.get_game(g103.metadata.id).unwrap().metadata.id,
        g103.metadata.id
    );
}

// =========================================================================
// Test Group C: Lossless & Redundancy-Verified Backup Reconciliation (Tests A–F)
// =========================================================================

#[test]
fn test_reconciliation_valid_backup_corrupted_root_file() {
    let tmp = tempdir().unwrap();
    let target = tmp
        .path()
        .join("test_reconciliation_valid_backup_corrupted_root");

    create_legacy_flat_dataset(&target, 4, 100).unwrap();
    let shard_file = target.join("shards").join("shard_0000.cgb3");
    let valid_shard_bytes = fs::read(&shard_file).unwrap();

    // Create backup directory containing the ONLY valid shard copy
    let backup_dir = target.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::copy(&shard_file, backup_dir.join("shard_0000.cgb3")).unwrap();

    // Corrupt root shard file with truncated garbage data
    fs::write(&shard_file, b"corrupted partial data garbage").unwrap();

    // Run recovery: must detect corruption in root, prefer valid candidate in backup,
    // and atomically reconstruct without data loss!
    let recovered =
        recover_dataset(&target).expect("Recovery must succeed by preferring valid backup shard");
    assert_eq!(recovered, target.join("current"));

    // Verify dataset integrity and deep block reading
    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);

    // Verify the corrupt shard was replaced with the valid one and backup was cleaned up after successful reconstruction
    assert!(
        !backup_dir.exists(),
        "Redundant backup directory should be cleaned up after safe reconstruction"
    );
    let restored_bytes = fs::read(
        target
            .join("current")
            .join("shards")
            .join("shard_0000.cgb3"),
    )
    .unwrap();
    assert_eq!(restored_bytes, valid_shard_bytes);
}

#[test]
fn test_reconciliation_both_files_valid_but_different_preserves_both_and_fails_closed() {
    let tmp = tempdir().unwrap();
    let target_a = tmp.path().join("dataset_a");
    let target_b = tmp.path().join("dataset_b");

    create_legacy_flat_dataset(&target_a, 4, 100).unwrap();
    create_legacy_flat_dataset(&target_b, 4, 500).unwrap();

    let shard_a_bytes = fs::read(target_a.join("shards").join("shard_0000.cgb3")).unwrap();
    let shard_b_bytes = fs::read(target_b.join("shards").join("shard_0000.cgb3")).unwrap();
    assert_ne!(shard_a_bytes, shard_b_bytes);

    // In target_a, create backup containing shard_0000 from dataset_b
    let backup_dir = target_a.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("shard_0000.cgb3"), &shard_b_bytes).unwrap();

    // Run recovery: should detect conflicting distinct valid files, preserve both, and fail closed
    let err = recover_dataset(&target_a).unwrap_err();
    assert!(
        err.to_string().contains("conflicting") || err.to_string().contains("Conflicting"),
        "Unexpected error: {}",
        err
    );

    // Both files and backup directory MUST BE PRESERVED safely
    assert!(
        backup_dir.exists(),
        "Backup directory must not be removed on conflict"
    );
    assert_eq!(
        fs::read(backup_dir.join("shard_0000.cgb3")).unwrap(),
        shard_b_bytes,
        "Backup artifact must remain intact"
    );
    assert_eq!(
        fs::read(target_a.join("shards").join("shard_0000.cgb3")).unwrap(),
        shard_a_bytes,
        "Root artifact must remain intact"
    );
}

#[test]
fn test_reconciliation_unknown_backup_contents_preserved_and_fails_closed() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("test_unknown_backup");

    create_dataset(&target, 4, 100).unwrap();
    let shard_file = target
        .join("current")
        .join("shards")
        .join("shard_0000.cgb3");
    let shard_bytes = fs::read(&shard_file).unwrap();

    // Create a backup directory with valid shard_0000 AND an extra unknown file
    let backup_dir = target.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("shard_0000.cgb3"), &shard_bytes).unwrap();
    let extra_content = b"extra unknown data that is unique";
    fs::write(backup_dir.join("extra.txt"), extra_content).unwrap();

    // Run recovery: backup must not be declared redundant merely because current is valid!
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("redundant")
            || err.to_string().contains("unique")
            || err.to_string().contains("preserved"),
        "Unexpected error: {}",
        err
    );

    // Verify backup directory and unknown file are preserved
    assert!(backup_dir.exists(), "Backup directory must be preserved");
    assert_eq!(
        fs::read(backup_dir.join("extra.txt")).unwrap(),
        extra_content
    );
    assert_eq!(
        fs::read(backup_dir.join("shard_0000.cgb3")).unwrap(),
        shard_bytes
    );
}

#[test]
fn test_reconciliation_symlink_points_to_wrong_target_preserves_backup() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("test_wrong_symlink");

    create_dataset(&target, 4, 100).unwrap();
    let shard_file = target
        .join("current")
        .join("shards")
        .join("shard_0000.cgb3");
    let shard_bytes = fs::read(&shard_file).unwrap();

    // Create a different existing directory
    let other_dir = target.join("other_directory");
    fs::create_dir_all(&other_dir).unwrap();
    fs::write(other_dir.join("some_other_file.bin"), b"other data").unwrap();

    // Point root symlink 'shards' to 'other_directory' (which exists, but is WRONG target)
    let root_shards = target.join("shards");
    if root_shards.exists() || fs::symlink_metadata(&root_shards).is_ok() {
        let _ = fs::remove_file(&root_shards);
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("other_directory", &root_shards).unwrap();

    // Create a backup containing recoverable shard data
    let backup_dir = target.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("shard_0000.cgb3"), &shard_bytes).unwrap();

    // Run recovery: existing symlink target must NOT authorize backup deletion!
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("expected active dataset artifact")
            || err.to_string().contains("does not reference")
            || err.to_string().contains("cannot be deleted"),
        "Unexpected error: {}",
        err
    );

    // Verify backup is preserved
    assert!(backup_dir.exists(), "Backup directory must not be deleted");
    assert_eq!(
        fs::read(backup_dir.join("shard_0000.cgb3")).unwrap(),
        shard_bytes
    );
}

#[test]
fn test_reconciliation_truly_redundant_backup_cleanup_succeeds() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("test_redundant_backup");

    create_dataset(&target, 4, 100).unwrap();
    let shard_file = target
        .join("current")
        .join("shards")
        .join("shard_0000.cgb3");
    let shard_bytes = fs::read(&shard_file).unwrap();

    // Create a backup directory containing exact identical copies and nothing else
    let backup_dir = target.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("shard_0000.cgb3"), &shard_bytes).unwrap();

    // Run recovery
    let recovered =
        recover_dataset(&target).expect("Recovery must succeed for truly redundant backup");
    assert_eq!(recovered, target.join("current"));

    // Redundant backup must be safely cleaned up
    assert!(
        !backup_dir.exists(),
        "Redundant backup directory should be removed"
    );

    // Dataset is verified and complete
    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);
}

#[test]
fn test_reconciliation_idempotency_and_correction_flow() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("test_idempotency");

    create_dataset(&target, 4, 100).unwrap();
    let shard_file = target
        .join("current")
        .join("shards")
        .join("shard_0000.cgb3");
    let shard_bytes = fs::read(&shard_file).unwrap();

    // Create backup with unknown file
    let backup_dir = target.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("shard_0000.cgb3"), &shard_bytes).unwrap();
    let extra_file = backup_dir.join("extra.txt");
    fs::write(&extra_file, b"unique data").unwrap();

    // 1. First recovery fails and preserves
    let err1 = recover_dataset(&target).unwrap_err();
    assert!(backup_dir.exists());
    assert!(extra_file.exists());

    // 2. Second recovery without fix produces identical failure and still preserves (idempotent failure)
    let err2 = recover_dataset(&target).unwrap_err();
    assert_eq!(err1.to_string(), err2.to_string());
    assert!(backup_dir.exists());
    assert!(extra_file.exists());

    // 3. User corrects the problem: moves extra.txt to a safe location
    let safe_location = tmp.path().join("rescued_extra.txt");
    fs::rename(&extra_file, &safe_location).unwrap();

    // 4. Recovery retry now succeeds!
    let recovered =
        recover_dataset(&target).expect("Recovery must succeed after fixing the problem");
    assert_eq!(recovered, target.join("current"));
    assert!(
        !backup_dir.exists(),
        "Backup should be removed once redundant"
    );

    // 5. Subsequent recovery on already-recovered dataset succeeds idempotently
    let recovered2 = recover_dataset(&target).expect("Repeated recovery must succeed");
    assert_eq!(recovered2, target.join("current"));

    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
}

// =========================================================================
// Test Group D: Fail-Closed Directory Enumeration (Tests A–C)
// =========================================================================

#[test]
fn test_enumeration_failure_during_redundancy_check_preserves_backup_and_fails_closed() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("test_redundancy_enum_fail");

    create_dataset(&target, 4, 100).unwrap();
    let shard_file = target
        .join("current")
        .join("shards")
        .join("shard_0000.cgb3");
    let shard_bytes = fs::read(&shard_file).unwrap();

    // Create a backup directory containing shard_0000.cgb3 AND a unique file
    let backup_dir = target.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("shard_0000.cgb3"), &shard_bytes).unwrap();
    let unique_content = b"potentially unique data";
    fs::write(backup_dir.join("unique_report.bin"), unique_content).unwrap();

    // Inject enumeration failure during redundancy verification
    fault::set_fault_point(Some(FaultPoint::FailRedundancyCheckEnumeration));

    // Run recovery: must fail closed on enumeration failure!
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailRedundancyCheckEnumeration")
            || err.to_string().contains("Failed to read"),
        "Unexpected error: {}",
        err
    );

    // CRITICAL: Backup directory and ALL its contents MUST NOT be deleted!
    assert!(
        backup_dir.exists(),
        "CRITICAL BUG: Backup directory was deleted after failed enumeration!"
    );
    assert_eq!(
        fs::read(backup_dir.join("unique_report.bin")).unwrap(),
        unique_content,
        "Unique file in backup must remain intact!"
    );
    assert_eq!(
        fs::read(backup_dir.join("shard_0000.cgb3")).unwrap(),
        shard_bytes
    );
}

#[test]
fn test_enumeration_failure_while_checking_remaining_backup_files_preserves_backup_and_fails_closed(
) {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("test_remaining_enum_fail");

    create_legacy_flat_dataset(&target, 4, 100).unwrap();
    let shard_file = target.join("shards").join("shard_0000.cgb3");
    let shard_bytes = fs::read(&shard_file).unwrap();

    // Create a backup directory containing identical shard_0000.cgb3
    let backup_dir = target.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("shard_0000.cgb3"), &shard_bytes).unwrap();

    // Inject failure during remaining-entries enumeration check
    fault::set_fault_point(Some(FaultPoint::FailRemainingEntriesEnumeration));

    // Run recovery: reconciliation of matching file proceeds, but remaining-entries verification fails!
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailRemainingEntriesEnumeration")
            || err.to_string().contains("remaining"),
        "Unexpected error: {}",
        err
    );

    // CRITICAL: Backup directory MUST be preserved on enumeration failure!
    assert!(
        backup_dir.exists(),
        "CRITICAL BUG: Backup directory was deleted when remaining-entries enumeration failed!"
    );
}

#[test]
fn test_enumeration_failure_recovery_retry_succeeds_and_is_idempotent() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("test_retry_enum_success");

    create_legacy_flat_dataset(&target, 4, 100).unwrap();
    let shard_file = target.join("shards").join("shard_0000.cgb3");
    let shard_bytes = fs::read(&shard_file).unwrap();

    // Create a backup directory containing identical shard_0000.cgb3
    let backup_dir = target.join(".backup_shards_123_456");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("shard_0000.cgb3"), &shard_bytes).unwrap();

    // 1. Inject failure during remaining-entries check
    fault::set_fault_point(Some(FaultPoint::FailRemainingEntriesEnumeration));
    let err = recover_dataset(&target).unwrap_err();
    assert!(
        err.to_string().contains("FailRemainingEntriesEnumeration")
            || err.to_string().contains("remaining"),
        "Unexpected error: {}",
        err
    );
    assert!(backup_dir.exists());

    // 2. Clear injected enumeration failure
    fault::set_fault_point(None);

    // 3. Retry recovery: must complete successfully when artifacts are safely reconcilable
    let recovered = recover_dataset(&target)
        .expect("Recovery must succeed once enumeration failure is cleared");
    assert_eq!(recovered, target.join("current"));

    // Redundant backup directory safely removed
    assert!(
        !backup_dir.exists(),
        "Redundant backup directory should be removed after successful recovery"
    );

    // Deep verification passes
    let v_rep = verify_dataset(&target, true).unwrap();
    assert!(v_rep.deep_verification_passed);
    let reader = DatasetReader::open(&target).unwrap();
    assert_eq!(reader.total_games(), 4);

    // 4. Subsequent recovery call remains idempotent
    let recovered2 =
        recover_dataset(&target).expect("Subsequent recovery must succeed idempotently");
    assert_eq!(recovered2, target.join("current"));
    let v_rep2 = verify_dataset(&target, true).unwrap();
    assert!(v_rep2.deep_verification_passed);
}
