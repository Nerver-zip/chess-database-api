//! Concurrent, snapshot-consistent dataset state and bounded pooled reader management.

use crate::config::ServerConfig;
use crate::error::ApiError;
use crate::models::{CollectionCapabilities, CollectionMetadataResponse};
use crate::search_runtime::{SearchOptions, SearchRuntime};
use opening_explorer_core::Collection;
use opening_explorer_index::position::DiskPositionIndex;
use opening_explorer_index::search::{
    catalog::SearchCatalog, control::SearchControl, postings::SearchPostingsReader,
};
use opening_explorer_storage::search::{CATALOG_FILENAME, POSTINGS_FILENAME, SEARCH_DIRECTORY};
use opening_explorer_storage::{
    resolve_dataset_snapshot, DatasetManifest, DatasetReader, GameLookupReader, StorageError,
};
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Semaphore;
use tracing::info;

/// Thread-safe, strictly bounded object pool of [`DatasetReader`] instances pinned to an immutable physical snapshot.
///
/// Enforces a hard upper bound on concurrent readers via an asynchronous semaphore,
/// preventing resource exhaustion under arbitrary client concurrency.
pub struct DatasetReaderPool {
    snapshot_dir: PathBuf,
    collection: Collection,
    idle_readers: Mutex<Vec<DatasetReader>>,
    semaphore: Arc<Semaphore>,
    capacity: usize,
    active_readers: AtomicUsize,
    peak_active_readers: AtomicUsize,
}

impl DatasetReaderPool {
    /// Creates a new reader pool pinned to `snapshot_dir` with a strict `capacity` ceiling.
    pub fn new(
        snapshot_dir: PathBuf,
        capacity: usize,
        collection: Collection,
    ) -> Result<Self, StorageError> {
        let capacity = capacity.max(1);
        // Eagerly validate that we can construct a reader instance
        let initial_reader = DatasetReader::open(&snapshot_dir)?;
        Ok(Self {
            snapshot_dir,
            collection,
            idle_readers: Mutex::new(vec![initial_reader]),
            semaphore: Arc::new(Semaphore::new(capacity)),
            capacity,
            active_readers: AtomicUsize::new(0),
            peak_active_readers: AtomicUsize::new(0),
        })
    }

    /// Returns the currently active (checked-out) reader count.
    pub fn active_readers(&self) -> usize {
        self.active_readers.load(Ordering::Relaxed)
    }

    /// Returns the peak active readers observed over the lifetime of this pool.
    pub fn peak_active_readers(&self) -> usize {
        self.peak_active_readers.load(Ordering::Relaxed)
    }

    /// Returns the maximum capacity of this reader pool.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Asynchronously acquires a reader permit, offloads CGB3 seek and decompression to
    /// `tokio::task::spawn_blocking`, and returns the canonical PGN string.
    ///
    /// Respects pool capacity: if all permits are occupied, requests wait asynchronously up to `timeout`.
    /// Waiting requests do NOT block Tokio executor threads.
    /// Permits and reader instances are safely returned even during errors or panics.
    pub async fn get_game_pgn(
        self: &Arc<Self>,
        game_id: u64,
        timeout: Duration,
    ) -> Result<String, ApiError> {
        let permit =
            match tokio::time::timeout(timeout, self.semaphore.clone().acquire_owned()).await {
                Ok(Ok(p)) => p,
                Ok(Err(_)) => return Err(ApiError::Internal("Reader pool closed".to_string())),
                Err(_) => {
                    return Err(ApiError::PoolTimeout(format!(
                        "Reader pool acquisition timed out after {:?}",
                        timeout
                    )));
                }
            };

        let current_active = self.active_readers.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_active_readers
            .fetch_max(current_active, Ordering::SeqCst);

        let pool = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            struct ReaderGuard {
                pool: Arc<DatasetReaderPool>,
                permit: Option<tokio::sync::OwnedSemaphorePermit>,
                reader: Option<DatasetReader>,
            }
            impl Drop for ReaderGuard {
                fn drop(&mut self) {
                    self.pool.active_readers.fetch_sub(1, Ordering::SeqCst);
                    if let Some(r) = self.reader.take() {
                        let mut idle = self.pool.idle_readers.lock();
                        if idle.len() < self.pool.capacity {
                            idle.push(r);
                        }
                    }
                    drop(self.permit.take());
                }
            }

            let maybe_idle = {
                let mut idle = pool.idle_readers.lock();
                idle.pop()
            };

            let reader = match maybe_idle {
                Some(r) => r,
                None => match DatasetReader::open(&pool.snapshot_dir) {
                    Ok(r) => r,
                    Err(e) => {
                        return Err(ApiError::Storage(format!("Failed to open reader: {}", e)))
                    }
                },
            };

            let mut guard = ReaderGuard {
                pool: Arc::clone(&pool),
                permit: Some(permit),
                reader: Some(reader),
            };

            guard
                .reader
                .as_mut()
                .unwrap()
                .get_game_pgn(game_id)
                .map_err(|e| match e {
                    // Name the collection, not the server's snapshot path.
                    StorageError::GameNotFound(_) => {
                        ApiError::GameNotFound(game_id, guard.pool.collection.as_str().to_string())
                    }
                    other => ApiError::Storage(format!("Game retrieval failed: {}", other)),
                })
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Worker thread join error: {}", e)))?
    }
}

