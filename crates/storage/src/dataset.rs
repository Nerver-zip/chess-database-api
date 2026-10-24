use codb_core::{Collection, IngestionStats, ParsedGame};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use crate::format::{export_to_pgn, GameBlock, StorageError};
use crate::lookup::{GameLookupReader, GameLookupWriter};
use crate::shard::{BlockDescriptor, ShardReader, ShardWriter};

pub mod fault {
    use std::cell::RefCell;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FaultPoint {
        BeforeStagingVerify,
        AfterStagingVerifyBeforeVersionMove,
        BeforeCurrentSymlinkSwitch,
        AfterCurrentSymlinkSwitch,
        FailSyncDirContents,
        FailSyncDirItself,
        // Legacy migration interruption points
        BeforeLegacyMigrationPrep,
        AfterLegacyMigrationDestCreated,
        AfterLegacyFirstFileTransfer,
        BetweenLegacyFileTransfers,
        AfterLegacyCandidateVerify,
        BeforeLegacySymlinkSwitch,
        AfterLegacySymlinkSwitch,
        DuringLegacyCleanup,
        // Recovery-specific fault injection points
        FailRecoveryCopyManifest,
        FailRecoveryCopyLookup,
        FailRecoveryCopyShard,
        FailRecoveryCreateDestDir,
        FailRecoverySyncReconstructed,
        FailCreateRootSymlink,
        // Intermediate failure points for partial copy recovery
        FailPartialCopyManifest,
        FailPartialCopyLookup,
        FailPartialCopyShard,
        // Intermediate failure points for backup and symlink handling
        FailAfterBackupBeforeSymlink,
        FailRenameSymlinkOverRoot,
        FailRestoreBackup,
        // Directory enumeration fail-closed fault points
        FailRedundancyCheckEnumeration,
        FailRemainingEntriesEnumeration,
        FailReconcileEntriesEnumeration,
    }

    thread_local! {
        static ACTIVE_FAULT: RefCell<Option<FaultPoint>> = const { RefCell::new(None) };
    }

    pub fn set_fault_point(point: Option<FaultPoint>) {
        ACTIVE_FAULT.with(|f| *f.borrow_mut() = point);
    }

    pub fn is_fault_active(point: FaultPoint) -> bool {
        ACTIVE_FAULT.with(|f| *f.borrow() == Some(point))
    }

    pub fn trigger_if(point: FaultPoint) -> Result<(), crate::format::StorageError> {
        let should_fail = ACTIVE_FAULT.with(|f| *f.borrow() == Some(point));
        if should_fail {
            return Err(crate::format::StorageError::Io(std::io::Error::other(
                format!("Injected publication fault at {:?}", point),
            )));
        }
        if let Ok(val) = std::env::var("CODB_CRASH_AT") {
            let match_str = match point {
                FaultPoint::BeforeStagingVerify => "BeforeStagingVerify",
                FaultPoint::AfterStagingVerifyBeforeVersionMove => {
                    "AfterStagingVerifyBeforeVersionMove"
                }
                FaultPoint::BeforeCurrentSymlinkSwitch => "BeforeCurrentSymlinkSwitch",
                FaultPoint::AfterCurrentSymlinkSwitch => "AfterCurrentSymlinkSwitch",
                FaultPoint::FailSyncDirContents => "FailSyncDirContents",
                FaultPoint::FailSyncDirItself => "FailSyncDirItself",
                FaultPoint::BeforeLegacyMigrationPrep => "BeforeLegacyMigrationPrep",
                FaultPoint::AfterLegacyMigrationDestCreated => "AfterLegacyMigrationDestCreated",
                FaultPoint::AfterLegacyFirstFileTransfer => "AfterLegacyFirstFileTransfer",
                FaultPoint::BetweenLegacyFileTransfers => "BetweenLegacyFileTransfers",
                FaultPoint::AfterLegacyCandidateVerify => "AfterLegacyCandidateVerify",
                FaultPoint::BeforeLegacySymlinkSwitch => "BeforeLegacySymlinkSwitch",
                FaultPoint::AfterLegacySymlinkSwitch => "AfterLegacySymlinkSwitch",
                FaultPoint::DuringLegacyCleanup => "DuringLegacyCleanup",
                FaultPoint::FailRecoveryCopyManifest => "FailRecoveryCopyManifest",
                FaultPoint::FailRecoveryCopyLookup => "FailRecoveryCopyLookup",
                FaultPoint::FailRecoveryCopyShard => "FailRecoveryCopyShard",
                FaultPoint::FailRecoveryCreateDestDir => "FailRecoveryCreateDestDir",
                FaultPoint::FailRecoverySyncReconstructed => "FailRecoverySyncReconstructed",
                FaultPoint::FailCreateRootSymlink => "FailCreateRootSymlink",
                FaultPoint::FailPartialCopyManifest => "FailPartialCopyManifest",
                FaultPoint::FailPartialCopyLookup => "FailPartialCopyLookup",
                FaultPoint::FailPartialCopyShard => "FailPartialCopyShard",
                FaultPoint::FailAfterBackupBeforeSymlink => "FailAfterBackupBeforeSymlink",
                FaultPoint::FailRenameSymlinkOverRoot => "FailRenameSymlinkOverRoot",
                FaultPoint::FailRestoreBackup => "FailRestoreBackup",
                FaultPoint::FailRedundancyCheckEnumeration => "FailRedundancyCheckEnumeration",
                FaultPoint::FailRemainingEntriesEnumeration => "FailRemainingEntriesEnumeration",
                FaultPoint::FailReconcileEntriesEnumeration => "FailReconcileEntriesEnumeration",
            };
            if val == match_str {
                std::process::exit(137);
            }
        }
        Ok(())
    }
}

