use codb_core::{
    compute_stable_game_id, Collection, CompactMove, GameMetadata, GameResult,
    IngestionCompletionStatus, IngestionStats, ParsedGame,
};
use codb_storage::fault::{self, FaultPoint};
use codb_storage::{
    cleanup_old_versions, recover_dataset, resolve_dataset_snapshot, verify_dataset, DatasetReader,
    DatasetWriter, DatasetWriterConfig, SourceArchiveMetadata, StorageError,
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
    let corrupt_version = target.join("versions").join("codb-corrupt-newer");
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
    if std::env::var("CODB_TEST_SUBPROCESS_CRASH").as_deref() == Ok("1") {
        let child_target =
            std::path::PathBuf::from(std::env::var("CODB_TEST_CHILD_TARGET").unwrap());
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
        .env("CODB_TEST_SUBPROCESS_CRASH", "1")
        .env("CODB_TEST_CHILD_TARGET", &target)
        .env("CODB_CRASH_AT", "BeforeCurrentSymlinkSwitch")
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
