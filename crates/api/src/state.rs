//! Concurrent, snapshot-consistent dataset state and pooled reader management.

use crate::config::ServerConfig;
use crate::error::ApiError;
use crate::models::CollectionMetadataResponse;
use codb_core::Collection;
use codb_index::position::DiskPositionIndex;
use codb_storage::{resolve_dataset_snapshot, DatasetManifest, DatasetReader, StorageError};
use parking_lot::{Mutex, RwLock};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{info, warn};

/// Thread-safe object pool of [`DatasetReader`] instances pinned to an immutable physical snapshot.
pub struct DatasetReaderPool {
    snapshot_dir: PathBuf,
    pool: Mutex<Vec<DatasetReader>>,
    max_size: usize,
}

impl DatasetReaderPool {
    /// Creates a new reader pool pinned to `snapshot_dir`.
    pub fn new(snapshot_dir: PathBuf, max_size: usize) -> Result<Self, StorageError> {
        // Validate by eagerly opening one reader instance
        let initial_reader = DatasetReader::open(&snapshot_dir)?;
        Ok(Self {
            snapshot_dir,
            pool: Mutex::new(vec![initial_reader]),
            max_size,
        })
    }

    /// Acquires a reader from the pool or initializes a new one pinned to the same snapshot.
    pub fn acquire(&self) -> Result<PooledReader<'_>, StorageError> {
        let maybe_reader = {
            let mut guard = self.pool.lock();
            guard.pop()
        };

        let reader = match maybe_reader {
            Some(r) => r,
            None => DatasetReader::open(&self.snapshot_dir)?,
        };

        Ok(PooledReader {
            pool: self,
            reader: Some(reader),
        })
    }

    /// Retrieves and formats a game as PGN using a pooled reader.
    pub fn get_game_pgn(&self, game_id: u64) -> Result<String, StorageError> {
        let mut pooled = self.acquire()?;
        pooled.get_game_pgn(game_id)
    }
}

/// RAII guard that automatically returns a [`DatasetReader`] to the pool when dropped.
pub struct PooledReader<'a> {
    pool: &'a DatasetReaderPool,
    reader: Option<DatasetReader>,
}

impl<'a> Deref for PooledReader<'a> {
    type Target = DatasetReader;
    fn deref(&self) -> &Self::Target {
        self.reader.as_ref().expect("Reader present")
    }
}

impl<'a> DerefMut for PooledReader<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.reader.as_mut().expect("Reader present")
    }
}

impl<'a> Drop for PooledReader<'a> {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            let mut guard = self.pool.pool.lock();
            if guard.len() < self.pool.max_size {
                guard.push(reader);
            }
        }
    }
}

/// An immutable, snapshot-consistent handle to a published dataset version.
pub struct DatasetSnapshot {
    pub collection: Collection,
    pub dataset_dir: PathBuf,
    pub snapshot_dir: PathBuf,
    pub manifest: DatasetManifest,
    pub cpi_index: Option<Arc<DiskPositionIndex>>,
    pub reader_pool: Arc<DatasetReaderPool>,
    pub loaded_at: SystemTime,
}

impl DatasetSnapshot {
    /// Opens and validates a dataset snapshot from the given dataset directory path.
    pub fn open(
        dataset_dir: &Path,
        expected_collection: Collection,
        pool_size: usize,
    ) -> Result<Self, StorageError> {
        let snapshot_dir = resolve_dataset_snapshot(dataset_dir)?;
        let manifest_path = snapshot_dir.join("manifest.json");
        if !manifest_path.exists() {
            return Err(StorageError::Decoding(format!(
                "Snapshot directory '{}' is missing manifest.json",
                snapshot_dir.display()
            )));
        }

        let manifest_bytes = std::fs::read(&manifest_path)?;
        let manifest: DatasetManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| StorageError::Decoding(format!("Invalid manifest.json: {}", e)))?;

        if manifest.collection != expected_collection {
            return Err(StorageError::Decoding(format!(
                "Dataset at '{}' belongs to collection {:?}, expected {:?}",
                dataset_dir.display(),
                manifest.collection,
                expected_collection
            )));
        }

        // Open CPI v2 index if specified in manifest
        let cpi_index = if let Some(ref cpi_meta) = manifest.position_index {
            let cpi_path = snapshot_dir.join(&cpi_meta.filename);
            if !cpi_path.exists() {
                warn!(
                    "Manifest specifies position index '{}', but file does not exist at '{}'",
                    cpi_meta.filename,
                    cpi_path.display()
                );
                None
            } else {
                let idx = DiskPositionIndex::open(&cpi_path).map_err(|e| {
                    StorageError::Decoding(format!(
                        "Failed to open CPI index at '{}': {}",
                        cpi_path.display(),
                        e
                    ))
                })?;
                Some(Arc::new(idx))
            }
        } else {
            None
        };

        let reader_pool = Arc::new(DatasetReaderPool::new(snapshot_dir.clone(), pool_size)?);

        Ok(Self {
            collection: expected_collection,
            dataset_dir: dataset_dir.to_path_buf(),
            snapshot_dir,
            manifest,
            cpi_index,
            reader_pool,
            loaded_at: SystemTime::now(),
        })
    }
}

/// Root shared application state managed by Axum.
pub struct AppState {
    pub config: Arc<ServerConfig>,
    masters_snapshot: RwLock<Option<Arc<DatasetSnapshot>>>,
    elite_snapshot: RwLock<Option<Arc<DatasetSnapshot>>>,
}