pub const DEFAULT_BLOCK_CAPACITY: usize = 512;
pub const DEFAULT_MAX_GAMES_PER_SHARD: usize = 500_000;
pub const DEFAULT_ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceArchiveMetadata {
    pub filename: String,
    pub url: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: u64,
    pub license: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_verified: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurationPolicyMetadata {
    pub min_rating_both: u16,
    pub min_rating_one: u16,
    pub min_rating_other: u16,
    pub min_plies: usize,
    pub extra_criteria: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShardMetadata {
    pub shard_id: u16,
    pub filename: String,
    pub file_size_bytes: u64,
    pub sha256: String,
    pub block_count: u32,
    pub game_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileChecksumMetadata {
    pub filename: String,
    pub file_size_bytes: u64,
    pub sha256: String,
    pub entry_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatasetManifest {
    pub manifest_version: u32,
    pub dataset_build_id: String,
    pub collection: Collection,
    pub build_timestamp: String,
    pub format_version: String,
    pub schema_version: String,
    pub source_archives: Vec<SourceArchiveMetadata>,
    pub curation_policy: CurationPolicyMetadata,
    pub deduplication_policy_version: String,
    pub accounting: IngestionStats,
    pub total_games: u64,
    pub shards: Vec<ShardMetadata>,
    pub lookup_table: FileChecksumMetadata,
    pub total_physical_bytes: u64,
    pub is_complete: bool,
}

pub struct DatasetWriterConfig {
    pub collection: Collection,
    pub block_capacity: usize,
    pub max_games_per_shard: usize,
    pub zstd_level: i32,
    pub curation_policy: CurationPolicyMetadata,
    pub deduplication_policy_version: String,
}

impl Default for DatasetWriterConfig {
    fn default() -> Self {
        DatasetWriterConfig {
            collection: Collection::MastersOtb,
            block_capacity: DEFAULT_BLOCK_CAPACITY,
            max_games_per_shard: DEFAULT_MAX_GAMES_PER_SHARD,
            zstd_level: DEFAULT_ZSTD_LEVEL,
            curation_policy: CurationPolicyMetadata {
                min_rating_both: 2200,
                min_rating_one: 2400,
                min_rating_other: 2000,
                min_plies: 4,
                extra_criteria: "Historical exception pre-1970".into(),
            },
            deduplication_policy_version: "v2-verified-prefix".into(),
        }
    }
}

pub struct StagingCatalog {
    conn: rusqlite::Connection,
    db_path: PathBuf,
}

impl StagingCatalog {
    pub fn create(path: PathBuf) -> Result<Self, StorageError> {
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             PRAGMA journal_mode = WAL;
             PRAGMA temp_store = FILE;
             PRAGMA cache_size = -4096;
             CREATE TABLE active_games (
                 game_id INTEGER PRIMARY KEY,
                 shard_id INTEGER NOT NULL,
                 block_index INTEGER NOT NULL,
                 game_index INTEGER NOT NULL
             );
             CREATE TABLE tombstones (
                 shard_id INTEGER NOT NULL,
                 block_index INTEGER NOT NULL,
                 game_index INTEGER NOT NULL,
                 PRIMARY KEY (shard_id, block_index, game_index)
             );",
        )?;
        Ok(StagingCatalog {
            conn,
            db_path: path,
        })
    }

    pub fn insert_entry(
        &mut self,
        game_id: u64,
        shard_id: u16,
        block_index: u32,
        game_index: u16,
    ) -> Result<(), StorageError> {
        let res = self.conn.execute(
            "INSERT INTO active_games (game_id, shard_id, block_index, game_index) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![game_id as i64, shard_id, block_index, game_index],
        );
        match res {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ffi::ErrorCode::ConstraintViolation =>
            {
                Err(StorageError::Integrity(format!(
                    "Game ID collision in dataset: stable ID {} already exists for an active game",
                    game_id
                )))
            }
            Err(e) => Err(StorageError::Database(e.to_string())),
        }
    }

    pub fn insert_locations(
        &mut self,
        shard_id: u16,
        block_index: u32,
        games: &[ParsedGame],
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        for (idx, game) in games.iter().enumerate() {
            let res = tx.execute(
                "INSERT INTO active_games (game_id, shard_id, block_index, game_index) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![game.metadata.id as i64, shard_id, block_index, idx as u16],
            );
            match res {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ffi::ErrorCode::ConstraintViolation =>
                {
                    return Err(StorageError::Integrity(format!(
                        "Game ID collision in dataset: stable ID {} already exists for an active game",
                        game.metadata.id
                    )));
                }
                Err(e) => return Err(StorageError::Database(e.to_string())),
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mark_tombstone(&mut self, game_id: u64) -> Result<bool, StorageError> {
        let tx = self.conn.transaction()?;
        let loc: Option<(u16, u32, u16)> = match tx.query_row(
            "SELECT shard_id, block_index, game_index FROM active_games WHERE game_id = ?1",
            [game_id as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ) {
            Ok(tuple) => Some(tuple),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(StorageError::Database(e.to_string())),
        };

        if let Some((s_id, b_idx, g_idx)) = loc {
            tx.execute(
                "DELETE FROM active_games WHERE game_id = ?1",
                [game_id as i64],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO tombstones (shard_id, block_index, game_index) VALUES (?1, ?2, ?3)",
                rusqlite::params![s_id, b_idx, g_idx],
            )?;
            tx.commit()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn has_tombstones(&self) -> Result<bool, StorageError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tombstones", [], |r| r.get(0))?;
        Ok(count > 0)
    }

    pub fn is_tombstoned(
        &self,
        shard_id: u16,
        block_index: u32,
        game_index: u16,
    ) -> Result<bool, StorageError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tombstones WHERE shard_id = ?1 AND block_index = ?2 AND game_index = ?3",
            rusqlite::params![shard_id, block_index, game_index],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn tombstoned_shards(&self) -> Result<Vec<u16>, StorageError> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT shard_id FROM tombstones ORDER BY shard_id ASC")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        let mut shards = Vec::new();
        for r in rows {
            shards.push(r?);
        }
        Ok(shards)
    }

    pub fn clear_tombstones_for_shard(&mut self, shard_id: u16) -> Result<(), StorageError> {
        self.conn
            .execute("DELETE FROM tombstones WHERE shard_id = ?1", [shard_id])?;
        Ok(())
    }

    pub fn update_game_location(
        &mut self,
        game_id: u64,
        shard_id: u16,
        block_index: u32,
        game_index: u16,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE active_games SET shard_id = ?1, block_index = ?2, game_index = ?3 WHERE game_id = ?4",
            rusqlite::params![shard_id, block_index, game_index, game_id as i64],
        )?;
        Ok(())
    }

    pub fn count_active_games(&self) -> Result<u64, StorageError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM active_games", [], |r| r.get(0))?;
        Ok(count as u64)
    }

    pub fn write_lookup_file(
        &self,
        dest_path: PathBuf,
        collection: Collection,
    ) -> Result<(PathBuf, u64, String), StorageError> {
        let count = self.count_active_games()?;
        let mut pos_stmt = self.conn.prepare(
            "SELECT game_id, shard_id, block_index, game_index FROM active_games WHERE game_id >= 0 ORDER BY game_id ASC",
        )?;
        let mut neg_stmt = self.conn.prepare(
            "SELECT game_id, shard_id, block_index, game_index FROM active_games WHERE game_id < 0 ORDER BY game_id ASC",
        )?;

        let map_row = |r: &rusqlite::Row| {
            let gid: i64 = r.get(0)?;
            let sid: u16 = r.get(1)?;
            let bidx: u32 = r.get(2)?;
            let gidx: u16 = r.get(3)?;
            Ok(crate::lookup::GameLookupEntry::new(
                gid as u64, sid, bidx, gidx,
            ))
        };

        let pos_rows = pos_stmt.query_map([], map_row)?;
        let neg_rows = neg_stmt.query_map([], map_row)?;

        let entry_iter = pos_rows
            .chain(neg_rows)
            .map(|res| res.map_err(|e| StorageError::Database(e.to_string())));

        GameLookupWriter::write_from_iter(dest_path, collection, count, entry_iter)
    }

    pub fn explain_lookup_query_plans(&self) -> Result<Vec<String>, StorageError> {
        let mut plans = Vec::new();
        for q in [
            "EXPLAIN QUERY PLAN SELECT game_id, shard_id, block_index, game_index FROM active_games WHERE game_id >= 0 ORDER BY game_id ASC",
            "EXPLAIN QUERY PLAN SELECT game_id, shard_id, block_index, game_index FROM active_games WHERE game_id < 0 ORDER BY game_id ASC",
        ] {
            let mut stmt = self.conn.prepare(q)?;
            let rows = stmt.query_map([], |r| {
                let detail: String = r.get(3)?;
                Ok(detail)
            })?;
            for r in rows {
                plans.push(r?);
            }
        }
        Ok(plans)
    }

    pub fn destroy(self) {
        let path = self.db_path.clone();
        drop(self.conn);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = fs::remove_file(path.with_extension("sqlite-shm"));
    }
}

/// Persistent, bounded-memory dataset builder.
pub struct DatasetWriter {
    target_dir: PathBuf,
    staging_dir: PathBuf,
    config: DatasetWriterConfig,
    current_shard_idx: u16,
    active_shard: Option<ShardWriter>,
    completed_shards: Vec<(PathBuf, Vec<BlockDescriptor>, u64, u16)>,
    current_block: GameBlock,
    catalog: StagingCatalog,
    total_retained_games: u64,
}

impl DatasetWriter {
    pub fn new(target_dir: PathBuf, config: DatasetWriterConfig) -> Result<Self, StorageError> {
        if let Some(parent) = target_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(&target_dir)?;
        let versions_dir = target_dir.join("versions");
        fs::create_dir_all(&versions_dir)?;

        // If target_dir has a legacy layout without versions/current, migrate it
        let root_manifest = target_dir.join("manifest.json");
        let is_flat_legacy = root_manifest.is_file()
            && !fs::symlink_metadata(&root_manifest)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            && !target_dir.join("current").exists();
        if is_flat_legacy {
            migrate_legacy_dataset(&target_dir)?;
        }

        // Clean up any stale staging directories or temporary symlinks from previously aborted runs
        if let Ok(entries) = fs::read_dir(&target_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(".staging_") {
                    let _ = fs::remove_dir_all(entry.path());
                } else if name_str.starts_with(".current_tmp") || name_str.starts_with(".tmp_link_")
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        if let Ok(entries) = fs::read_dir(&versions_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(".staging_") {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }

        let staging_id = format!(
            "build_{}_{}_{}",
            config.collection,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let staging_dir = target_dir.join(format!(".staging_{}", staging_id));

        if staging_dir.exists() {
            fs::remove_dir_all(&staging_dir)?;
        }
        fs::create_dir_all(&staging_dir)?;
        fs::create_dir_all(staging_dir.join("shards"))?;

        let catalog_path = staging_dir.join("staging_catalog.sqlite");
        let catalog = StagingCatalog::create(catalog_path)?;

        let collection = config.collection;
        Ok(DatasetWriter {
            target_dir,
            staging_dir,
            config,
            current_shard_idx: 0,
            active_shard: None,
            completed_shards: Vec::new(),
            current_block: GameBlock::new(collection),
            catalog,
            total_retained_games: 0,
        })
    }

    fn ensure_active_shard(&mut self) -> Result<&mut ShardWriter, StorageError> {
        if self.active_shard.is_none() {
            let shard_filename = format!("shard_{:04}.cgb3", self.current_shard_idx);
            let shard_path = self.staging_dir.join("shards").join(shard_filename);
            let writer =
                ShardWriter::create(shard_path, self.current_shard_idx, self.config.collection)?;
            self.active_shard = Some(writer);
        }
        Ok(self.active_shard.as_mut().unwrap())
    }

    /// Flushes the active in-memory block to the active shard.
    pub fn flush_block(&mut self) -> Result<(), StorageError> {
        if self.current_block.games.is_empty() {
            return Ok(());
        }

        let game_count = self.current_block.games.len() as u32;
        let first_game_id = self.current_block.games[0].metadata.id;
        let (compressed_payload, uncompressed_size) = self
            .current_block
            .compress_with_stats(self.config.zstd_level)?;

        let shard = self.ensure_active_shard()?;
        let shard_id = shard.shard_id();
        let block_index = shard.append_block(
            &compressed_payload,
            game_count,
            first_game_id,
            uncompressed_size as u32,
        )?;

        // Record location for all games in this block into disk-backed catalog
        self.catalog
            .insert_locations(shard_id, block_index, &self.current_block.games)?;

        self.current_block.games.clear();

        // Check if shard rotation is needed
        if let Some(shard) = &self.active_shard {
            if shard.total_games() as usize >= self.config.max_games_per_shard {
                self.rotate_shard()?;
            }
        }

        Ok(())
    }

    fn rotate_shard(&mut self) -> Result<(), StorageError> {
        if let Some(shard) = self.active_shard.take() {
            let shard_id = shard.shard_id();
            let (path, descriptors, total_size) = shard.finish()?;
            self.completed_shards
                .push((path, descriptors, total_size, shard_id));
            self.current_shard_idx += 1;
        }
        Ok(())
    }

    /// Ingests an accepted canonical game into the persistent pipeline.
    pub fn push_game(&mut self, game: ParsedGame) -> Result<(), StorageError> {
        self.total_retained_games += 1;
        self.current_block.games.push(game);

        if self.current_block.games.len() >= self.config.block_capacity {
            self.flush_block()?;
        }
        Ok(())
    }

    /// Handles supersession of a prior game:
    /// If the prior game is in the active uncommitted block buffer, replaces it in-place.
    /// If the prior game was already committed to an on-disk shard block, records the old location as tombstoned.
    pub fn handle_superseded_game(
        &mut self,
        old_id: u64,
        new_game: ParsedGame,
    ) -> Result<(), StorageError> {
        // 1. Check if old game is in the current uncommitted block buffer
        if let Some(pos) = self
            .current_block
            .games
            .iter()
            .position(|g| g.metadata.id == old_id)
        {
            self.current_block.games[pos] = new_game;
            return Ok(());
        }

        // 2. Old game was already written to disk: mark tombstone in catalog
        self.catalog.mark_tombstone(old_id)?;

        // Push new game into pipeline (replaces prior game logically)
        self.current_block.games.push(new_game);
        if self.current_block.games.len() >= self.config.block_capacity {
            self.flush_block()?;
        }

        Ok(())
    }

    /// Finalizes the dataset build, writing the global lookup index, verifying integrity,
    /// generating manifest, and atomically publishing to `target_dir`.
    pub fn finalize(
        mut self,
        accounting: IngestionStats,
        sources: Vec<SourceArchiveMetadata>,
    ) -> Result<DatasetManifest, StorageError> {
        // Flush remaining in-memory block
        self.flush_block()?;

        // Close last active shard
        self.rotate_shard()?;

        // If any tombstones exist, perform block compaction
        if self.catalog.has_tombstones()? {
            self.compact_tombstones()?;
        }

        let total_retained = self.catalog.count_active_games()?;

        // Write global game lookup index (games.idx) via streaming from catalog
        let lookup_path = self.staging_dir.join("games.idx");
        let (_written_lookup_path, lookup_size_bytes, _internal_sha) = self
            .catalog
            .write_lookup_file(lookup_path.clone(), self.config.collection)?;
        let lookup_sha256 = compute_file_sha256(&lookup_path)?;

        // Remove staging catalog SQLite database files
        self.catalog.destroy();

        // Compute checksums for all finalized shards
        let mut shards_meta = Vec::new();
        let mut total_physical_bytes = lookup_size_bytes;

        for (shard_path, descriptors, size_bytes, shard_id) in &self.completed_shards {
            let sha256 = compute_file_sha256(shard_path)?;
            let game_count: u64 = descriptors.iter().map(|d| d.game_count as u64).sum();
            total_physical_bytes += size_bytes;

            shards_meta.push(ShardMetadata {
                shard_id: *shard_id,
                filename: shard_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                file_size_bytes: *size_bytes,
                sha256,
                block_count: descriptors.len() as u32,
                game_count,
            });
        }

        // Verify accounting invariants
        if !accounting.is_reconciled() {
            return Err(StorageError::Encoding(
                "Accounting categories do not reconcile to total_encountered".into(),
            ));
        }
        if !accounting.verify_retained_count(total_retained as usize) {
            return Err(StorageError::Encoding(format!(
                "Retained games count mismatch: expected {} from accepted_unique, got {}",
                accounting.accepted_unique, total_retained
            )));
        }

        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);

        let manifest = DatasetManifest {
            manifest_version: 1,
            dataset_build_id: format!(
                "codb-{}-{}-{}",
                self.config.collection,
                now_nanos,
                std::process::id()
            ),
            collection: self.config.collection,
            build_timestamp: chrono_iso8601_now(),
            format_version: "CGB3/v1".into(),
            schema_version: "1.0.0".into(),
            source_archives: sources,
            curation_policy: self.config.curation_policy,
            deduplication_policy_version: self.config.deduplication_policy_version,
            accounting,
            total_games: total_retained,
            shards: shards_meta,
            lookup_table: FileChecksumMetadata {
                filename: "games.idx".into(),
                file_size_bytes: lookup_size_bytes,
                sha256: lookup_sha256,
                entry_count: total_retained,
            },
            total_physical_bytes,
            is_complete: true,
        };

        // Write manifest.json in staging directory
        let manifest_path = self.staging_dir.join("manifest.json");
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| StorageError::Encoding(e.to_string()))?;
        fs::write(&manifest_path, manifest_json)?;

        // Synchronize all files in staging directory before verification and publication
        fault::trigger_if(fault::FaultPoint::FailSyncDirContents)?;
        sync_dir_contents(&self.staging_dir)?;

        // Self-verify staging build before publication
        fault::trigger_if(fault::FaultPoint::BeforeStagingVerify)?;
        verify_dataset(&self.staging_dir, true)?;
        fault::trigger_if(fault::FaultPoint::AfterStagingVerifyBeforeVersionMove)?;

        // Safe publication protocol:
        // Move verified staging build to target_dir/versions/<dataset_build_id>
        let versions_dir = self.target_dir.join("versions");
        fs::create_dir_all(&versions_dir)?;
        let version_dir = versions_dir.join(&manifest.dataset_build_id);
        if version_dir.exists() {
            fs::remove_dir_all(&version_dir)?;
        }
        fs::rename(&self.staging_dir, &version_dir)?;

        sync_dir_contents(&version_dir)?;
        sync_dir_itself(&versions_dir)?;

        // Atomic active-version transition
        fault::trigger_if(fault::FaultPoint::BeforeCurrentSymlinkSwitch)?;
        #[cfg(unix)]
        {
            let tmp_link = self
                .target_dir
                .join(format!(".current_tmp_{}", std::process::id()));
            let _ = fs::remove_file(&tmp_link);
            symlink(
                Path::new("versions").join(&manifest.dataset_build_id),
                &tmp_link,
            )?;
            fs::rename(&tmp_link, self.target_dir.join("current"))?;
        }

        fault::trigger_if(fault::FaultPoint::AfterCurrentSymlinkSwitch)?;

        // Ensure convenience root symlinks
        ensure_root_symlinks(&self.target_dir)?;

        // Fsync target directory itself so link changes are durable on disk
        fault::trigger_if(fault::FaultPoint::FailSyncDirItself)?;
        sync_dir_itself(&self.target_dir)?;

        Ok(manifest)
    }

    /// Compaction pass: rewrites blocks containing tombstoned games to guarantee
    /// 100% dense, active games with zero superseded records in the finalized dataset.
    /// Operates in streaming O(1) memory by rewriting block-by-block.
    fn compact_tombstones(&mut self) -> Result<(), StorageError> {
        let tombstoned_shards = self.catalog.tombstoned_shards()?;
        if tombstoned_shards.is_empty() {
            return Ok(());
        }

        let mut new_completed_shards = Vec::new();

        for (shard_path, descriptors, size_bytes, shard_id) in self.completed_shards.drain(..) {
            if !tombstoned_shards.contains(&shard_id) {
                new_completed_shards.push((shard_path, descriptors, size_bytes, shard_id));
            } else {
                // Stream-compact this shard block-by-block without loading all games into memory
                let mut reader = ShardReader::open(&shard_path)?;
                let temp_compact_path = shard_path.with_extension("compact");
                let mut compact_writer = ShardWriter::create(
                    temp_compact_path.clone(),
                    shard_id,
                    self.config.collection,
                )?;

                let mut compact_block = GameBlock::new(self.config.collection);

                for b_idx in 0..reader.block_count() {
                    let block_games = reader.decompress_block(b_idx)?;
                    for (g_idx, game) in block_games.into_iter().enumerate() {
                        if !self.catalog.is_tombstoned(shard_id, b_idx, g_idx as u16)? {
                            compact_block.games.push(game);
                            if compact_block.games.len() >= self.config.block_capacity {
                                let (payload, uncompressed_size) =
                                    compact_block.compress_with_stats(self.config.zstd_level)?;
                                let game_count = compact_block.games.len() as u32;
                                let first_id = compact_block.games[0].metadata.id;
                                let new_b_idx = compact_writer.append_block(
                                    &payload,
                                    game_count,
                                    first_id,
                                    uncompressed_size as u32,
                                )?;
                                for (new_g_idx, g) in compact_block.games.iter().enumerate() {
                                    self.catalog.update_game_location(
                                        g.metadata.id,
                                        shard_id,
                                        new_b_idx,
                                        new_g_idx as u16,
                                    )?;
                                }
                                compact_block.games.clear();
                            }
                        }
                    }
                }

                // Flush remaining games in compact_block
                if !compact_block.games.is_empty() {
                    let (payload, uncompressed_size) =
                        compact_block.compress_with_stats(self.config.zstd_level)?;
                    let game_count = compact_block.games.len() as u32;
                    let first_id = compact_block.games[0].metadata.id;
                    let new_b_idx = compact_writer.append_block(
                        &payload,
                        game_count,
                        first_id,
                        uncompressed_size as u32,
                    )?;
                    for (new_g_idx, g) in compact_block.games.iter().enumerate() {
                        self.catalog.update_game_location(
                            g.metadata.id,
                            shard_id,
                            new_b_idx,
                            new_g_idx as u16,
                        )?;
                    }
                    compact_block.games.clear();
                }

                let (_, new_desc, new_size) = compact_writer.finish()?;
                fs::rename(&temp_compact_path, &shard_path)?;
                new_completed_shards.push((shard_path, new_desc, new_size, shard_id));
                self.catalog.clear_tombstones_for_shard(shard_id)?;
            }
        }

        self.completed_shards = new_completed_shards;
        Ok(())
    }
}

/// Reader for a finalized persistent CGB3 dataset.
///
/// Guaranteed snapshot consistency: Upon opening, the reader binds to an immutable physical
/// dataset version snapshot directory (resolved via [`resolve_dataset_snapshot`]). All subsequent
/// manifest reads, index lookups, and lazy shard loadings are pinned strictly to that physical
/// directory, even if active-version transitions or new publications occur concurrently.
pub struct DatasetReader {
    dataset_dir: PathBuf,
    manifest: DatasetManifest,
    lookup_reader: GameLookupReader,
    open_shards: HashMap<u16, ShardReader>,
    // Single-block LRU cache to accelerate sequential queries in the same block: (shard_id, block_index) -> games
    cached_block: Option<(u16, u32, Vec<ParsedGame>)>,
}

impl DatasetReader {
    /// Opens a dataset for reading, binding exclusively to a single immutable dataset snapshot.
    ///
    /// The physical version directory is resolved once at initialization. Even if `current`
    /// is atomically updated to a newer dataset version later, this reader instance continues
    /// to observe and decompress from its original version snapshot without version tearing.
    pub fn open(dataset_dir: &Path) -> Result<Self, StorageError> {
        let snapshot_dir = resolve_dataset_snapshot(dataset_dir)?;
        let manifest_path = snapshot_dir.join("manifest.json");
        if !manifest_path.exists() {
            return Err(StorageError::Decoding(format!(
                "Dataset snapshot directory '{}' is missing manifest.json",
                snapshot_dir.display()
            )));
        }

        let manifest_bytes = fs::read(&manifest_path)?;
        let manifest: DatasetManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| StorageError::Decoding(format!("Invalid manifest.json: {}", e)))?;

        let lookup_path = snapshot_dir.join(&manifest.lookup_table.filename);
        let lookup_reader = GameLookupReader::open(&lookup_path)?;

        Ok(DatasetReader {
            dataset_dir: snapshot_dir,
            manifest,
            lookup_reader,
            open_shards: HashMap::new(),
            cached_block: None,
        })
    }

    /// Returns the immutable physical snapshot directory this reader is pinned to.
    pub fn snapshot_dir(&self) -> &Path {
        &self.dataset_dir
    }

    pub fn manifest(&self) -> &DatasetManifest {
        &self.manifest
    }

    pub fn collection(&self) -> Collection {
        self.manifest.collection
    }

    pub fn total_games(&self) -> u64 {
        self.manifest.total_games
    }

    fn get_shard_reader(&mut self, shard_id: u16) -> Result<&mut ShardReader, StorageError> {
        if !self.open_shards.contains_key(&shard_id) {
            let shard_meta = self
                .manifest
                .shards
                .iter()
                .find(|s| s.shard_id == shard_id)
                .ok_or_else(|| {
                    StorageError::Decoding(format!(
                        "Shard ID {} not listed in dataset manifest",
                        shard_id
                    ))
                })?;
            let shard_path = self.dataset_dir.join("shards").join(&shard_meta.filename);
            let reader = ShardReader::open(&shard_path)?;
            self.open_shards.insert(shard_id, reader);
        }
        Ok(self.open_shards.get_mut(&shard_id).unwrap())
    }

    /// Retrieves a game by stable game ID with sub-microsecond index lookup
    /// and single-block on-demand decompression.
    pub fn get_game(&mut self, game_id: u64) -> Result<ParsedGame, StorageError> {
        let locations = self.lookup_reader.find_locations(game_id);
        if locations.is_empty() {
            return Err(StorageError::GameNotFound(game_id));
        }

        // For each candidate location (handles ID collision disambiguation)
        for loc in locations {
            let games = self.get_block_games(loc.shard_id, loc.block_index)?;
            if let Some(game) = games.get(loc.game_index_in_block as usize) {
                if game.metadata.id == game_id {
                    return Ok(game.clone());
                }
            }
        }

        Err(StorageError::GameNotFound(game_id))
    }

    fn get_block_games(
        &mut self,
        shard_id: u16,
        block_index: u32,
    ) -> Result<&[ParsedGame], StorageError> {
        let is_hit = match &self.cached_block {
            Some((s_id, b_idx, _)) => *s_id == shard_id && *b_idx == block_index,
            None => false,
        };

        if !is_hit {
            let shard = self.get_shard_reader(shard_id)?;
            let decompressed = shard.decompress_block(block_index)?;
            self.cached_block = Some((shard_id, block_index, decompressed));
        }

        Ok(&self.cached_block.as_ref().unwrap().2)
    }

    /// Retrieves a game and formats it as standard PGN.
    pub fn get_game_pgn(&mut self, game_id: u64) -> Result<String, StorageError> {
        let game = self.get_game(game_id)?;
        export_to_pgn(&game)
    }
}

pub fn compute_file_sha256(path: &Path) -> Result<String, StorageError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:064x}", hasher.finalize()))
}

fn chrono_iso8601_now() -> String {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Simple UTC representation: YYYY-MM-DDTHH:MM:SSZ
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let mins = (time_secs % 3600) / 60;
    let s = time_secs % 60;

    // Approximate calendar conversion for ISO-8601 UTC timestamp
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, mins, s
    )
}

fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    let mut y = 1970u32;
    let mut d = days;
    loop {
        let leap = is_leap_year(y);
        let days_in_year = if leap { 366 } else { 365 };
        if d >= days_in_year {
            d -= days_in_year;
            y += 1;
        } else {
            break;
        }
    }
    let leap = is_leap_year(y);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 1u32;
    for &md in &month_days {
        if d >= md {
            d -= md;
            m += 1;
        } else {
            break;
        }
    }
    (y, m, (d + 1) as u32)
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerificationReport {
    pub dataset_dir: String,
    pub collection: String,
    pub total_games: u64,
    pub shards_checked: usize,
    pub lookups_checked: u64,
    pub checksums_valid: bool,
    pub accounting_reconciled: bool,
    pub deep_verification_passed: bool,
    pub status: String,
}

/// Comprehensive dataset verification command logic.
///
/// Guaranteed snapshot consistency: resolves the dataset to its immutable physical version
/// directory at startup and performs all index checksum, shard checksum, accounting, and
/// block-decompression checks against that single pinned snapshot.
pub fn verify_dataset(dataset_dir: &Path, deep: bool) -> Result<VerificationReport, StorageError> {
    let snapshot_dir = resolve_dataset_snapshot(dataset_dir)?;
    let reader = DatasetReader::open(&snapshot_dir)?;
    let manifest = reader.manifest().clone();

    // 1. Verify lookup file SHA-256
    let lookup_path = snapshot_dir.join(&manifest.lookup_table.filename);
    let actual_lookup_sha256 = compute_file_sha256(&lookup_path)?;
    if actual_lookup_sha256 != manifest.lookup_table.sha256 {
        return Err(StorageError::Decoding(format!(
            "games.idx checksum mismatch: calculated {}, manifest {}",
            actual_lookup_sha256, manifest.lookup_table.sha256
        )));
    }

    // 2. Verify all shard file SHA-256 checksums and headers
    for shard_meta in &manifest.shards {
        let shard_path = snapshot_dir.join("shards").join(&shard_meta.filename);
        let actual_shard_sha256 = compute_file_sha256(&shard_path)?;
        if actual_shard_sha256 != shard_meta.sha256 {
            return Err(StorageError::Decoding(format!(
                "Shard '{}' checksum mismatch: calculated {}, manifest {}",
                shard_meta.filename, actual_shard_sha256, shard_meta.sha256
            )));
        }
    }

    // 3. Verify accounting invariants
    let accounting_reconciled = manifest.accounting.is_reconciled()
        && manifest
            .accounting
            .verify_retained_count(manifest.total_games as usize);

    if !accounting_reconciled {
        return Err(StorageError::Decoding(
            "Manifest accounting invariants failed reconciliation check".into(),
        ));
    }

    // 4. Optional deep verification: exact 1-to-1 mapping between physical storage and lookup index
    let deep_passed = if deep {
        // A. Verify collection matches
        if reader.lookup_reader.collection() != manifest.collection {
            return Err(StorageError::Integrity(format!(
                "Collection mismatch: manifest collection is '{}', but lookup table is '{}'",
                manifest.collection,
                reader.lookup_reader.collection()
            )));
        }

        // B. Verify lookup entry count matches manifest
        if reader.lookup_reader.entry_count() != manifest.total_games {
            return Err(StorageError::Integrity(format!(
                "Lookup entry count ({}) does not match manifest total_games ({})",
                reader.lookup_reader.entry_count(),
                manifest.total_games
            )));
        }

        // C. Verify lookup entries are strictly sorted and check location bounds
        let mut prev_game_id: Option<u64> = None;
        let count = reader.lookup_reader.entry_count() as usize;
        for i in 0..count {
            let entry = reader.lookup_reader.get_entry(i).ok_or_else(|| {
                StorageError::Integrity(format!("Failed to read lookup entry at index {}", i))
            })?;

            if let Some(prev) = prev_game_id {
                if entry.game_id < prev {
                    return Err(StorageError::Integrity(format!(
                        "Lookup table entries not sorted at index {}: game ID {} < previous {}",
                        i, entry.game_id, prev
                    )));
                }
                if entry.game_id == prev {
                    return Err(StorageError::Integrity(format!(
                        "Duplicate or colliding game ID {} found in lookup table at index {}",
                        entry.game_id, i
                    )));
                }
            }
            prev_game_id = Some(entry.game_id);

            let shard_meta = manifest
                .shards
                .iter()
                .find(|s| s.shard_id == entry.shard_id)
                .ok_or_else(|| {
                    StorageError::Integrity(format!(
                        "Lookup entry for game ID {} points to nonexistent shard ID {}",
                        entry.game_id, entry.shard_id
                    ))
                })?;

            if entry.block_index >= shard_meta.block_count {
                return Err(StorageError::Integrity(format!(
                    "Lookup entry for game ID {} points to block {} >= shard block count {}",
                    entry.game_id, entry.block_index, shard_meta.block_count
                )));
            }
        }

        // D. Decompress every block in every shard and verify 1-to-1 mapping to lookup
        let mut physical_game_count = 0u64;
        for shard_meta in &manifest.shards {
            let shard_path = snapshot_dir.join("shards").join(&shard_meta.filename);
            let mut s_reader = ShardReader::open(&shard_path)?;

            if s_reader.collection() != manifest.collection {
                return Err(StorageError::Integrity(format!(
                    "Shard '{}' collection '{}' does not match manifest '{}'",
                    shard_meta.filename,
                    s_reader.collection(),
                    manifest.collection
                )));
            }

            for b_idx in 0..s_reader.block_count() {
                let games = s_reader.decompress_block(b_idx)?;
                for (g_idx, g) in games.iter().enumerate() {
                    physical_game_count += 1;

                    let locs = reader.lookup_reader.find_locations(g.metadata.id);
                    if locs.is_empty() {
                        return Err(StorageError::Integrity(format!(
                            "Deep verification failed: game ID {} in shard {} block {} slot {} missing from lookup table",
                            g.metadata.id, shard_meta.shard_id, b_idx, g_idx
                        )));
                    }

                    let matching = locs.iter().find(|loc| {
                        loc.shard_id == shard_meta.shard_id
                            && loc.block_index == b_idx
                            && loc.game_index_in_block == g_idx as u16
                    });

                    if matching.is_none() {
                        return Err(StorageError::Integrity(format!(
                            "Deep verification failed: game ID {} physical location is ({}, {}, {}), but lookup table points elsewhere",
                            g.metadata.id, shard_meta.shard_id, b_idx, g_idx
                        )));
                    }
                }
            }
        }

        if physical_game_count != manifest.total_games {
            return Err(StorageError::Integrity(format!(
                "Deep verification failed: physical game count in shards ({}) != manifest total_games ({})",
                physical_game_count, manifest.total_games
            )));
        }

        true
    } else {
        false
    };

    Ok(VerificationReport {
        dataset_dir: dataset_dir.display().to_string(),
        collection: manifest.collection.to_string(),
        total_games: manifest.total_games,
        shards_checked: manifest.shards.len(),
        lookups_checked: manifest.total_games,
        checksums_valid: true,
        accounting_reconciled,
        deep_verification_passed: deep_passed,
        status: "PASS".into(),
    })
}

/// Resolves a dataset directory to its immutable physical snapshot directory.
///
/// If `dataset_dir/current` exists, resolves it to the physical version directory
/// (e.g. `<target>/versions/<build_id>`) and validates that `manifest.json` is present.
/// If `dataset_dir` directly contains `manifest.json` (e.g. legacy flat layout or direct
/// version path), canonicalizes and validates it.
///
/// Once resolved, readers and verifiers bind exclusively to this immutable snapshot directory,
/// ensuring that subsequent publication transitions or symlink changes do not alter
/// the dataset snapshot observed by the active reader or verifier.
pub fn resolve_dataset_snapshot(dataset_dir: &Path) -> Result<PathBuf, StorageError> {
    if !dataset_dir.exists() {
        return Err(StorageError::Decoding(format!(
            "Dataset directory '{}' does not exist",
            dataset_dir.display()
        )));
    }

    let current = dataset_dir.join("current");
    if fs::symlink_metadata(&current).is_ok() || current.exists() {
        if let Ok(canonical) = fs::canonicalize(&current) {
            if canonical.join("manifest.json").exists() {
                return Ok(canonical);
            }
        }
    }

    let manifest = dataset_dir.join("manifest.json");
    if manifest.exists() {
        let canonical = fs::canonicalize(dataset_dir).unwrap_or_else(|_| dataset_dir.to_path_buf());
        if canonical.join("manifest.json").exists() {
            return Ok(canonical);
        }
    }

    Err(StorageError::Decoding(format!(
        "Dataset directory '{}' is missing manifest.json (neither current/manifest.json nor manifest.json found)",
        dataset_dir.display()
    )))
}

/// Resolves a dataset directory path to its active data directory,
/// supporting both versioned layouts (`target/current`) and legacy flat directories.
pub fn resolve_dataset_dir(dataset_dir: &Path) -> PathBuf {
    resolve_dataset_snapshot(dataset_dir).unwrap_or_else(|_| dataset_dir.to_path_buf())
}

fn migrate_legacy_dataset(target_dir: &Path) -> Result<(), StorageError> {
    fault::trigger_if(fault::FaultPoint::BeforeLegacyMigrationPrep)?;
    let versions_dir = target_dir.join("versions");
    fs::create_dir_all(&versions_dir)?;

    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let staging_id = format!(".staging_legacy_{}_{}", std::process::id(), now_nanos);
    let staging_dir = versions_dir.join(&staging_id);
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    fs::create_dir_all(&staging_dir)?;
    fs::create_dir_all(staging_dir.join("shards"))?;

    fault::trigger_if(fault::FaultPoint::AfterLegacyMigrationDestCreated)?;

    // 1. Copy manifest.json (preserving source in target_dir)
    let src_manifest = target_dir.join("manifest.json");
    if src_manifest.exists() {
        fs::copy(&src_manifest, staging_dir.join("manifest.json"))?;
    }
    fault::trigger_if(fault::FaultPoint::AfterLegacyFirstFileTransfer)?;

    // 2. Copy games.idx (preserving source in target_dir)
    let src_lookup = target_dir.join("games.idx");
    if src_lookup.exists() {
        fs::copy(&src_lookup, staging_dir.join("games.idx"))?;
    }
    fault::trigger_if(fault::FaultPoint::BetweenLegacyFileTransfers)?;

    // 3. Copy shards (preserving source in target_dir)
    let src_shards = target_dir.join("shards");
    if src_shards.is_dir() {
        for entry in fs::read_dir(&src_shards)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let fname = entry.file_name();
                fs::copy(&path, staging_dir.join("shards").join(&fname))?;
            }
        }
    }

    // 4. Synchronize staged contents to durable storage
    sync_dir_contents(&staging_dir)?;

    // 5. Verify staging candidate before activation
    verify_dataset(&staging_dir, false)?;
    fault::trigger_if(fault::FaultPoint::AfterLegacyCandidateVerify)?;

    // 6. Rename staged candidate to versions/legacy_v0
    let legacy_dir = versions_dir.join("legacy_v0");
    if legacy_dir.exists() {
        let _ = fs::remove_dir_all(&legacy_dir);
    }
    fs::rename(&staging_dir, &legacy_dir)?;
    sync_dir_contents(&legacy_dir)?;
    sync_dir_itself(&versions_dir)?;

    // 7. Atomic symlink activation
    fault::trigger_if(fault::FaultPoint::BeforeLegacySymlinkSwitch)?;
    #[cfg(unix)]
    {
        let tmp_link = target_dir.join(format!(".current_tmp_{}", std::process::id()));
        let _ = fs::remove_file(&tmp_link);
        symlink("versions/legacy_v0", &tmp_link)?;
        fs::rename(&tmp_link, target_dir.join("current"))?;
    }
    fault::trigger_if(fault::FaultPoint::AfterLegacySymlinkSwitch)?;

    // 8. Cleanup/symlink step: replace root files with convenience symlinks
    fault::trigger_if(fault::FaultPoint::DuringLegacyCleanup)?;
    ensure_root_symlinks(target_dir)?;
    sync_dir_itself(target_dir)?;

    Ok(())
}

