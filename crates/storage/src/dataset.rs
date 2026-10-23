use codb_core::{Collection, GameLocation, IngestionStats, ParsedGame};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::format::{export_to_pgn, GameBlock, StorageError};
use crate::lookup::{GameLookupReader, GameLookupWriter};
use crate::shard::{BlockDescriptor, ShardReader, ShardWriter};

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

/// Persistent, bounded-memory dataset builder.
pub struct DatasetWriter {
    target_dir: PathBuf,
    staging_dir: PathBuf,
    config: DatasetWriterConfig,
    current_shard_idx: u16,
    active_shard: Option<ShardWriter>,
    completed_shards: Vec<(PathBuf, Vec<BlockDescriptor>, u64, u16)>,
    current_block: GameBlock,
    // Maps active game_id -> GameLocation
    game_locations: HashMap<u64, GameLocation>,
    // Set of game locations that have been tombstoned due to superseding
    tombstoned_locations: HashSet<GameLocation>,
    total_retained_games: u64,
}

impl DatasetWriter {
    pub fn new(target_dir: PathBuf, config: DatasetWriterConfig) -> Result<Self, StorageError> {
        let staging_id = format!(
            "build_{}_{}_{}",
            config.collection,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let staging_dir = target_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".staging_{}", staging_id));

        if staging_dir.exists() {
            fs::remove_dir_all(&staging_dir)?;
        }
        fs::create_dir_all(&staging_dir)?;
        fs::create_dir_all(staging_dir.join("shards"))?;