impl AppState {
    /// Initializes application state, validating and opening configured datasets at startup.
    pub fn new(config: ServerConfig) -> Result<Arc<Self>, anyhow::Error> {
        config.validate().map_err(anyhow::Error::msg)?;

        let masters = if let Some(ref path) = config.masters_dataset {
            info!("Loading Masters OTB dataset from: {}", path.display());
            let snapshot =
                DatasetSnapshot::open(path, Collection::MastersOtb, config.reader_pool_size)?;
            info!(
                "Successfully loaded Masters OTB snapshot (version: {}, games: {}, CPI: {})",
                snapshot.manifest.dataset_build_id,
                snapshot.manifest.total_games,
                snapshot.cpi_index.is_some()
            );
            Some(Arc::new(snapshot))
        } else {
            None
        };

        let elite = if let Some(ref path) = config.elite_dataset {
            info!("Loading Elite Online dataset from: {}", path.display());
            let snapshot =
                DatasetSnapshot::open(path, Collection::EliteOnline, config.reader_pool_size)?;
            info!(
                "Successfully loaded Elite Online snapshot (version: {}, games: {}, CPI: {})",
                snapshot.manifest.dataset_build_id,
                snapshot.manifest.total_games,
                snapshot.cpi_index.is_some()
            );
            Some(Arc::new(snapshot))
        } else {
            None
        };

        Ok(Arc::new(Self {
            config: Arc::new(config),
            masters_snapshot: RwLock::new(masters),
            elite_snapshot: RwLock::new(elite),
        }))
    }

    /// Returns a pinned reference to the active snapshot for `collection`.
    ///
    /// Atomic read: acquiring the read lock takes < 10ns, cloning the `Arc` keeps the snapshot
    /// alive for the entire duration of the request even if a reload or publication occurs concurrently.
    pub fn get_snapshot(&self, collection: Collection) -> Result<Arc<DatasetSnapshot>, ApiError> {
        let maybe_snap = match collection {
            Collection::MastersOtb => self.masters_snapshot.read().clone(),
            Collection::EliteOnline => self.elite_snapshot.read().clone(),
        };

        maybe_snap.ok_or_else(|| ApiError::CollectionUnavailable(collection.as_str().to_string()))
    }

    /// Atomically refreshes/reloads all configured dataset snapshots without disrupting in-flight requests.
    ///
    /// If opening a newly published snapshot fails, the error is returned immediately and the active
    /// serving snapshot remains completely untouched.
    pub fn reload(&self) -> Result<Vec<String>, anyhow::Error> {
        let mut reloaded = Vec::new();

        let new_masters = if let Some(ref path) = self.config.masters_dataset {
            info!("Reloading Masters OTB dataset from {}", path.display());
            let snap =
                DatasetSnapshot::open(path, Collection::MastersOtb, self.config.reader_pool_size)?;
            reloaded.push(format!(
                "masters-otb (version: {})",
                snap.manifest.dataset_build_id
            ));
            Some(Arc::new(snap))
        } else {
            None
        };

        let new_elite = if let Some(ref path) = self.config.elite_dataset {
            info!("Reloading Elite Online dataset from {}", path.display());
            let snap =
                DatasetSnapshot::open(path, Collection::EliteOnline, self.config.reader_pool_size)?;
            reloaded.push(format!(
                "elite-online (version: {})",
                snap.manifest.dataset_build_id
            ));
            Some(Arc::new(snap))
        } else {
            None
        };

        // All new snapshots opened successfully, perform atomic pointer swap
        if let Some(snap) = new_masters {
            *self.masters_snapshot.write() = Some(snap);
        }
        if let Some(snap) = new_elite {
            *self.elite_snapshot.write() = Some(snap);
        }

        info!("Reload completed successfully for: {:?}", reloaded);
        Ok(reloaded)
    }

    /// Returns metadata summaries for all currently loaded collections.
    pub fn available_collections(&self) -> Vec<CollectionMetadataResponse> {
        let mut result = Vec::new();

        if let Some(ref snap) = *self.masters_snapshot.read() {
            result.push(CollectionMetadataResponse {
                collection: snap.collection.as_str().to_string(),
                dataset_version: snap.manifest.dataset_build_id.clone(),
                build_timestamp: snap.manifest.build_timestamp.clone(),
                total_games: snap.manifest.total_games,
                cpi_available: snap.cpi_index.is_some(),
                cpi_depth_plies: snap
                    .manifest
                    .position_index
                    .as_ref()
                    .map(|p| p.indexed_depth_plies as u32),
                cpi_total_positions: snap
                    .manifest
                    .position_index
                    .as_ref()
                    .map(|p| p.unique_positions),
                format_version: snap.manifest.format_version.clone(),
            });
        }

        if let Some(ref snap) = *self.elite_snapshot.read() {
            result.push(CollectionMetadataResponse {
                collection: snap.collection.as_str().to_string(),
                dataset_version: snap.manifest.dataset_build_id.clone(),
                build_timestamp: snap.manifest.build_timestamp.clone(),
                total_games: snap.manifest.total_games,
                cpi_available: snap.cpi_index.is_some(),
                cpi_depth_plies: snap
                    .manifest
                    .position_index
                    .as_ref()
                    .map(|p| p.indexed_depth_plies as u32),
                cpi_total_positions: snap
                    .manifest
                    .position_index
                    .as_ref()
                    .map(|p| p.unique_positions),
                format_version: snap.manifest.format_version.clone(),
            });
        }

        result
    }

    /// Returns a list of active collection names.
    pub fn ready_collections(&self) -> Vec<String> {
        let mut list = Vec::new();
        if self.masters_snapshot.read().is_some() {
            list.push("masters-otb".to_string());
        }
        if self.elite_snapshot.read().is_some() {
            list.push("elite-online".to_string());
        }
        list
    }
}