fn is_real_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| !m.file_type().is_symlink() && m.file_type().is_file())
        .unwrap_or(false)
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| !m.file_type().is_symlink() && m.file_type().is_dir())
        .unwrap_or(false)
}

fn validate_manifest_file(path: &Path) -> Option<DatasetManifest> {
    if !is_real_file(path) {
        return None;
    }
    let data = fs::read(path).ok()?;
    let m = serde_json::from_slice::<DatasetManifest>(&data).ok()?;
    if m.is_complete && m.manifest_version > 0 {
        Some(m)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
enum ReconstructionArtifact<'a> {
    Manifest,
    Lookup(Option<&'a DatasetManifest>, u64),
    Shard(Option<&'a DatasetManifest>, &'a str, u64),
}

fn is_artifact_valid(path: &Path, artifact: &ReconstructionArtifact) -> bool {
    if !is_real_file(path) {
        return false;
    }
    match artifact {
        ReconstructionArtifact::Manifest => validate_manifest_file(path).is_some(),
        ReconstructionArtifact::Lookup(manifest_opt, expected_len) => {
            if let Ok(meta) = fs::metadata(path) {
                if meta.len() < 64 || (meta.len() - 64) % 16 != 0 {
                    return false;
                }
                // Check magic bytes CGL1
                if let Ok(mut f) = File::open(path) {
                    use std::io::Read;
                    let mut magic = [0u8; 4];
                    if f.read_exact(&mut magic).is_err() || &magic != crate::lookup::LOOKUP_MAGIC {
                        return false;
                    }
                } else {
                    return false;
                }

                if let Some(manifest) = manifest_opt {
                    if meta.len() != manifest.lookup_table.file_size_bytes {
                        return false;
                    }
                    if let Ok(hash) = compute_file_sha256(path) {
                        return hash.eq_ignore_ascii_case(&manifest.lookup_table.sha256);
                    }
                    return false;
                } else {
                    return meta.len() == *expected_len;
                }
            }
            false
        }
        ReconstructionArtifact::Shard(manifest_opt, filename, expected_len) => {
            if let Ok(meta) = fs::metadata(path) {
                if meta.len() < 84 {
                    return false;
                }
                // Check magic bytes CGCS
                if let Ok(mut f) = File::open(path) {
                    use std::io::Read;
                    let mut magic = [0u8; 4];
                    if f.read_exact(&mut magic).is_err()
                        || &magic != crate::shard::SHARD_MAGIC_HEADER
                    {
                        return false;
                    }
                } else {
                    return false;
                }

                if let Some(manifest) = manifest_opt {
                    if let Some(shard_meta) =
                        manifest.shards.iter().find(|s| s.filename == *filename)
                    {
                        if meta.len() != shard_meta.file_size_bytes {
                            return false;
                        }
                        if let Ok(hash) = compute_file_sha256(path) {
                            return hash.eq_ignore_ascii_case(&shard_meta.sha256);
                        }
                        return false;
                    }
                }
                return meta.len() == *expected_len;
            }
            false
        }
    }
}

fn atomic_reconstruct_file(
    src: &Path,
    dest: &Path,
    artifact: &ReconstructionArtifact,
    pre_fault: Option<fault::FaultPoint>,
    partial_fault: Option<fault::FaultPoint>,
) -> Result<(), StorageError> {
    // 1. If dest already exists and is completely valid, nothing to do!
    if is_artifact_valid(dest, artifact) {
        return Ok(());
    }

    // 2. If dest is not valid, verify that src IS valid before attempting reconstruction
    if !is_artifact_valid(src, artifact) {
        return Err(StorageError::Integrity(format!(
            "Reconstruction failed verification: Cannot reconstruct '{}': source '{}' is invalid or incomplete and destination is invalid; artifacts preserved safely without deletion",
            dest.display(),
            src.display()
        )));
    }

    // 3. Pre-copy fault trigger point
    if let Some(pf) = pre_fault {
        fault::trigger_if(pf)?;
    }

    // 4. Ensure destination parent directory exists
    let parent_dir = dest.parent().ok_or_else(|| {
        StorageError::Io(std::io::Error::other(format!(
            "Destination '{}' has no parent directory",
            dest.display()
        )))
    })?;
    fs::create_dir_all(parent_dir)?;

    // 5. Create uniquely named temporary reconstruction file in destination parent directory
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_name = format!(
        ".reconstruct_tmp_{}_{}_{}",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        now_nanos
    );
    let tmp_path = parent_dir.join(&tmp_name);

    let mut src_file = File::open(src)?;
    let mut tmp_file = File::create(&tmp_path)?;

    // 6. Copy bytes, with support for simulated partial-copy failures
    if let Some(part_fault) = partial_fault {
        if fault::is_fault_active(part_fault) {
            use std::io::{Read, Write};
            let mut partial_buf = vec![0u8; 32];
            let n = src_file.read(&mut partial_buf)?;
            tmp_file.write_all(&partial_buf[..n])?;
            tmp_file.sync_all()?;
            // Trigger partial copy failure after partial write
            fault::trigger_if(part_fault)?;
        }
    }

    std::io::copy(&mut src_file, &mut tmp_file)?;

    // 7. Synchronize completed temporary file before publication
    tmp_file.sync_all()?;
    drop(tmp_file);

    // 8. Validate reconstructed artifact against authoritative metadata before publication
    if !is_artifact_valid(&tmp_path, artifact) {
        let _ = fs::remove_file(&tmp_path);
        return Err(StorageError::Integrity(format!(
            "Reconstructed temporary artifact '{}' failed verification against authoritative metadata",
            tmp_path.display()
        )));
    }

    // 9. Atomically rename completed temporary file into place
    fs::rename(&tmp_path, dest)?;

    // 10. Synchronize parent directory for durability
    sync_dir_itself(parent_dir)?;

    Ok(())
}

fn rollback_backup(backup_path: &Path, link_path: &Path) -> Result<(), StorageError> {
    if fault::trigger_if(fault::FaultPoint::FailRestoreBackup).is_err() {
        return Err(StorageError::Io(std::io::Error::other(
            "Injected fault at FailRestoreBackup",
        )));
    }
    if link_path.exists() || fs::symlink_metadata(link_path).is_ok() {
        fs::remove_file(link_path)?;
    }
    fs::rename(backup_path, link_path)?;
    sync_dir_itself(link_path.parent().unwrap())?;
    Ok(())
}

fn ensure_root_symlinks(target_dir: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        for (link_name, target) in [
            ("manifest.json", "current/manifest.json"),
            ("games.idx", "current/games.idx"),
            ("shards", "current/shards"),
        ] {
            let link_path = target_dir.join(link_name);
            let target_path = Path::new(target);

            // If it's already a symlink pointing to the expected target and target exists, nothing to do
            if let Ok(existing_target) = fs::read_link(&link_path) {
                if existing_target == target_path && target_dir.join(target).exists() {
                    continue;
                }
            }

            fault::trigger_if(fault::FaultPoint::FailCreateRootSymlink)?;

            let tmp_link =
                target_dir.join(format!(".tmp_link_{}_{}", link_name, std::process::id()));
            if tmp_link.exists() || fs::symlink_metadata(&tmp_link).is_ok() {
                fs::remove_file(&tmp_link)?;
            }
            symlink(target, &tmp_link)?;

            // If link_path is a real directory (not a symlink), rename over it will fail on POSIX with EISDIR/ENOTEMPTY.
            // Safely back it up first, perform atomic rename, and then clean up backup.
            if is_real_dir(&link_path) {
                let now_nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let backup_path = target_dir.join(format!(
                    ".backup_{}_{}_{}",
                    link_name,
                    std::process::id(),
                    now_nanos
                ));

                // Move real directory to uniquely named backup
                fs::rename(&link_path, &backup_path)?;

                // Fault point: Failure after moving original root directory to backup
                if let Err(e) = fault::trigger_if(fault::FaultPoint::FailAfterBackupBeforeSymlink) {
                    let rollback_res = rollback_backup(&backup_path, &link_path);
                    let _ = fs::remove_file(&tmp_link);
                    if let Err(rb_err) = rollback_res {
                        return Err(StorageError::Integrity(format!(
                            "Failed after moving '{}' to backup and failed to restore backup from '{}': {}. Injected fault: {}",
                            link_path.display(),
                            backup_path.display(),
                            rb_err,
                            e
                        )));
                    }
                    return Err(e);
                }

                // Install replacement symlink
                let rename_res =
                    if fault::trigger_if(fault::FaultPoint::FailRenameSymlinkOverRoot).is_err() {
                        Err(std::io::Error::other(
                            "Injected fault at FailRenameSymlinkOverRoot",
                        ))
                    } else {
                        fs::rename(&tmp_link, &link_path)
                    };

                if let Err(e) = rename_res {
                    // Attempt rollback
                    let rollback_res = rollback_backup(&backup_path, &link_path);
                    let _ = fs::remove_file(&tmp_link);
                    if let Err(rb_err) = rollback_res {
                        return Err(StorageError::Integrity(format!(
                            "Failed to install replacement symlink '{}' and failed to restore backup from '{}': {}. Underlying error: {}",
                            link_path.display(),
                            backup_path.display(),
                            rb_err,
                            e
                        )));
                    }
                    return Err(StorageError::Io(e));
                }

                // Verify replacement symlink points to target and target exists
                let target_full = target_dir.join(target);
                if !target_full.exists() {
                    let rollback_res = rollback_backup(&backup_path, &link_path);
                    return Err(StorageError::Integrity(format!(
                        "Replacement symlink target '{}' does not exist; backup restored: {:?}",
                        target_full.display(),
                        rollback_res
                    )));
                }

                // Verify backup is safely redundant before removal
                let manifest_opt = find_authoritative_manifest(target_dir);
                if !is_backup_redundant(&backup_path, &target_full, manifest_opt.as_ref())? {
                    return Err(StorageError::Integrity(format!(
                        "Backup directory '{}' cannot be established as safely redundant with replacement target '{}'; backup preserved safely",
                        backup_path.display(),
                        target_full.display()
                    )));
                }

                // Replacement symlink verified and backup established as redundant! Safe to remove the redundant backup directory.
                fs::remove_dir_all(&backup_path)?;
            } else {
                // If it's a file or existing symlink, atomic POSIX rename replaces it safely
                fs::rename(&tmp_link, &link_path)?;
            }
        }
    }
    Ok(())
}

fn find_authoritative_manifest(target_dir: &Path) -> Option<DatasetManifest> {
    // 1. Check current
    if let Some(m) = validate_manifest_file(&target_dir.join("current").join("manifest.json")) {
        return Some(m);
    }
    // 2. Check root
    if let Some(m) = validate_manifest_file(&target_dir.join("manifest.json")) {
        return Some(m);
    }
    // 3. Check versions
    let versions_dir = target_dir.join("versions");
    if versions_dir.is_dir() {
        if let Some(m) =
            validate_manifest_file(&versions_dir.join("legacy_v0").join("manifest.json"))
        {
            return Some(m);
        }
        if let Ok(entries) = fs::read_dir(&versions_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    if let Some(m) = validate_manifest_file(&entry.path().join("manifest.json")) {
                        return Some(m);
                    }
                }
            }
        }
    }
    None
}

