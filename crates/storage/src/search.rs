//! Search artifact identity and verification for immutable dataset snapshots.

#[cfg(unix)]
pub mod bundle;

use crate::{compute_file_sha256, DatasetManifest, DatasetReader, StorageError};
use opening_explorer_index::search::{catalog::SearchCatalog, postings::SearchPostingsReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const SEARCH_FORMAT_VERSION: u32 = 1;
pub const SEARCH_DIRECTORY: &str = "search";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SearchCoverage {
    Bounded { max_ply: u16 },
    Full,
}

impl SearchCoverage {
    pub fn max_ply(self) -> Option<u16> {
        match self {
            Self::Bounded { max_ply } => Some(max_ply),
            Self::Full => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SearchArtifact {
    pub file_size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SearchIndexMetadata {
    pub format_version: u32,
    pub coverage: SearchCoverage,
    pub game_count: u32,
    pub position_count: u64,
    pub posting_count: u64,
    pub source_snapshot: String,
    pub canonical_sha256: String,
    pub sort_buffer_bytes: usize,
    pub artifacts: BTreeMap<String, SearchArtifact>,
}

impl SearchIndexMetadata {
    pub fn file_size_bytes(&self) -> Result<u64, StorageError> {
        self.artifacts.values().try_fold(0u64, |sum, artifact| {
            sum.checked_add(artifact.file_size_bytes)
                .ok_or_else(|| StorageError::Integrity("Search artifact size overflow".into()))
        })
    }
}

/// Fingerprint the canonical artifact set used by search. CPI is separately derived
/// and can be rebuilt without changing the game or attribution identity.
pub fn canonical_search_fingerprint(manifest: &DatasetManifest) -> String {
    let mut digest = Sha256::new();
    digest.update(b"chess-database-search-source-v1\0");
    digest.update(manifest.collection.as_str().as_bytes());
    digest.update([0]);
    digest.update(manifest.total_games.to_be_bytes());
    digest.update(manifest.lookup_table.sha256.to_ascii_lowercase().as_bytes());
    let mut shards: Vec<_> = manifest.shards.iter().collect();
    shards.sort_unstable_by_key(|shard| shard.shard_id);
    for shard in shards {
        digest.update(shard.shard_id.to_be_bytes());
        digest.update(shard.sha256.to_ascii_lowercase().as_bytes());
    }
    if let Some(attribution) = &manifest.source_attribution {
        digest.update(attribution.sha256.to_ascii_lowercase().as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(crate) fn verify_search_artifacts(
    snapshot: &Path,
    reader: &DatasetReader,
    deep: bool,
) -> Result<u64, StorageError> {
    let manifest = reader.manifest();
    let directory = snapshot.join(SEARCH_DIRECTORY);
    let Some(metadata) = &manifest.search_index else {
        if fs::symlink_metadata(&directory).is_ok() {
            return Err(StorageError::Integrity(
                "Undeclared search directory".into(),
            ));
        }
        return Ok(0);
    };
    if metadata.format_version != SEARCH_FORMAT_VERSION
        || u64::from(metadata.game_count) != manifest.total_games
        || metadata.canonical_sha256 != canonical_search_fingerprint(manifest)
        || metadata.source_snapshot.is_empty()
        || metadata.sort_buffer_bytes < 24
        || metadata.coverage.max_ply() == Some(0)
        || metadata.position_count > metadata.posting_count
        || metadata.posting_count < u64::from(metadata.game_count)
    {
        return Err(StorageError::Integrity(
            "Search manifest version, coverage, counts or canonical identity are invalid".into(),
        ));
    }
    if metadata.artifacts.len() != 2
        || !metadata.artifacts.contains_key(CATALOG_FILENAME)
        || !metadata.artifacts.contains_key(POSTINGS_FILENAME)
    {
        return Err(StorageError::Integrity(
            "Unexpected search artifact set".into(),
        ));
    }
    if !fs::symlink_metadata(&directory)?.is_dir() {
        return Err(StorageError::Integrity(
            "Search artifact directory is not a real directory".into(),
        ));
    }
    for entry in fs::read_dir(&directory)? {
        let name = entry?.file_name();
        if !name
            .to_str()
            .is_some_and(|name| metadata.artifacts.contains_key(name))
        {
            return Err(StorageError::Integrity(
                "Search directory contains an undeclared artifact".into(),
            ));
        }
    }
    for (name, artifact) in &metadata.artifacts {
        if name.is_empty()
            || name.starts_with('.')
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(StorageError::Integrity(
                "Invalid search artifact name".into(),
            ));
        }
        let path = directory.join(name);
        let info = fs::symlink_metadata(&path)?;
        if !info.is_file() || info.len() != artifact.file_size_bytes {
            return Err(StorageError::Integrity(format!(
                "Search artifact '{name}' is missing or has incorrect size"
            )));
        }
        if compute_file_sha256(&path)? != artifact.sha256 {
            return Err(StorageError::Integrity(format!(
                "Search artifact '{name}' checksum mismatch"
            )));
        }
    }
    let postings = SearchPostingsReader::open(directory.join(POSTINGS_FILENAME))?;
    if postings.game_count() != u64::from(metadata.game_count)
        || postings.position_count() != metadata.position_count
        || postings.posting_count() != metadata.posting_count
    {
        return Err(StorageError::Integrity(
            "Search postings header does not match the manifest".into(),
        ));
    }
    let catalog = SearchCatalog::open(&directory.join(CATALOG_FILENAME))
        .map_err(|error| StorageError::Integrity(error.to_string()))?;
    if deep {
        catalog
            .verify_integrity()
            .map_err(|error| StorageError::Integrity(error.to_string()))?;
        postings.verify_for_coverage(metadata.coverage.max_ply())?;
        verify_catalog_ordinals(reader, &catalog, metadata)?;
    }
    metadata.file_size_bytes()
}

fn verify_catalog_ordinals(
    reader: &DatasetReader,
    catalog: &SearchCatalog,
    metadata: &SearchIndexMetadata,
) -> Result<(), StorageError> {
    let mut statement = catalog
        .connection()
        .prepare("SELECT ordinal, game_id FROM games ORDER BY ordinal")?;
    let mut projections = statement.query([])?;
    let mut expected_postings = 0u64;
    for (ordinal, game) in reader.iter_games()?.enumerate() {
        let game = game?;
        let projected = projections.next()?.ok_or_else(|| {
            StorageError::Integrity("Missing ordinal in search metadata catalog".into())
        })?;
        let projected_id: [u8; 8] = projected.get(1)?;
        if projected.get::<_, usize>(0)? != ordinal
            || u64::from_be_bytes(projected_id) != game.metadata.id
        {
            return Err(StorageError::Integrity(format!(
                "Invalid search ordinal mapping at {ordinal}"
            )));
        }
        let length = game.moves.len();
        let depth = metadata.coverage.max_ply().map_or(length, usize::from);
        expected_postings += (length.min(depth) + 1) as u64;
    }
    if projections.next()?.is_some() || metadata.posting_count != expected_postings {
        return Err(StorageError::Integrity(
            "Search ordinal or position coverage accounting differs from canonical games".into(),
        ));
    }
    Ok(())
}

/// Settings for deriving search indexes from one already-published canonical snapshot.
#[derive(Clone, Debug)]
pub struct BuildSearchConfig {
    pub coverage: Option<SearchCoverage>,
    pub memory_limit_bytes: usize,
    pub temp_directory: Option<std::path::PathBuf>,
}

impl Default for BuildSearchConfig {
    fn default() -> Self {
        Self {
            coverage: None,
            memory_limit_bytes: 128 * 1024 * 1024,
            temp_directory: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SearchBuildReport {
    pub source_snapshot: String,
    pub published_snapshot: String,
    pub collection: opening_explorer_core::Collection,
    pub index: SearchIndexMetadata,
    pub spill: opening_explorer_index::search::spill::SpillStats,
    pub postings_temporary_spool_bytes: u64,
    pub build_seconds: f64,
    pub verification_seconds: f64,
}

fn artifact_metadata(directory: &Path) -> Result<BTreeMap<String, SearchArtifact>, StorageError> {
    fs::read_dir(directory)?
        .map(|entry| {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| {
                StorageError::Integrity("Search artifact filename is not UTF-8".into())
            })?;
            let path = entry.path();
            Ok((
                name,
                SearchArtifact {
                    file_size_bytes: entry.metadata()?.len(),
                    sha256: compute_file_sha256(&path)?,
                },
            ))
        })
        .collect()
}

fn preserve_canonical_artifacts(
    source: &Path,
    staged: &Path,
    manifest: &DatasetManifest,
) -> Result<(), StorageError> {
    use crate::dataset::link_or_copy_verified_file;

    let lookup = &manifest.lookup_table;
    link_or_copy_verified_file(
        &source.join(&lookup.filename),
        &staged.join(&lookup.filename),
        lookup.file_size_bytes,
        &lookup.sha256,
    )?;
    if let Some(meta) = &manifest.source_attribution {
        link_or_copy_verified_file(
            &source.join(&meta.filename),
            &staged.join(&meta.filename),
            meta.file_size_bytes,
            &meta.sha256,
        )?;
    }
    if let Some(meta) = &manifest.position_index {
        link_or_copy_verified_file(
            &source.join(&meta.filename),
            &staged.join(&meta.filename),
            meta.file_size_bytes,
            &meta.sha256,
        )?;
    }
    fs::create_dir(staged.join("shards"))?;
    for shard in &manifest.shards {
        let name = Path::new("shards").join(&shard.filename);
        link_or_copy_verified_file(
            &source.join(&name),
            &staged.join(&name),
            shard.file_size_bytes,
            &shard.sha256,
        )?;
    }
    Ok(())
}

/// Keep the original release notices and evidence with a local derivative.
fn preserve_release_records(source: &Path, destination: &Path) -> Result<(), StorageError> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_file() {
            fs::copy(entry.path(), target)?;
        } else if kind.is_dir() {
            preserve_release_records(&entry.path(), &target)?;
        } else {
            return Err(StorageError::Integrity(
                "Unexpected non-file in source release records".into(),
            ));
        }
    }
    Ok(())
}

/// Publish a verified staged snapshot using the existing dataset version directory and pointer
/// protocol. The caller owns the dataset lock through the final directory sync.
#[cfg(unix)]
fn publish_search_stage(dataset: &Path, stage: &Path, build_id: &str) -> Result<(), StorageError> {
    use crate::{sync_dir_contents, sync_dir_itself};
    use std::os::unix::fs::symlink;

    let versions = dataset.join("versions");
    let version = versions.join(build_id);
    if version.exists() {
        return Err(StorageError::Integrity(format!(
            "Refusing to overwrite dataset version '{build_id}'"
        )));
    }
    sync_dir_contents(stage)?;
    fs::rename(stage, &version)?;
    sync_dir_itself(&versions)?;
    let current = dataset.join("current");
    let previous = fs::read_link(&current)?;
    let temporary_link = dataset.join(format!(".current_tmp_{build_id}"));
    symlink(Path::new("versions").join(build_id), &temporary_link)?;
    if let Err(error) = fs::rename(&temporary_link, &current) {
        let _ = fs::remove_file(temporary_link);
        return Err(error.into());
    }
    if let Err(error) = sync_dir_itself(dataset) {
        let rollback = (|| -> Result<(), StorageError> {
            symlink(previous, &temporary_link)?;
            fs::rename(&temporary_link, &current)?;
            sync_dir_itself(dataset)
        })();
        if let Err(rollback) = rollback {
            return Err(StorageError::Integrity(format!(
                "Search publication sync failed ({error}), and restoring the prior pointer failed ({rollback})"
            )));
        }
        return Err(error);
    }
    Ok(())
}

pub const CATALOG_FILENAME: &str = "metadata.sqlite";
pub const POSTINGS_FILENAME: &str = "positions.postings";

/// Index canonical games and publish a new version of the same local dataset.
#[cfg(unix)]
pub fn build_search_index(
    dataset: &Path,
    config: BuildSearchConfig,
) -> Result<SearchBuildReport, StorageError> {
    use crate::dataset::SourceAttributionLine;
    use crate::{
        resolve_dataset_snapshot, verify_dataset, DatasetLock, DatasetReader, StagingLock,
    };
    use opening_explorer_core::Collection;
    use opening_explorer_index::search::{
        catalog::SearchCatalogBuilder,
        postings::{SearchPosting, SearchPostingsWriter},
        spill::OccurrenceSorter,
        visit_position_occurrences,
    };
    use std::io::{BufRead, BufReader, Read};
    use std::time::Instant;

    let build_start = Instant::now();
    let dataset = fs::canonicalize(dataset)?;
    let _lock = DatasetLock::try_acquire_exclusive(&dataset)?;
    if !fs::symlink_metadata(dataset.join("current")).is_ok_and(|meta| meta.is_symlink()) {
        return Err(StorageError::Integrity(
            "Search builds require a versioned dataset root with a current symlink".into(),
        ));
    }
    let source = resolve_dataset_snapshot(&dataset)?;
    verify_dataset(&source, false)?;
    let reader = DatasetReader::open(&source)?;
    let source_manifest = reader.manifest();
    let game_count = u32::try_from(source_manifest.total_games)
        .map_err(|_| StorageError::Encoding("Search supports at most 4294967295 games".into()))?;
    let default_depth = match source_manifest.collection {
        Collection::MastersOtb => 40usize,
        Collection::EliteOnline => 30usize,
    };
    let coverage = match config.coverage {
        Some(coverage) => coverage,
        None => SearchCoverage::Bounded {
            max_ply: u16::try_from(
                source_manifest
                    .position_index
                    .as_ref()
                    .map_or(default_depth, |cpi| cpi.indexed_depth_plies),
            )
            .map_err(|_| {
                StorageError::Encoding("CPI depth exceeds 65535; use full coverage".into())
            })?,
        },
    };
    if coverage.max_ply() == Some(0) {
        return Err(StorageError::Encoding(
            "Search depth must be positive".into(),
        ));
    }

    let versions = dataset.join("versions");
    fs::create_dir_all(&versions)?;
    let stage = tempfile::Builder::new()
        .prefix(".staging_search_")
        .tempdir_in(&versions)?;
    let _stage_lock = StagingLock::acquire(stage.path())?;
    let temporary = match &config.temp_directory {
        Some(path) => {
            let path = fs::canonicalize(path)?;
            if path.starts_with(&source) || path.starts_with(&versions) {
                return Err(StorageError::Encoding(
                    "Search temporary directory must be outside snapshot versions".into(),
                ));
            }
            path
        }
        None => stage.path().to_path_buf(),
    };
    let artifacts = stage.path().join(SEARCH_DIRECTORY);
    fs::create_dir(&artifacts)?;

    let mut sorter = OccurrenceSorter::new(&temporary, config.memory_limit_bytes)?;
    let mut catalog = SearchCatalogBuilder::create(
        &artifacts.join(CATALOG_FILENAME),
        source_manifest.collection,
        u64::from(game_count),
        16,
    )
    .map_err(|error| StorageError::Database(error.to_string()))?;

    let mut streamed = 0u32;
    for game in reader.iter_games()? {
        let game = game?;
        if streamed == game_count {
            return Err(StorageError::Integrity(
                "Too many canonical games for search".into(),
            ));
        }
        catalog
            .push(&game)
            .map_err(|error| StorageError::Database(error.to_string()))?;
        visit_position_occurrences(&game, coverage.max_ply(), |occurrence| {
            sorter.push(streamed, occurrence)
        })?;
        streamed += 1;
        if streamed.is_multiple_of(100_000) {
            tracing::info!(streamed, game_count, "Streamed canonical games for search");
        }
    }
    if streamed != game_count {
        return Err(StorageError::Integrity(
            "Search traversal game count differs from the source manifest".into(),
        ));
    }

    if let Some(meta) = &source_manifest.source_attribution {
        tracing::info!("Projecting verified source attribution");
        let mut lines = BufReader::new(fs::File::open(source.join(&meta.filename))?);
        let mut buffer = Vec::new();
        let mut previous = None;
        let mut count = 0u64;
        loop {
            buffer.clear();
            let bytes = (&mut lines)
                .take((1 << 20) + 1)
                .read_until(b'\n', &mut buffer)?;
            if bytes == 0 {
                break;
            }
            if bytes > 1 << 20 {
                return Err(StorageError::Integrity(
                    "Oversized source attribution line".into(),
                ));
            }
            let line: SourceAttributionLine = serde_json::from_slice(&buffer)
                .map_err(|error| StorageError::Decoding(error.to_string()))?;
            if line.schema_version != 1 || previous.is_some_and(|id| id >= line.game_id) {
                return Err(StorageError::Integrity(
                    "Invalid source attribution order or version".into(),
                ));
            }
            catalog
                .add_attributions(line.game_id, &line.source_attributions)
                .map_err(|error| StorageError::Database(error.to_string()))?;
            previous = Some(line.game_id);
            count += 1;
        }
        if count != u64::from(game_count) {
            return Err(StorageError::Integrity(
                "Search attribution game count mismatch".into(),
            ));
        }
    }
    tracing::info!("Finishing search metadata and player and event catalogs");
    catalog
        .finish()
        .map_err(|error| StorageError::Database(error.to_string()))?;

    tracing::info!("Merging search position runs");
    let mut postings = SearchPostingsWriter::create(
        artifacts.join(POSTINGS_FILENAME),
        u64::from(game_count),
        &temporary,
    )?;
    let spill = sorter.finish(|key, ordinal, ply, played| {
        postings.push(SearchPosting {
            key,
            ordinal,
            ply,
            played,
        })
    })?;
    let stats = postings.finish()?;

    let index_metadata = SearchIndexMetadata {
        format_version: SEARCH_FORMAT_VERSION,
        coverage,
        game_count,
        position_count: stats.position_count,
        posting_count: stats.posting_count,
        source_snapshot: source_manifest.dataset_build_id.clone(),
        canonical_sha256: canonical_search_fingerprint(source_manifest),
        sort_buffer_bytes: spill.buffer_bytes,
        artifacts: artifact_metadata(&artifacts)?,
    };
    let (build_id, verification_seconds) =
        publish_index_metadata(&dataset, &reader, stage.path(), index_metadata.clone())?;
    let build_seconds = build_start.elapsed().as_secs_f64() - verification_seconds;
    Ok(SearchBuildReport {
        source_snapshot: index_metadata.source_snapshot.clone(),
        published_snapshot: build_id,
        collection: source_manifest.collection,
        index: index_metadata,
        spill,
        postings_temporary_spool_bytes: stats.temporary_spool_bytes,
        build_seconds,
        verification_seconds,
    })
}

/// Finalize locally derived or bundled search artifacts using the verified source snapshot.
/// The caller owns the dataset and staging locks and populates the stage's search directory.
#[cfg(unix)]
pub(crate) fn publish_index_metadata(
    dataset: &Path,
    reader: &DatasetReader,
    stage: &Path,
    metadata: SearchIndexMetadata,
) -> Result<(String, f64), StorageError> {
    use crate::dataset::{chrono_iso8601_now, ensure_root_symlinks};
    use crate::verify_dataset;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    let source = reader.snapshot_dir();
    let source_manifest = reader.manifest();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce: u64 = rand::random();
    let build_id = format!(
        "codb-{}-{now}-{nonce:016x}-search",
        source_manifest.collection
    );

    preserve_canonical_artifacts(source, stage, source_manifest)?;
    let artifact_permissions =
        fs::metadata(source.join(&source_manifest.lookup_table.filename))?.permissions();
    for name in [CATALOG_FILENAME, POSTINGS_FILENAME] {
        fs::set_permissions(
            stage.join(SEARCH_DIRECTORY).join(name),
            artifact_permissions.clone(),
        )?;
    }
    fs::set_permissions(stage, fs::metadata(source)?.permissions())?;
    let provenance = source.join("source-release");
    let provenance = if provenance.is_dir() {
        provenance
    } else {
        source.join("RELEASE")
    };
    if provenance.is_dir() {
        preserve_release_records(&provenance, &stage.join("source-release"))?;
    }
    let mut manifest = source_manifest.clone();
    let previous_bytes = match manifest.search_index.take() {
        Some(index) => index.file_size_bytes()?,
        None => 0,
    };
    let indexed_bytes = metadata.file_size_bytes()?;
    manifest.total_physical_bytes = manifest
        .total_physical_bytes
        .checked_sub(previous_bytes)
        .and_then(|size| size.checked_add(indexed_bytes))
        .ok_or_else(|| StorageError::Integrity("Search snapshot size overflow".into()))?;
    manifest.dataset_build_id = build_id.clone();
    manifest.build_timestamp = chrono_iso8601_now();
    manifest.search_index = Some(metadata);
    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| StorageError::Encoding(error.to_string()))?;
    fs::write(stage.join("manifest.json"), manifest_json)?;

    tracing::info!("Deeply verifying staged search snapshot");
    let verification_start = Instant::now();
    verify_dataset(stage, true)?;
    let verification_seconds = verification_start.elapsed().as_secs_f64();
    fs::remove_file(stage.join(".staging.lock"))?;
    ensure_root_symlinks(dataset)?;
    publish_search_stage(dataset, stage, &build_id)?;
    Ok((build_id, verification_seconds))
}