/// An immutable, snapshot-consistent handle to a published dataset version.
pub struct DatasetSnapshot {
    pub collection: Collection,
    pub dataset_dir: PathBuf,
    pub snapshot_dir: PathBuf,
    pub manifest: DatasetManifest,
    pub lookup_reader: Arc<GameLookupReader>,
    pub cpi_index: Option<Arc<DiskPositionIndex>>,
    pub(crate) search: Option<SnapshotSearch>,
    pub reader_pool: Arc<DatasetReaderPool>,
    pub loaded_at: SystemTime,
}

pub(crate) struct SnapshotSearch {
    pub(crate) postings: SearchPostingsReader,
    catalog: Mutex<Vec<SearchCatalog>>,
}

impl SnapshotSearch {
    pub(crate) fn with_catalog<T>(
        &self,
        control: &SearchControl,
        operation: impl FnOnce(&SearchCatalog) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        struct CatalogGuard<'a> {
            pool: &'a Mutex<Vec<SearchCatalog>>,
            catalog: Option<SearchCatalog>,
        }
        impl Drop for CatalogGuard<'_> {
            fn drop(&mut self) {
                if let Some(catalog) = self.catalog.take() {
                    self.pool.lock().push(catalog);
                }
            }
        }

        let catalog = self.catalog.lock().pop().ok_or(ApiError::SearchBusy)?;
        let guard = CatalogGuard {
            pool: &self.catalog,
            catalog: Some(catalog),
        };
        let catalog = guard.catalog.as_ref().expect("leased search catalog");
        control.interrupt_sqlite(catalog.connection());
        operation(catalog)
    }
}

impl DatasetSnapshot {
    pub(crate) fn filtered_explorer_depth(&self) -> Option<usize> {
        let search = self.manifest.search_index.as_ref()?;
        let cpi = self.manifest.position_index.as_ref()?;
        if search
            .coverage
            .max_ply()
            .map_or(true, |depth| usize::from(depth) >= cpi.indexed_depth_plies)
        {
            Some(cpi.indexed_depth_plies)
        } else {
            None
        }
    }

    /// Checks whether the snapshot contains the given 64-bit stable game ID via `games.idx` binary search.
    pub fn contains_game(&self, game_id: u64) -> bool {
        !self.lookup_reader.find_locations(game_id).is_empty()
    }

    /// Opens and validates a dataset snapshot from the given dataset directory path.
    ///
    /// Validates `manifest.json` accounting, SHA-256 digests of `games.idx`, shard files,
    /// and `positions.cpi`, as well as internal CPI sparse index integrity.
    pub fn open_and_validate(
        dataset_dir: &Path,
        expected_collection: Collection,
        pool_size: usize,
        deep_validate: bool,
    ) -> Result<Self, StorageError> {
        Self::open_and_validate_with_search(
            dataset_dir,
            expected_collection,
            pool_size,
            deep_validate,
            1,
        )
    }