fn is_valid_container_format(path: &Path, link_name: &str) -> bool {
    if !is_real_file(path) {
        return false;
    }
    if link_name == "shards" {
        crate::shard::ShardReader::open(path).is_ok()
    } else if link_name == "games.idx" {
        crate::lookup::GameLookupReader::open(path).is_ok()
    } else if link_name == "manifest.json" {
        validate_manifest_file(path).is_some()
    } else {
        false
    }
}

fn is_backup_redundant(
    backup_path: &Path,
    expected_path: &Path,
    manifest_opt: Option<&DatasetManifest>,
) -> Result<bool, StorageError> {
    if !backup_path.exists() || !expected_path.exists() {
        return Ok(false);
    }

    let backup_meta = match fs::metadata(backup_path) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    let expected_meta = match fs::metadata(expected_path) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };

    if backup_meta.is_file() {
        if !expected_meta.is_file() {
            return Ok(false);
        }
        if backup_meta.len() != expected_meta.len() {
            return Ok(false);
        }
        let backup_hash = compute_file_sha256(backup_path)?;
        let expected_hash = compute_file_sha256(expected_path)?;
        if !backup_hash.eq_ignore_ascii_case(&expected_hash) {
            return Ok(false);
        }
        return Ok(true);
    }

    if backup_meta.is_dir() {
        if !expected_meta.is_dir() {
            return Ok(false);
        }
        let entries = fs::read_dir(backup_path).map_err(|e| {
            StorageError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to read backup directory '{}': {}",
                    backup_path.display(),
                    e
                ),
            ))
        })?;
        for entry_res in entries {
            fault::trigger_if(fault::FaultPoint::FailRedundancyCheckEnumeration)?;
            let entry = entry_res.map_err(|e| {
                StorageError::Io(std::io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to read entry in backup directory '{}': {}",
                        backup_path.display(),
                        e
                    ),
                ))
            })?;
            let file_name = entry.file_name();
            let sub_backup = entry.path();
            let sub_expected = expected_path.join(&file_name);

            if !sub_expected.exists() {
                // File or dir in backup does not exist in expected target: unique/unknown!
                return Ok(false);
            }

            let sub_backup_meta = entry.metadata()?;
            let sub_expected_meta = fs::metadata(&sub_expected)?;

            if sub_backup_meta.is_dir() {
                if !sub_expected_meta.is_dir() {
                    return Ok(false);
                }
                if !is_backup_redundant(&sub_backup, &sub_expected, manifest_opt)? {
                    return Ok(false);
                }
            } else if sub_backup_meta.is_file() {
                if !sub_expected_meta.is_file() {
                    return Ok(false);
                }
                if sub_backup_meta.len() != sub_expected_meta.len() {
                    return Ok(false);
                }
                let backup_hash = compute_file_sha256(&sub_backup)?;
                let expected_hash = compute_file_sha256(&sub_expected)?;
                if !backup_hash.eq_ignore_ascii_case(&expected_hash) {
                    return Ok(false);
                }
                let file_name_str = file_name.to_string_lossy();
                if let Some(manifest) = manifest_opt {
                    if let Some(shard_meta) =
                        manifest.shards.iter().find(|s| s.filename == file_name_str)
                    {
                        if !backup_hash.eq_ignore_ascii_case(&shard_meta.sha256) {
                            return Ok(false);
                        }
                    } else if file_name_str == manifest.lookup_table.filename
                        && !backup_hash.eq_ignore_ascii_case(&manifest.lookup_table.sha256)
                    {
                        return Ok(false);
                    }
                }
            } else {
                return Ok(false);
            }
        }
        return Ok(true);
    }

    Ok(false)
}