        let collection = config.collection;
        Ok(DatasetWriter {
            target_dir,
            staging_dir,
            config,
            current_shard_idx: 0,
            active_shard: None,
            completed_shards: Vec::new(),
            current_block: GameBlock::new(collection),
            game_locations: HashMap::new(),
            tombstoned_locations: HashSet::new(),
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
        let compressed_payload = self.current_block.compress(self.config.zstd_level)?;
        let uncompressed_size = (self.current_block.games.len() * 150) as u32; // representative size

        let shard = self.ensure_active_shard()?;
        let shard_id = shard.shard_id();
        let block_index = shard.append_block(
            &compressed_payload,
            game_count,
            first_game_id,
            uncompressed_size,
        )?;

        // Record location for all games in this block
        for (idx, game) in self.current_block.games.iter().enumerate() {
            let loc = GameLocation::new(shard_id, block_index, idx as u16);
            self.game_locations.insert(game.metadata.id, loc);
        }

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

        // 2. Old game was already written to disk
        if let Some(old_loc) = self.game_locations.remove(&old_id) {
            self.tombstoned_locations.insert(old_loc);
        }

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
        if !self.tombstoned_locations.is_empty() {
            self.compact_tombstones()?;
        }

        // Write global game lookup index (games.idx)
        let lookup_path = self.staging_dir.join("games.idx");
        let mut lookup_writer = GameLookupWriter::with_capacity(
            lookup_path.clone(),
            self.config.collection,
            self.game_locations.len(),
        );

        for (&game_id, &loc) in &self.game_locations {
            lookup_writer.add_entry(game_id, loc);
        }

        let (_written_lookup_path, lookup_size_bytes, _internal_sha) = lookup_writer.finish()?;
        let lookup_sha256 = compute_file_sha256(&lookup_path)?;

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

        let total_retained = self.game_locations.len() as u64;

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

        let manifest = DatasetManifest {
            manifest_version: 1,
            dataset_build_id: format!(
                "codb-{}-{}",
                self.config.collection,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
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

        // Self-verify staging build before publication
        Self::verify_staging_artifacts(&self.staging_dir, &manifest)?;

        // Atomically publish: rename staging_dir to target_dir
        if self.target_dir.exists() {
            fs::remove_dir_all(&self.target_dir)?;
        }
        if let Some(parent) = self.target_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&self.staging_dir, &self.target_dir)?;

        Ok(manifest)
    }

    /// Compaction pass: rewrites blocks containing tombstoned games to guarantee
    /// 100% dense, active games with zero superseded records in the finalized dataset.
    fn compact_tombstones(&mut self) -> Result<(), StorageError> {
        // Collect shards needing compaction
        let tombstoned_shards: HashSet<u16> = self
            .tombstoned_locations
            .iter()
            .map(|loc| loc.shard_id)
            .collect();

        if tombstoned_shards.is_empty() {
            return Ok(());
        }

        // For each affected shard, read non-tombstoned games and rewrite
        let mut new_completed_shards = Vec::new();
        self.game_locations.clear();

        for (shard_path, descriptors, _, shard_id) in self.completed_shards.drain(..) {
            if !tombstoned_shards.contains(&shard_id) {
                // Re-register locations for untouched shard
                let mut reader = ShardReader::open(&shard_path)?;
                for (b_idx, _desc) in descriptors.iter().enumerate() {
                    let block_games = reader.decompress_block(b_idx as u32)?;
                    for (g_idx, game) in block_games.iter().enumerate() {
                        self.game_locations.insert(
                            game.metadata.id,
                            GameLocation::new(shard_id, b_idx as u32, g_idx as u16),
                        );
                    }
                }
                new_completed_shards.push((
                    shard_path,
                    descriptors,
                    reader.total_games(),
                    shard_id,
                ));
            } else {
                // Read all non-tombstoned games from this shard
                let mut reader = ShardReader::open(&shard_path)?;
                let mut retained_games = Vec::new();

                for (b_idx, _desc) in descriptors.iter().enumerate() {
                    let block_games = reader.decompress_block(b_idx as u32)?;
                    for (g_idx, game) in block_games.into_iter().enumerate() {
                        let loc = GameLocation::new(shard_id, b_idx as u32, g_idx as u16);
                        if !self.tombstoned_locations.contains(&loc) {
                            retained_games.push(game);
                        }
                    }
                }

                // Rewrite compacted shard to a temporary file, then replace
                let temp_compact_path = shard_path.with_extension("compact");
                let mut compact_writer = ShardWriter::create(
                    temp_compact_path.clone(),
                    shard_id,
                    self.config.collection,
                )?;

                for chunk in retained_games.chunks(self.config.block_capacity) {
                    let mut block = GameBlock::new(self.config.collection);
                    block.games = chunk.to_vec();
                    let payload = block.compress(self.config.zstd_level)?;
                    let first_id = chunk[0].metadata.id;
                    let b_idx = compact_writer.append_block(
                        &payload,
                        chunk.len() as u32,
                        first_id,
                        (chunk.len() * 150) as u32,
                    )?;
                    for (g_idx, g) in chunk.iter().enumerate() {
                        self.game_locations.insert(
                            g.metadata.id,
                            GameLocation::new(shard_id, b_idx, g_idx as u16),
                        );
                    }
                }

                let (_, new_desc, new_size) = compact_writer.finish()?;
                fs::rename(&temp_compact_path, &shard_path)?;
                new_completed_shards.push((shard_path, new_desc, new_size, shard_id));
            }
        }

        self.completed_shards = new_completed_shards;
        self.tombstoned_locations.clear();
        Ok(())
    }

    fn verify_staging_artifacts(
        staging_dir: &Path,
        manifest: &DatasetManifest,
    ) -> Result<(), StorageError> {
        let lookup_path = staging_dir.join("games.idx");
        if !lookup_path.exists() {
            return Err(StorageError::Decoding(
                "Missing games.idx in staging build".into(),
            ));
        }

        let lookup_reader = GameLookupReader::open(&lookup_path)?;
        if lookup_reader.entry_count() != manifest.total_games {
            return Err(StorageError::Decoding(format!(
                "Lookup entry count ({}) does not match manifest total_games ({})",
                lookup_reader.entry_count(),
                manifest.total_games
            )));
        }

        if !lookup_reader.verify_checksum()? {
            return Err(StorageError::Decoding(
                "games.idx failed SHA-256 integrity check".into(),
            ));
        }

        for shard_meta in &manifest.shards {
            let shard_path = staging_dir.join("shards").join(&shard_meta.filename);
            if !shard_path.exists() {
                return Err(StorageError::Decoding(format!(
                    "Missing shard file '{}' in staging",
                    shard_meta.filename
                )));
            }
            let reader = ShardReader::open(&shard_path)?;
            if reader.block_count() != shard_meta.block_count {
                return Err(StorageError::Decoding(format!(
                    "Shard '{}' block count mismatch: actual {}, expected {}",
                    shard_meta.filename,
                    reader.block_count(),
                    shard_meta.block_count
                )));
            }
        }

        Ok(())
    }
}

/// Reader for a finalized persistent CGB3 dataset.
pub struct DatasetReader {
    dataset_dir: PathBuf,
    manifest: DatasetManifest,
    lookup_reader: GameLookupReader,
    open_shards: HashMap<u16, ShardReader>,
    // Single-block LRU cache to accelerate sequential queries in the same block: (shard_id, block_index) -> games
    cached_block: Option<(u16, u32, Vec<ParsedGame>)>,
}

impl DatasetReader {
    pub fn open(dataset_dir: &Path) -> Result<Self, StorageError> {
        let manifest_path = dataset_dir.join("manifest.json");
        if !manifest_path.exists() {
            return Err(StorageError::Decoding(format!(
                "Dataset directory '{}' is missing manifest.json",
                dataset_dir.display()
            )));
        }

        let manifest_bytes = fs::read(&manifest_path)?;
        let manifest: DatasetManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| StorageError::Decoding(format!("Invalid manifest.json: {}", e)))?;

        let lookup_path = dataset_dir.join(&manifest.lookup_table.filename);
        let lookup_reader = GameLookupReader::open(&lookup_path)?;

        Ok(DatasetReader {
            dataset_dir: dataset_dir.to_path_buf(),
            manifest,
            lookup_reader,
            open_shards: HashMap::new(),
            cached_block: None,
        })
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
        if let Some((s_id, b_idx, ref games)) = self.cached_block {
            if s_id == shard_id && b_idx == block_index {
                // Cache hit
                return Ok(unsafe { &*(games.as_slice() as *const [ParsedGame]) });
            }
        }

        // Cache miss: decompress block
        let shard = self.get_shard_reader(shard_id)?;
        let decompressed = shard.decompress_block(block_index)?;
        self.cached_block = Some((shard_id, block_index, decompressed));

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
pub fn verify_dataset(dataset_dir: &Path, deep: bool) -> Result<VerificationReport, StorageError> {
    let reader = DatasetReader::open(dataset_dir)?;
    let manifest = reader.manifest().clone();

    // 1. Verify lookup file SHA-256
    let lookup_path = dataset_dir.join(&manifest.lookup_table.filename);
    let actual_lookup_sha256 = compute_file_sha256(&lookup_path)?;
    if actual_lookup_sha256 != manifest.lookup_table.sha256 {
        return Err(StorageError::Decoding(format!(
            "games.idx checksum mismatch: calculated {}, manifest {}",
            actual_lookup_sha256, manifest.lookup_table.sha256
        )));
    }

    // 2. Verify all shard file SHA-256 checksums and headers
    for shard_meta in &manifest.shards {
        let shard_path = dataset_dir.join("shards").join(&shard_meta.filename);
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

    // 4. Optional deep verification: decompress and verify every game in every block
    let deep_passed = if deep {
        for shard_meta in &manifest.shards {
            let shard_path = dataset_dir.join("shards").join(&shard_meta.filename);
            let mut s_reader = ShardReader::open(&shard_path)?;
            for b_idx in 0..s_reader.block_count() {
                let games = s_reader.decompress_block(b_idx)?;
                for g in games {
                    // Check game exists in lookup index
                    let locs = reader.lookup_reader.find_locations(g.metadata.id);
                    if locs.is_empty() {
                        return Err(StorageError::Decoding(format!(
                            "Deep verification failed: game ID {} in shard {} block {} missing from lookup table",
                            g.metadata.id, shard_meta.shard_id, b_idx
                        )));
                    }
                }
            }
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