    fn open_and_validate_with_search(
        dataset_dir: &Path,
        expected_collection: Collection,
        pool_size: usize,
        deep_validate: bool,
        search_slots: usize,
    ) -> Result<Self, StorageError> {
        let snapshot_dir = resolve_dataset_snapshot(dataset_dir)?;
        let manifest_path = snapshot_dir.join("manifest.json");
        if !manifest_path.exists() {
            return Err(StorageError::Decoding(format!(
                "Snapshot directory '{}' is missing manifest.json",
                snapshot_dir.display()
            )));
        }

        // 1. Verify dataset integrity before opening structures!
        // This validates sha256 checksums of games.idx, shard_XXXX.cgb3, and positions.cpi!
        let _report = opening_explorer_storage::verify_dataset(&snapshot_dir, deep_validate)?;

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

        // 2. Open and verify game lookup table
        let lookup_path = snapshot_dir.join(&manifest.lookup_table.filename);
        let lookup_reader = Arc::new(GameLookupReader::open(&lookup_path)?);

        // 3. Open and verify CPI v2 index integrity if present
        let cpi_index = if let Some(ref cpi_meta) = manifest.position_index {
            let cpi_path = snapshot_dir.join(&cpi_meta.filename);
            if !cpi_path.exists() {
                return Err(StorageError::Integrity(format!(
                    "Manifest declares position index '{}', but file is missing",
                    cpi_meta.filename
                )));
            }
            let idx = DiskPositionIndex::open(&cpi_path).map_err(|e| {
                StorageError::Decoding(format!(
                    "Failed to open CPI index at '{}': {}",
                    cpi_path.display(),
                    e
                ))
            })?;
            // Perform CPI internal integrity verification (headers, sparse index, block bounds)
            idx.verify_integrity().map_err(|e| {
                StorageError::Integrity(format!(
                    "CPI index at '{}' failed integrity verification: {}",
                    cpi_path.display(),
                    e
                ))
            })?;
            Some(Arc::new(idx))
        } else {
            None
        };

        let search = if manifest.search_index.is_some() {
            let directory = snapshot_dir.join(SEARCH_DIRECTORY);
            let postings = SearchPostingsReader::open(directory.join(POSTINGS_FILENAME))?;
            let catalog_path = directory.join(CATALOG_FILENAME);
            let mut catalog = Vec::with_capacity(search_slots);
            for _ in 0..search_slots {
                catalog.push(
                    SearchCatalog::open(&catalog_path)
                        .map_err(|error| StorageError::Integrity(error.to_string()))?,
                );
            }
            Some(SnapshotSearch {
                postings,
                catalog: Mutex::new(catalog),
            })
        } else {
            None
        };

        let reader_pool = Arc::new(DatasetReaderPool::new(
            snapshot_dir.clone(),
            pool_size,
            expected_collection,
        )?);

        Ok(Self {
            collection: expected_collection,
            dataset_dir: dataset_dir.to_path_buf(),
            snapshot_dir,
            manifest,
            lookup_reader,
            cpi_index,
            search,
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
    reload_lock: tokio::sync::Mutex<()>,
    pub(crate) search: SearchRuntime,
}

impl AppState {
    /// Initializes application state, validating and opening configured datasets at startup.
    pub fn new(config: ServerConfig) -> Result<Arc<Self>, anyhow::Error> {
        Self::new_with_search(config, SearchOptions::default())
    }

    pub fn new_with_search(
        config: ServerConfig,
        search: SearchOptions,
    ) -> Result<Arc<Self>, anyhow::Error> {
        config.validate().map_err(anyhow::Error::msg)?;
        let search = SearchRuntime::new(search)?;

        let masters = if let Some(ref path) = config.masters_dataset {
            info!("Loading Masters OTB dataset from: {}", path.display());
            let snapshot = DatasetSnapshot::open_and_validate_with_search(
                path,
                Collection::MastersOtb,
                config.reader_pool_size,
                config.deep_validate_on_startup,
                search.options.concurrency,
            )?;
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
            let snapshot = DatasetSnapshot::open_and_validate_with_search(
                path,
                Collection::EliteOnline,
                config.reader_pool_size,
                config.deep_validate_on_startup,
                search.options.concurrency,
            )?;
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
            reload_lock: tokio::sync::Mutex::new(()),
            search,
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

    /// Atomically refreshes/reloads all configured dataset snapshots with strict integrity validation.
    ///
    /// Validates replacement snapshots in a worker thread before activating them.
    /// If opening or verifying a replacement fails, the error is logged and returned, and the active
    /// serving snapshot remains completely untouched.
    /// Concurrent reloads are serialized via an internal mutex.
    pub async fn reload(&self) -> Result<Vec<String>, ApiError> {
        let _guard = self.reload_lock.lock().await;

        let config = Arc::clone(&self.config);
        let search_slots = self.search.options.concurrency;
        let (new_masters, new_elite, reloaded) = tokio::task::spawn_blocking(move || {
            let mut reloaded = Vec::new();

            let new_masters = if let Some(ref path) = config.masters_dataset {
                info!(
                    "Validating replacement Masters OTB snapshot from {}",
                    path.display()
                );
                let snap = DatasetSnapshot::open_and_validate_with_search(
                    path,
                    Collection::MastersOtb,
                    config.reader_pool_size,
                    config.deep_validate_on_startup,
                    search_slots,
                )
                .map_err(|e| {
                    tracing::error!("Masters OTB replacement snapshot validation failed: {}", e);
                    ApiError::ReloadFailed(format!("masters-otb validation failed: {}", e))
                })?;
                reloaded.push(format!(
                    "masters-otb (version: {})",
                    snap.manifest.dataset_build_id
                ));
                Some(Arc::new(snap))
            } else {
                None
            };

            let new_elite = if let Some(ref path) = config.elite_dataset {
                info!(
                    "Validating replacement Elite Online snapshot from {}",
                    path.display()
                );
                let snap = DatasetSnapshot::open_and_validate_with_search(
                    path,
                    Collection::EliteOnline,
                    config.reader_pool_size,
                    config.deep_validate_on_startup,
                    search_slots,
                )
                .map_err(|e| {
                    tracing::error!("Elite Online replacement snapshot validation failed: {}", e);
                    ApiError::ReloadFailed(format!("elite-online validation failed: {}", e))
                })?;
                reloaded.push(format!(
                    "elite-online (version: {})",
                    snap.manifest.dataset_build_id
                ));
                Some(Arc::new(snap))
            } else {
                None
            };

            Ok::<_, ApiError>((new_masters, new_elite, reloaded))
        })
        .await
        .map_err(|e| {
            ApiError::Internal(format!("Worker thread join error during reload: {}", e))
        })??;

        // All new snapshots validated successfully! Perform atomic pointer swap
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
    fn capabilities(&self, snapshot: &DatasetSnapshot) -> CollectionCapabilities {
        let metadata = snapshot.manifest.search_index.as_ref();
        let discovery = metadata.is_some() && self.search.options.discovery(&snapshot.collection);
        CollectionCapabilities {
            opening_explorer: snapshot.cpi_index.is_some(),
            game_retrieval: self.config.enable_game_retrieval,
            game_search: discovery,
            position_search: discovery,
            player_search: discovery,
            event_search: discovery,
            filtered_explorer: discovery && snapshot.filtered_explorer_depth().is_some(),
            game_metadata: metadata.is_some()
                && self.search.options.game_metadata(&snapshot.collection),
            position_search_coverage: metadata.filter(|_| discovery).map(|index| index.coverage),
            game_filter_families: if discovery {
                [
                    "player",
                    "side",
                    "matchup",
                    "rating",
                    "date",
                    "result",
                    "event",
                    "site",
                    "eco",
                    "time_control_category",
                    "plies",
                    "position",
                    "continuation",
                ]
                .iter()
                .map(|s| (*s).into())
                .collect()
            } else {
                Vec::new()
            },
        }
    }

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
                capabilities: self.capabilities(snap),
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
                capabilities: self.capabilities(snap),
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