fn reconcile_and_recover_backups(target_dir: &Path) -> Result<(), StorageError> {
    let entries = fs::read_dir(target_dir).map_err(|e| {
        StorageError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to read dataset directory '{}': {}",
                target_dir.display(),
                e
            ),
        ))
    })?;
    for entry_res in entries {
        let entry = entry_res.map_err(|e| {
            StorageError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to read entry in dataset directory '{}': {}",
                    target_dir.display(),
                    e
                ),
            ))
        })?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(remainder) = name_str.strip_prefix(".backup_") {
            // Extract link_name: strip prefix ".backup_" and suffix "_<pid>_<nanos>" if present
            let link_name_opt = if let Some(last_underscore) = remainder.rfind('_') {
                let without_nanos = &remainder[..last_underscore];
                without_nanos
                    .rfind('_')
                    .map(|pid_underscore| &without_nanos[..pid_underscore])
            } else {
                None
            };
            let link_name = link_name_opt.unwrap_or(remainder);
            let link_path = target_dir.join(link_name);

            let is_symlink = fs::symlink_metadata(&link_path)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);

            if !link_path.exists() && fs::symlink_metadata(&link_path).is_err() {
                // link_path does not exist at all! Restore the backup directory or file!
                rollback_backup(&path, &link_path)?;
            } else if is_symlink && !link_path.exists() {
                // link_path is a broken/dangling symlink! Restore the backup!
                rollback_backup(&path, &link_path)?;
            } else if is_symlink {
                // link_path is a symlink pointing to an existing location.
                // Must verify actual target matches expected active dataset artifact!
                let current_path = target_dir.join("current");
                let current_valid = verify_dataset(&current_path, false).is_ok();
                let expected_artifact = current_path.join(link_name);

                let expected_canon = fs::canonicalize(&expected_artifact).ok();
                let link_canon = fs::canonicalize(&link_path).ok();

                let is_expected_target = match (&expected_canon, &link_canon) {
                    (Some(exp), Some(lnk)) => exp == lnk,
                    _ => false,
                };

                if current_valid && is_expected_target {
                    let expected_dir = expected_canon.as_ref().unwrap();
                    let manifest_opt = find_authoritative_manifest(target_dir);
                    if is_backup_redundant(&path, expected_dir, manifest_opt.as_ref())? {
                        // Truly redundant! Safe to remove.
                        if path.is_dir() {
                            fs::remove_dir_all(&path)?;
                        } else {
                            fs::remove_file(&path)?;
                        }
                    } else {
                        // Redundancy cannot be established!
                        // Backup contains unique, conflicting, or unknown data.
                        // Preserve backup and fail closed!
                        return Err(StorageError::Integrity(format!(
                            "Backup '{}' cannot be established as safely redundant with active dataset artifact '{}'; backup preserved safely",
                            path.display(),
                            expected_dir.display()
                        )));
                    }
                } else {
                    // Either active dataset is not valid, or symlink points to wrong target.
                    // Symlink pointing to wrong target does not authorize backup deletion!
                    return Err(StorageError::Integrity(format!(
                        "Root symlink '{}' does not reference expected active dataset artifact '{:?}' (points to '{:?}'); backup '{}' cannot be deleted and is preserved safely",
                        link_name,
                        expected_canon,
                        link_canon,
                        path.display()
                    )));
                }
            } else if is_real_dir(&link_path) && path.is_dir() {
                // Both link_path and backup exist as real directories.
                let manifest_opt = find_authoritative_manifest(target_dir);
                let backup_entries = fs::read_dir(&path)
                    .map_err(|e| {
                        StorageError::Io(std::io::Error::new(
                            e.kind(),
                            format!(
                                "Failed to read backup directory '{}': {}",
                                path.display(),
                                e
                            ),
                        ))
                    })?
                    .map(|res| {
                        fault::trigger_if(fault::FaultPoint::FailReconcileEntriesEnumeration)?;
                        res.map_err(|e| {
                            StorageError::Io(std::io::Error::new(
                                e.kind(),
                                format!(
                                    "Failed to read entry in backup directory '{}': {}",
                                    path.display(),
                                    e
                                ),
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, StorageError>>()?;

                let mut unreconciled_items = Vec::new();

                for sub in backup_entries {
                    let sub_name = sub.file_name();
                    let sub_name_str = sub_name.to_string_lossy().to_string();
                    let sub_src = sub.path();
                    let sub_dst = link_path.join(&sub_name);

                    let artifact = if link_name == "shards" {
                        let expected_len = if let Some(ref m) = manifest_opt {
                            m.shards
                                .iter()
                                .find(|s| s.filename == sub_name_str)
                                .map(|s| s.file_size_bytes)
                                .unwrap_or(0)
                        } else {
                            sub_src.metadata().map(|m| m.len()).unwrap_or(0)
                        };
                        ReconstructionArtifact::Shard(
                            manifest_opt.as_ref(),
                            &sub_name_str,
                            expected_len,
                        )
                    } else if link_name == "games.idx" {
                        let expected_len = manifest_opt
                            .as_ref()
                            .map(|m| m.lookup_table.file_size_bytes)
                            .unwrap_or(0);
                        ReconstructionArtifact::Lookup(manifest_opt.as_ref(), expected_len)
                    } else {
                        ReconstructionArtifact::Manifest
                    };

                    if !sub_dst.exists() {
                        // Missing in destination root directory
                        if is_artifact_valid(&sub_src, &artifact) {
                            atomic_reconstruct_file(&sub_src, &sub_dst, &artifact, None, None)?;
                            let _ = fs::remove_file(&sub_src);
                        } else {
                            unreconciled_items.push(format!(
                                "missing destination, invalid source: {}",
                                sub_name_str
                            ));
                        }
                    } else {
                        // Name collision!
                        let src_valid = is_artifact_valid(&sub_src, &artifact);
                        let dst_valid = is_artifact_valid(&sub_dst, &artifact);
                        let src_is_container = is_valid_container_format(&sub_src, link_name);

                        if src_valid && !dst_valid {
                            // Destination is corrupt/invalid, backup is valid.
                            // Reconstruct atomically over destination.
                            atomic_reconstruct_file(&sub_src, &sub_dst, &artifact, None, None)?;
                            let _ = fs::remove_file(&sub_src);
                        } else if src_valid && dst_valid {
                            // Both are valid against manifest; compare hashes
                            let src_hash = compute_file_sha256(&sub_src)?;
                            let dst_hash = compute_file_sha256(&sub_dst)?;
                            if src_hash.eq_ignore_ascii_case(&dst_hash) {
                                let _ = fs::remove_file(&sub_src);
                            } else {
                                unreconciled_items.push(format!(
                                        "conflicting distinct valid files for '{}' (root sha256: {}, backup sha256: {})",
                                        sub_name_str, dst_hash, src_hash
                                    ));
                            }
                        } else if !src_valid && dst_valid {
                            // Destination is valid against manifest.
                            // If backup is a valid distinct container, preserve both!
                            if src_is_container {
                                let src_hash = compute_file_sha256(&sub_src)?;
                                let dst_hash = compute_file_sha256(&sub_dst)?;
                                if src_hash.eq_ignore_ascii_case(&dst_hash) {
                                    let _ = fs::remove_file(&sub_src);
                                } else {
                                    unreconciled_items.push(format!(
                                            "conflicting distinct valid files for '{}' (root sha256: {}, backup sha256: {})",
                                            sub_name_str, dst_hash, src_hash
                                        ));
                                }
                            } else {
                                // Backup is an incomplete or corrupt fragment
                                let _ = fs::remove_file(&sub_src);
                            }
                        } else {
                            // Neither candidate matches manifest
                            if src_is_container {
                                let src_hash = compute_file_sha256(&sub_src).unwrap_or_default();
                                let dst_hash = compute_file_sha256(&sub_dst).unwrap_or_default();
                                if !src_hash.is_empty() && src_hash.eq_ignore_ascii_case(&dst_hash)
                                {
                                    let _ = fs::remove_file(&sub_src);
                                } else {
                                    unreconciled_items.push(format!(
                                            "conflicting unverified artifacts for '{}' (root sha256: {}, backup sha256: {})",
                                            sub_name_str, dst_hash, src_hash
                                        ));
                                }
                            } else {
                                unreconciled_items.push(format!(
                                        "both root and backup candidates for '{}' are invalid or incomplete",
                                        sub_name_str
                                    ));
                            }
                        }
                    }
                }

                if !unreconciled_items.is_empty() {
                    return Err(StorageError::Integrity(format!(
                            "Cannot safely reconcile backup directory '{}' with '{}': {:?}; backup and root artifacts preserved safely",
                            path.display(),
                            link_path.display(),
                            unreconciled_items
                        )));
                }

                // Check if backup directory is now empty
                fault::trigger_if(fault::FaultPoint::FailRemainingEntriesEnumeration)?;
                let remaining = fs::read_dir(&path)
                    .map_err(|e| {
                        StorageError::Io(std::io::Error::new(
                            e.kind(),
                            format!(
                                "Failed to verify remaining entries in backup directory '{}': {}",
                                path.display(),
                                e
                            ),
                        ))
                    })?
                    .map(|res| {
                        fault::trigger_if(fault::FaultPoint::FailRemainingEntriesEnumeration)?;
                        res.map_err(|e| {
                            StorageError::Io(std::io::Error::new(
                                e.kind(),
                                format!(
                                    "Failed to read remaining entry in backup directory '{}': {}",
                                    path.display(),
                                    e
                                ),
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, StorageError>>()?;

                if remaining.is_empty() {
                    fs::remove_dir_all(&path)?;
                    sync_dir_itself(target_dir)?;
                } else {
                    return Err(StorageError::Integrity(format!(
                            "Backup directory '{}' still contains unexpected files: {:?}; backup preserved safely",
                            path.display(),
                            remaining.iter().map(|e| e.file_name()).collect::<Vec<_>>()
                        )));
                }
            } else if is_real_file(&link_path) && path.is_file() {
                // Both link_path and backup exist as real files
                let manifest_opt = find_authoritative_manifest(target_dir);
                let artifact = if link_name == "games.idx" {
                    let expected_len = manifest_opt
                        .as_ref()
                        .map(|m| m.lookup_table.file_size_bytes)
                        .unwrap_or(0);
                    ReconstructionArtifact::Lookup(manifest_opt.as_ref(), expected_len)
                } else {
                    ReconstructionArtifact::Manifest
                };

                let src_valid = is_artifact_valid(&path, &artifact);
                let dst_valid = is_artifact_valid(&link_path, &artifact);
                let src_is_container = is_valid_container_format(&path, link_name);

                if src_valid && !dst_valid {
                    atomic_reconstruct_file(&path, &link_path, &artifact, None, None)?;
                    let _ = fs::remove_file(&path);
                } else if src_valid && dst_valid {
                    let src_hash = compute_file_sha256(&path)?;
                    let dst_hash = compute_file_sha256(&link_path)?;
                    if src_hash.eq_ignore_ascii_case(&dst_hash) {
                        let _ = fs::remove_file(&path);
                    } else {
                        return Err(StorageError::Integrity(format!(
                                "Conflicting distinct valid files for '{}' (root sha256: {}, backup sha256: {}); preserving both and aborting reconciliation",
                                link_name, dst_hash, src_hash
                            )));
                    }
                } else if !src_valid && dst_valid {
                    if src_is_container {
                        let src_hash = compute_file_sha256(&path)?;
                        let dst_hash = compute_file_sha256(&link_path)?;
                        if src_hash.eq_ignore_ascii_case(&dst_hash) {
                            let _ = fs::remove_file(&path);
                        } else {
                            return Err(StorageError::Integrity(format!(
                                    "Conflicting distinct valid files for '{}' (root sha256: {}, backup sha256: {}); preserving both and aborting reconciliation",
                                    link_name, dst_hash, src_hash
                                )));
                        }
                    } else {
                        let _ = fs::remove_file(&path);
                    }
                } else {
                    return Err(StorageError::Integrity(format!(
                            "Both root and backup candidates for '{}' are invalid or incomplete; preserving both and aborting reconciliation",
                            link_name
                        )));
                }
            } else {
                // In all other ambiguous cases, preserve the backup safely!
                return Err(StorageError::Integrity(format!(
                        "Ambiguous backup state for '{}' (backup: '{}', link: '{}'); preserving backup safely",
                        link_name,
                        path.display(),
                        link_path.display()
                    )));
            }
        }
    }
    Ok(())
}

fn cleanup_reconstruct_temps(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(".reconstruct_tmp_")
                || name_str.starts_with(".current_tmp")
                || name_str.starts_with(".tmp_link_")
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Safe, deterministic, and idempotent recovery procedure for datasets.
/// Detects interrupted publications and interrupted legacy migrations across every state,
/// cleans up stale staging data, restores missing pieces from split states,
/// and restores the active 'current' version safely.
pub fn recover_dataset(target_dir: &Path) -> Result<PathBuf, StorageError> {
    if !target_dir.exists() {
        return Err(StorageError::Integrity(format!(
            "Dataset directory '{}' does not exist",
            target_dir.display()
        )));
    }

    // 1. Reconcile any unrestored or redundant backup directories safely
    reconcile_and_recover_backups(target_dir)?;

    // 2. Clean up abandoned temporary symlinks, temporary reconstruction files, and staging directories
    cleanup_reconstruct_temps(target_dir);
    if let Ok(entries) = fs::read_dir(target_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(".staging_") {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    let versions_dir = target_dir.join("versions");
    if versions_dir.is_dir() {
        cleanup_reconstruct_temps(&versions_dir);
        let legacy_dir = versions_dir.join("legacy_v0");
        if legacy_dir.is_dir() {
            cleanup_reconstruct_temps(&legacy_dir);
            cleanup_reconstruct_temps(&legacy_dir.join("shards"));
        }
        if let Ok(entries) = fs::read_dir(&versions_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(".staging_") {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
    }
    cleanup_reconstruct_temps(&target_dir.join("shards"));

    // 3. Check if current symlink exists and is valid
    let current_path = target_dir.join("current");
    let current_is_valid = if current_path.exists() {
        verify_dataset(&current_path, false).is_ok()
    } else {
        false
    };

    if current_is_valid {
        ensure_root_symlinks(target_dir)?;
        sync_dir_itself(target_dir)?;
        return Ok(current_path);
    }

    // 4. Check for partial legacy split states (e.g. from prior aborted legacy migration where files were moved or split)
    let legacy_dir = versions_dir.join("legacy_v0");
    if legacy_dir.is_dir() {
        let leg_manifest_path = legacy_dir.join("manifest.json");
        let root_manifest_path = target_dir.join("manifest.json");

        let leg_has_manifest = validate_manifest_file(&leg_manifest_path).is_some();
        let root_has_manifest = validate_manifest_file(&root_manifest_path).is_some();

        // Reconstruct manifest if missing or invalid on one side
        if !leg_has_manifest && root_has_manifest {
            atomic_reconstruct_file(
                &root_manifest_path,
                &leg_manifest_path,
                &ReconstructionArtifact::Manifest,
                Some(fault::FaultPoint::FailRecoveryCopyManifest),
                Some(fault::FaultPoint::FailPartialCopyManifest),
            )?;
        }
        if !root_has_manifest && leg_has_manifest {
            atomic_reconstruct_file(
                &leg_manifest_path,
                &root_manifest_path,
                &ReconstructionArtifact::Manifest,
                Some(fault::FaultPoint::FailRecoveryCopyManifest),
                Some(fault::FaultPoint::FailPartialCopyManifest),
            )?;
        }

        // Re-check for authoritative manifest
        let authoritative_manifest_val = validate_manifest_file(&leg_manifest_path)
            .or_else(|| validate_manifest_file(&root_manifest_path));
        let authoritative_manifest = authoritative_manifest_val.as_ref();

        // Reconstruct lookup index
        let root_lookup = target_dir.join("games.idx");
        let leg_lookup = legacy_dir.join("games.idx");

        let lookup_expected_len = authoritative_manifest
            .map(|m| m.lookup_table.file_size_bytes)
            .unwrap_or_else(|| {
                fs::metadata(&root_lookup)
                    .or_else(|_| fs::metadata(&leg_lookup))
                    .map(|m| m.len())
                    .unwrap_or(0)
            });
        let lookup_artifact =
            ReconstructionArtifact::Lookup(authoritative_manifest, lookup_expected_len);

        if !is_artifact_valid(&leg_lookup, &lookup_artifact) && is_real_file(&root_lookup) {
            atomic_reconstruct_file(
                &root_lookup,
                &leg_lookup,
                &lookup_artifact,
                Some(fault::FaultPoint::FailRecoveryCopyLookup),
                Some(fault::FaultPoint::FailPartialCopyLookup),
            )?;
        }
        if !is_artifact_valid(&root_lookup, &lookup_artifact) && is_real_file(&leg_lookup) {
            atomic_reconstruct_file(
                &leg_lookup,
                &root_lookup,
                &lookup_artifact,
                Some(fault::FaultPoint::FailRecoveryCopyLookup),
                Some(fault::FaultPoint::FailPartialCopyLookup),
            )?;
        }

        // Reconstruct shards
        let root_shards = target_dir.join("shards");
        let leg_shards = legacy_dir.join("shards");

        if is_real_dir(&root_shards) {
            if !leg_shards.exists() {
                fault::trigger_if(fault::FaultPoint::FailRecoveryCreateDestDir)?;
                fs::create_dir_all(&leg_shards)?;
            }
            for entry in fs::read_dir(&root_shards)? {
                let entry = entry?;
                let path = entry.path();
                let fname = entry.file_name();
                let fname_str = fname.to_string_lossy().to_string();
                if fname_str.ends_with(".cgb3") {
                    let dest_shard = leg_shards.join(&fname);
                    let expected_len = authoritative_manifest
                        .and_then(|m| m.shards.iter().find(|s| s.filename == fname_str))
                        .map(|s| s.file_size_bytes)
                        .unwrap_or_else(|| fs::metadata(&path).map(|m| m.len()).unwrap_or(0));
                    let shard_artifact = ReconstructionArtifact::Shard(
                        authoritative_manifest,
                        &fname_str,
                        expected_len,
                    );
                    if !is_artifact_valid(&dest_shard, &shard_artifact) && is_real_file(&path) {
                        atomic_reconstruct_file(
                            &path,
                            &dest_shard,
                            &shard_artifact,
                            Some(fault::FaultPoint::FailRecoveryCopyShard),
                            Some(fault::FaultPoint::FailPartialCopyShard),
                        )?;
                    }
                }
            }
        }

        if is_real_dir(&leg_shards) {
            if !root_shards.exists() {
                fault::trigger_if(fault::FaultPoint::FailRecoveryCreateDestDir)?;
                fs::create_dir_all(&root_shards)?;
            }
            for entry in fs::read_dir(&leg_shards)? {
                let entry = entry?;
                let path = entry.path();
                let fname = entry.file_name();
                let fname_str = fname.to_string_lossy().to_string();
                if fname_str.ends_with(".cgb3") {
                    let dest_shard = root_shards.join(&fname);
                    let expected_len = authoritative_manifest
                        .and_then(|m| m.shards.iter().find(|s| s.filename == fname_str))
                        .map(|s| s.file_size_bytes)
                        .unwrap_or_else(|| fs::metadata(&path).map(|m| m.len()).unwrap_or(0));
                    let shard_artifact = ReconstructionArtifact::Shard(
                        authoritative_manifest,
                        &fname_str,
                        expected_len,
                    );
                    if !is_artifact_valid(&dest_shard, &shard_artifact) && is_real_file(&path) {
                        atomic_reconstruct_file(
                            &path,
                            &dest_shard,
                            &shard_artifact,
                            Some(fault::FaultPoint::FailRecoveryCopyShard),
                            Some(fault::FaultPoint::FailPartialCopyShard),
                        )?;
                    }
                }
            }
        }

        fault::trigger_if(fault::FaultPoint::FailRecoverySyncReconstructed)?;
        sync_dir_contents(&legacy_dir)?;

        if verify_dataset(&legacy_dir, false).is_ok() {
            #[cfg(unix)]
            {
                let tmp_link = target_dir.join(format!(".current_tmp_{}", std::process::id()));
                let _ = fs::remove_file(&tmp_link);
                symlink("versions/legacy_v0", &tmp_link)?;
                fs::rename(&tmp_link, target_dir.join("current"))?;
            }
            ensure_root_symlinks(target_dir)?;
            sync_dir_itself(target_dir)?;
            return Ok(target_dir.join("current"));
        } else {
            // NEVER delete legacy_dir or any surviving files!
            if verify_dataset(target_dir, false).is_ok() {
                migrate_legacy_dataset(target_dir)?;
                return Ok(target_dir.join("current"));
            }
            return Err(StorageError::Integrity(format!(
                "Failed to recover dataset in '{}': reconstructed legacy_v0 at '{}' failed verification; artifacts preserved safely without deletion",
                target_dir.display(),
                legacy_dir.display()
            )));
        }
    }

    // 4. Inspect versions directory for latest valid version
    if versions_dir.is_dir() {
        let mut candidates = Vec::new();
        for entry_res in fs::read_dir(&versions_dir)? {
            let entry = entry_res?;
            let p = entry.path();
            if p.is_dir() {
                let meta = p.metadata()?;
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                candidates.push((p, mtime));
            }
        }
        candidates.sort_by_key(|a| std::cmp::Reverse(a.1));

        for (cand, _) in candidates {
            if verify_dataset(&cand, false).is_ok() {
                let version_name = cand.file_name().unwrap();
                let tmp_link = target_dir.join(format!(".current_tmp_{}", std::process::id()));
                let _ = fs::remove_file(&tmp_link);
                #[cfg(unix)]
                symlink(Path::new("versions").join(version_name), &tmp_link)?;
                fs::rename(&tmp_link, target_dir.join("current"))?;
                ensure_root_symlinks(target_dir)?;
                sync_dir_itself(target_dir)?;
                return Ok(target_dir.join("current"));
            }
        }
    }

    // 5. Check if target_dir has a valid legacy flat layout
    let root_manifest = target_dir.join("manifest.json");
    if root_manifest.is_file() {
        let is_sym = fs::symlink_metadata(&root_manifest)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if !is_sym && verify_dataset(target_dir, false).is_ok() {
            migrate_legacy_dataset(target_dir)?;
            return Ok(target_dir.join("current"));
        }
    }

    Err(StorageError::Integrity(format!(
        "Failed to recover dataset in '{}': no valid dataset version found",
        target_dir.display()
    )))
}

/// Separated maintenance cleanup: removes older superseded versions while preserving
/// the active published version and the most recent `keep_versions` previous versions.
///
/// ### Reader Retention Contract:
/// - **Active Version Invariant:** The currently active published version (`current`) is never removed.
/// - **Configurable History Window:** Callers specify `keep_versions` to retain the N most recent
///   superseded versions on disk, providing a grace period for active concurrent readers.
/// - **Fail-Closed on Pruned Versions:** Readers pinned to older versions that have been pruned
///   will encounter an explicit [`StorageError::Io`] (e.g. `NotFound`) if they attempt to lazily
///   open an un-cached shard from a deleted version, preventing silent cross-version data mixing.
/// - **Quiescence / Maintenance Recommendation:** Deployments should size `keep_versions` to comfortably
///   exceed expected reader process lifetimes, or run cleanup during quiescent maintenance windows.
pub fn cleanup_old_versions(
    target_dir: &Path,
    keep_versions: usize,
) -> Result<usize, StorageError> {
    let versions_dir = target_dir.join("versions");
    if !versions_dir.is_dir() {
        return Ok(0);
    }

    // Find the currently active version name so we NEVER delete it!
    let active_version_name = if let Ok(target) = fs::read_link(target_dir.join("current")) {
        target.file_name().map(|n| n.to_string_lossy().to_string())
    } else {
        None
    };

    let mut versions = Vec::new();
    for entry_res in fs::read_dir(&versions_dir)? {
        let entry = entry_res?;
        let p = entry.path();
        if p.is_dir() {
            let meta = p.metadata()?;
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let is_active = active_version_name.as_deref() == Some(&name);
            versions.push((p, mtime, is_active));
        }
    }

    // Sort newest first
    versions.sort_by_key(|a| std::cmp::Reverse(a.1));

    let mut removed = 0;
    let mut kept_previous = 0;
    for (path, _, is_active) in versions {
        if is_active {
            // Never remove active version
            continue;
        }
        if kept_previous < keep_versions {
            kept_previous += 1;
        } else {
            fs::remove_dir_all(&path)?;
            removed += 1;
        }
    }
    sync_dir_itself(&versions_dir)?;
    Ok(removed)
}

/// Synchronizes a directory's entries to persistent storage and propagates I/O errors.
pub fn sync_dir_itself(dir: &Path) -> Result<(), StorageError> {
    let f = File::open(dir)?;
    f.sync_all()?;
    Ok(())
}

/// Recursively synchronizes all file contents in a directory tree, skipping symlinks,
/// and propagates any I/O errors immediately.
pub fn sync_dir_contents(dir: &Path) -> Result<(), StorageError> {
    let entries = fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let p = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        } else if file_type.is_file() {
            let f = File::open(&p)?;
            f.sync_all()?;
        } else if file_type.is_dir() {
            sync_dir_contents(&p)?;
        }
    }
    sync_dir_itself(dir)?;
    Ok(())
}
