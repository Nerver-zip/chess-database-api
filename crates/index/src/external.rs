use crate::position::{
    extract_game_positions_detailed, CpiMetadata, IndexError, StreamingCpiWriter,
    DEFAULT_POSITIONS_PER_BLOCK,
};
use byteorder::{BigEndian, ReadBytesExt};
use codb_core::{CompactMove, GameResult, ParsedGame, PositionKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const CRUN_MAGIC: &[u8; 8] = b"CODB_RUN";
pub const CRUN_VERSION: u32 = 1;
pub const CRUN_HEADER_SIZE: usize = 12; // 8 magic + 4 version
pub const CRUN_RECORD_SIZE: usize = 30; // 16 key + 2 move + 4 w + 4 d + 4 b
pub const CRUN_FOOTER_SIZE: usize = 40; // 8 record_count + 32-byte SHA-256 digest

pub const DEFAULT_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024; // 128 MiB
pub const DEFAULT_MAX_MERGE_FAN_IN: usize = 16;
pub const DEFAULT_CPI_ZSTD_LEVEL: i32 = 3;

/// Fault injection points for testing fail-closed recovery and staging cleanup during CPI construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalIndexFaultPoint {
    /// Abort while flushing or generating sorted temporary runs.
    DuringRunGeneration,
    /// Abort during the external K-way merge before CPI finalization.
    DuringExternalMerge,
    /// Abort after writing CPI blocks but before finalizing the CPI header.
    BeforeCpiCompletion,
}

/// Configuration for [`ExternalPositionIndexer`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalIndexConfig {
    /// Maximum plies per game to extract into the opening position index.
    pub max_plies: usize,
    /// Approximate memory ceiling in bytes for the in-memory accumulation buffer.
    pub memory_limit_bytes: usize,
    /// Maximum number of sorted run files opened simultaneously during a single merge pass (>= 2).
    pub max_merge_fan_in: usize,
    /// Number of positions packed into each Zstd-compressed CPI block.
    pub positions_per_block: usize,
    /// Zstd compression level for CPI blocks.
    pub zstd_level: i32,
    /// Optional fault injection point for crash/interruption testing.
    pub fault_point: Option<ExternalIndexFaultPoint>,
}

impl Default for ExternalIndexConfig {
    fn default() -> Self {
        Self {
            max_plies: 40,
            memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
            max_merge_fan_in: DEFAULT_MAX_MERGE_FAN_IN,
            positions_per_block: DEFAULT_POSITIONS_PER_BLOCK,
            zstd_level: DEFAULT_CPI_ZSTD_LEVEL,
            fault_point: None,
        }
    }
}

/// Complete accounting and resource telemetry produced by [`ExternalPositionIndexer`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalIndexStats {
    pub games_processed: u64,
    pub games_indexed: u64,
    pub excluded_unknown_result_games: u64,
    pub custom_fen_games: u64,
    pub repeated_positions_skipped: u64,
    pub raw_observations_extracted: u64,
    pub runs_generated: usize,
    pub merge_passes: usize,
    pub peak_temp_bytes: u64,
    pub cpi_metadata: CpiMetadata,
}

/// Compact 24-byte in-memory observation buffered before sorting and run coalescing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawObservation {
    key: u128,
    compact_move: u16,
    result: GameResult,
}

/// Coalesced 30-byte record stored in sorted `.crun` temporary run files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunRecord {
    pub key: u128,
    pub compact_move: u16,
    pub white_wins: u32,
    pub draws: u32,
    pub black_wins: u32,
}

impl PartialOrd for RunRecord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RunRecord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.key, self.compact_move).cmp(&(other.key, other.compact_move))
    }
}

/// Writes a sorted, pre-coalesced `.crun` temporary run file with a trailing record count and SHA-256 integrity digest.
struct RunWriter {
    writer: BufWriter<File>,
    hasher: Sha256,
    record_count: u64,
    prev_key_move: Option<(u128, u16)>,
}

impl RunWriter {
    fn create(path: &Path) -> Result<Self, IndexError> {
        let file = File::create(path)?;
        let mut writer = BufWriter::with_capacity(256 * 1024, file);
        let mut hasher = Sha256::new();

        let mut header = [0u8; CRUN_HEADER_SIZE];
        header[0..8].copy_from_slice(CRUN_MAGIC);
        header[8..12].copy_from_slice(&CRUN_VERSION.to_be_bytes());

        writer.write_all(&header)?;
        hasher.update(header);

        Ok(Self {
            writer,
            hasher,
            record_count: 0,
            prev_key_move: None,
        })
    }

    fn write_record(&mut self, rec: &RunRecord) -> Result<(), IndexError> {
        if let Some(prev) = self.prev_key_move {
            if (rec.key, rec.compact_move) <= prev {
                return Err(IndexError::InvalidFormat(format!(
                    "RunWriter requires strictly increasing (key, move): ({:032x}, {}) <= ({:032x}, {})",
                    rec.key, rec.compact_move, prev.0, prev.1
                )));
            }
        }
        let total = rec
            .white_wins
            .checked_add(rec.draws)
            .and_then(|s| s.checked_add(rec.black_wins))
            .ok_or_else(|| IndexError::Overflow("RunRecord W/D/L sum overflow".into()))?;
        if total == 0 {
            return Err(IndexError::InvalidFormat(
                "RunWriter rejecting zero-game RunRecord".into(),
            ));
        }

        let mut buf = [0u8; CRUN_RECORD_SIZE];
        buf[0..16].copy_from_slice(&rec.key.to_be_bytes());
        buf[16..18].copy_from_slice(&rec.compact_move.to_be_bytes());
        buf[18..22].copy_from_slice(&rec.white_wins.to_be_bytes());
        buf[22..26].copy_from_slice(&rec.draws.to_be_bytes());
        buf[26..30].copy_from_slice(&rec.black_wins.to_be_bytes());

        self.writer.write_all(&buf)?;
        self.hasher.update(buf);
        self.record_count += 1;
        self.prev_key_move = Some((rec.key, rec.compact_move));
        Ok(())
    }

    fn finish(mut self) -> Result<u64, IndexError> {
        let count_bytes = self.record_count.to_be_bytes();
        self.writer.write_all(&count_bytes)?;
        self.hasher.update(count_bytes);

        let digest = self.hasher.finalize();
        self.writer.write_all(&digest)?;
        self.writer.flush()?;
        let total_bytes = (CRUN_HEADER_SIZE as u64)
            + self.record_count * (CRUN_RECORD_SIZE as u64)
            + (CRUN_FOOTER_SIZE as u64);
        Ok(total_bytes)
    }
}

/// Verified streaming reader for `.crun` temporary run files.
struct RunReader {
    reader: BufReader<File>,
    remaining_records: u64,
    prev_key_move: Option<(u128, u16)>,
}

impl RunReader {
    /// Opens a `.crun` file, validating its header, exact byte length, and full SHA-256 footer
    /// before allowing any records to be consumed.
    fn open_and_verify(path: &Path) -> Result<Self, IndexError> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < (CRUN_HEADER_SIZE + CRUN_FOOTER_SIZE) as u64 {
            return Err(IndexError::InvalidFormat(format!(
                "Run file {:?} is truncated ({} bytes)",
                path, file_len
            )));
        }

        // Read footer (record_count + expected_digest)
        file.seek(SeekFrom::Start(file_len - (CRUN_FOOTER_SIZE as u64)))?;
        let mut footer = [0u8; CRUN_FOOTER_SIZE];
        file.read_exact(&mut footer)?;
        let record_count = u64::from_be_bytes(footer[0..8].try_into().unwrap());
        let expected_digest = &footer[8..40];

        let body_bytes = record_count
            .checked_mul(CRUN_RECORD_SIZE as u64)
            .ok_or_else(|| IndexError::InvalidFormat("Run body size overflow".into()))?;
        let expected_file_len = (CRUN_HEADER_SIZE as u64)
            .checked_add(body_bytes)
            .and_then(|s| s.checked_add(CRUN_FOOTER_SIZE as u64))
            .ok_or_else(|| IndexError::InvalidFormat("Run file size overflow".into()))?;

        if file_len != expected_file_len {
            return Err(IndexError::InvalidFormat(format!(
                "Run file {:?} length mismatch: expected {} bytes for {} records, got {}",
                path, expected_file_len, record_count, file_len
            )));
        }

        file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(128 * 1024, file);
        let mut header = [0u8; CRUN_HEADER_SIZE];
        reader.read_exact(&mut header)?;
        if &header[0..8] != CRUN_MAGIC {
            return Err(IndexError::InvalidFormat(format!(
                "Invalid magic in run file {:?}",
                path
            )));
        }
        let version = u32::from_be_bytes(header[8..12].try_into().unwrap());
        if version != CRUN_VERSION {
            return Err(IndexError::InvalidFormat(format!(
                "Unsupported run version {} in {:?}",
                version, path
            )));
        }

        // Verify SHA-256 digest over header + body + record_count (everything before the 32-byte digest)
        let mut hasher = Sha256::new();
        hasher.update(header);
        let mut rem = body_bytes + 8;
        let mut buf = [0u8; 64 * 1024];
        while rem > 0 {
            let to_read = (rem as usize).min(buf.len());
            reader.read_exact(&mut buf[..to_read])?;
            hasher.update(&buf[..to_read]);
            rem -= to_read as u64;
        }
        let actual_digest = hasher.finalize();
        if actual_digest.as_slice() != expected_digest {
            return Err(IndexError::InvalidFormat(format!(
                "SHA-256 integrity mismatch in temporary run file {:?}",
                path
            )));
        }

        // Rewind to beginning of record stream
        let mut file = reader.into_inner();
        file.seek(SeekFrom::Start(CRUN_HEADER_SIZE as u64))?;
        Ok(Self {
            reader: BufReader::with_capacity(64 * 1024, file),
            remaining_records: record_count,
            prev_key_move: None,
        })
    }

    fn next_record(&mut self) -> Result<Option<RunRecord>, IndexError> {
        if self.remaining_records == 0 {
            return Ok(None);
        }
        let key = self.reader.read_u128::<BigEndian>()?;
        let compact_move = self.reader.read_u16::<BigEndian>()?;
        let white_wins = self.reader.read_u32::<BigEndian>()?;
        let draws = self.reader.read_u32::<BigEndian>()?;
        let black_wins = self.reader.read_u32::<BigEndian>()?;
        self.remaining_records -= 1;

        if let Some(prev) = self.prev_key_move {
            if (key, compact_move) <= prev {
                return Err(IndexError::InvalidFormat(format!(
                    "Unsorted record in temporary run: ({:032x}, {}) <= ({:032x}, {})",
                    key, compact_move, prev.0, prev.1
                )));
            }
        }
        let total = white_wins
            .checked_add(draws)
            .and_then(|s| s.checked_add(black_wins))
            .ok_or_else(|| IndexError::InvalidFormat("RunRecord W/D/L sum overflow".into()))?;
        if total == 0 {
            return Err(IndexError::InvalidFormat(
                "Zero-game record encountered in temporary run".into(),
            ));
        }

        self.prev_key_move = Some((key, compact_move));
        Ok(Some(RunRecord {
            key,
            compact_move,
            white_wins,
            draws,
            black_wins,
        }))
    }
}

/// Bounded-memory external-sorting position indexer.
///
/// Streams games one at a time, buffers extracted `(PositionKey, CompactMove, GameResult)` records
/// up to a configurable memory budget, flushes sorted and pre-coalesced `.crun` temporary run files
/// to an explicit staging directory, and performs a bounded-fan-in multi-pass K-way merge directly
/// into [`StreamingCpiWriter`].
pub struct ExternalPositionIndexer {
    config: ExternalIndexConfig,
    staging_dir: PathBuf,
    buffer: Vec<RawObservation>,
    max_buffer_records: usize,
    run_paths: Vec<PathBuf>,
    current_temp_bytes: u64,
    peak_temp_bytes: u64,
    games_processed: u64,
    games_indexed: u64,
    excluded_unknown_result_games: u64,
    custom_fen_games: u64,
    repeated_positions_skipped: u64,
    raw_observations_extracted: u64,
}

impl ExternalPositionIndexer {
    pub fn new(staging_dir: &Path, config: ExternalIndexConfig) -> Result<Self, IndexError> {
        if config.max_merge_fan_in < 2 {
            return Err(IndexError::InvalidFormat(format!(
                "max_merge_fan_in must be >= 2 (got {})",
                config.max_merge_fan_in
            )));
        }
        if config.memory_limit_bytes == 0 {
            return Err(IndexError::InvalidFormat(
                "memory_limit_bytes must be > 0".into(),
            ));
        }
        fs::create_dir_all(staging_dir)?;

        // Explicitly partition configured memory ceiling into:
        // 1. Observation buffer for in-memory RawObservations (std::mem::size_of::<RawObservation>() = 32 bytes).
        // 2. Headroom reserved for fixed runtime overhead (I/O buffers for run writers and readers,
        //    zstd compression buffers, allocation margin, and heap structures).
        // For production budgets (>= 16 MiB), reserve min(16 MiB, budget / 8).
        // For smaller test budgets (< 16 MiB), reserve a fractional proportion with a minimum of 1 record.
        let reserved_bytes = if config.memory_limit_bytes >= 16 * 1024 * 1024 {
            (config.memory_limit_bytes / 8).min(16 * 1024 * 1024)
        } else {
            (config.memory_limit_bytes / 4).max(std::mem::size_of::<RawObservation>())
        };
        let observation_budget_bytes = config.memory_limit_bytes.saturating_sub(reserved_bytes);
        let max_buffer_records =
            (observation_budget_bytes / std::mem::size_of::<RawObservation>()).max(1);
        let initial_cap = max_buffer_records.min(262_144);

        Ok(Self {
            config,
            staging_dir: staging_dir.to_path_buf(),
            buffer: Vec::with_capacity(initial_cap),
            max_buffer_records,
            run_paths: Vec::new(),
            current_temp_bytes: 0,
            peak_temp_bytes: 0,
            games_processed: 0,
            games_indexed: 0,
            excluded_unknown_result_games: 0,
            custom_fen_games: 0,
            repeated_positions_skipped: 0,
            raw_observations_extracted: 0,
        })
    }

    /// Processes a single canonical game, extracting opening positions up to `config.max_plies`
    /// and flushing sorted temporary runs whenever the in-memory buffer reaches capacity.
    pub fn push_game(&mut self, game: &ParsedGame) -> Result<(), IndexError> {
        let extraction = extract_game_positions_detailed(game, self.config.max_plies)?;
        self.games_processed = self
            .games_processed
            .checked_add(1)
            .ok_or_else(|| IndexError::Overflow("games_processed overflow".into()))?;

        if extraction.custom_starting_fen {
            self.custom_fen_games += 1;
        }
        self.repeated_positions_skipped = self
            .repeated_positions_skipped
            .checked_add(extraction.repeated_positions_skipped as u64)
            .ok_or_else(|| IndexError::Overflow("repeated_positions_skipped overflow".into()))?;

        if extraction.excluded_unknown_result {
            self.excluded_unknown_result_games += 1;
            return Ok(());
        }

        self.games_indexed += 1;
        self.raw_observations_extracted = self
            .raw_observations_extracted
            .checked_add(extraction.entries.len() as u64)
            .ok_or_else(|| IndexError::Overflow("raw_observations_extracted overflow".into()))?;

        for (key, cm, result) in extraction.entries {
            self.buffer.push(RawObservation {
                key: key.0,
                compact_move: cm.0,
                result,
            });
            if self.buffer.len() >= self.max_buffer_records {
                self.flush_buffer_to_run()?;
            }
        }

        Ok(())
    }

    fn flush_buffer_to_run(&mut self) -> Result<(), IndexError> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        if self.config.fault_point == Some(ExternalIndexFaultPoint::DuringRunGeneration) {
            // Write a partial/unfinalized run file to verify fail-closed cleanup of interrupted runs
            let run_idx = self.run_paths.len();
            let partial_path = self.staging_dir.join(format!("run_{:06}.crun", run_idx));
            let mut f = File::create(&partial_path)?;
            f.write_all(CRUN_MAGIC)?;
            f.flush()?;
            return Err(IndexError::InvalidFormat(
                "Injected fault at DuringRunGeneration".into(),
            ));
        }

        // Sort in-place by (PositionKey, CompactMove) without allocating
        self.buffer
            .sort_unstable_by_key(|obs| (obs.key, obs.compact_move));

        let run_idx = self.run_paths.len();
        let run_path = self.staging_dir.join(format!("run_{:06}.crun", run_idx));
        let mut writer = RunWriter::create(&run_path)?;

        // Coalesce identical (key, compact_move) observations and stream directly into RunWriter.
        // Draining self.buffer in-place prevents allocating an intermediate Vec<RunRecord>.
        let mut current_record: Option<RunRecord> = None;
        for obs in self.buffer.drain(..) {
            let (w_inc, d_inc, b_inc) = match obs.result {
                GameResult::WhiteWins => (1u32, 0u32, 0u32),
                GameResult::Draw => (0u32, 1u32, 0u32),
                GameResult::BlackWins => (0u32, 0u32, 1u32),
                GameResult::Star => continue,
            };

            if let Some(ref mut cur) = current_record {
                if cur.key == obs.key && cur.compact_move == obs.compact_move {
                    cur.white_wins = cur
                        .white_wins
                        .checked_add(w_inc)
                        .ok_or_else(|| IndexError::Overflow("Run white_wins overflow".into()))?;
                    cur.draws = cur
                        .draws
                        .checked_add(d_inc)
                        .ok_or_else(|| IndexError::Overflow("Run draws overflow".into()))?;
                    cur.black_wins = cur
                        .black_wins
                        .checked_add(b_inc)
                        .ok_or_else(|| IndexError::Overflow("Run black_wins overflow".into()))?;
                    continue;
                } else {
                    writer.write_record(cur)?;
                }
            }

            current_record = Some(RunRecord {
                key: obs.key,
                compact_move: obs.compact_move,
                white_wins: w_inc,
                draws: d_inc,
                black_wins: b_inc,
            });
        }

        if let Some(ref cur) = current_record {
            writer.write_record(cur)?;
        }

        if writer.record_count == 0 {
            // Nothing was written (e.g. only Star games), cleanup empty run file
            let _ = fs::remove_file(&run_path);
            return Ok(());
        }

        let run_bytes = writer.finish()?;

        self.current_temp_bytes = self.current_temp_bytes.saturating_add(run_bytes);
        if self.current_temp_bytes > self.peak_temp_bytes {
            self.peak_temp_bytes = self.current_temp_bytes;
        }
        self.run_paths.push(run_path);
        Ok(())
    }

    /// Finalizes all buffered observations, performs bounded-fan-in multi-pass K-way merging,
    /// writes the final `.cpi` artifact to `output_cpi_path`, removes temporary run files,
    /// and returns [`ExternalIndexStats`].
    pub fn finish(mut self, output_cpi_path: &Path) -> Result<ExternalIndexStats, IndexError> {
        if !self.buffer.is_empty() {
            self.flush_buffer_to_run()?;
        }
        // Release in-memory observation buffer capacity before starting external merge
        self.buffer = Vec::new();
        self.buffer.shrink_to_fit();

        let runs_generated = self.run_paths.len();
        let mut active_runs = self.run_paths.clone();
        let mut merge_passes = 0usize;

        // Multi-pass intermediate merging when run count exceeds max_merge_fan_in
        while active_runs.len() > self.config.max_merge_fan_in {
            merge_passes += 1;
            let mut next_runs = Vec::new();

            for (chunk_idx, chunk) in active_runs.chunks(self.config.max_merge_fan_in).enumerate() {
                if chunk.len() == 1 {
                    next_runs.push(chunk[0].clone());
                    continue;
                }

                if self.config.fault_point == Some(ExternalIndexFaultPoint::DuringExternalMerge) {
                    return Err(IndexError::InvalidFormat(
                        "Injected fault at DuringExternalMerge (intermediate pass)".into(),
                    ));
                }

                let merged_path = self.staging_dir.join(format!(
                    "merge_pass_{:03}_chunk_{:05}.crun",
                    merge_passes, chunk_idx
                ));
                let merged_bytes = Self::merge_runs_to_run_file(chunk, &merged_path)?;
                self.current_temp_bytes = self.current_temp_bytes.saturating_add(merged_bytes);
                if self.current_temp_bytes > self.peak_temp_bytes {
                    self.peak_temp_bytes = self.current_temp_bytes;
                }

                // Remove consumed input runs immediately to bound temporary disk footprint
                for consumed in chunk {
                    if let Ok(meta) = fs::metadata(consumed) {
                        self.current_temp_bytes =
                            self.current_temp_bytes.saturating_sub(meta.len());
                    }
                    fs::remove_file(consumed)?;
                }

                next_runs.push(merged_path);
            }

            active_runs = next_runs;
        }

        if !active_runs.is_empty() {
            merge_passes += 1;
        }

        if self.config.fault_point == Some(ExternalIndexFaultPoint::DuringExternalMerge) {
            return Err(IndexError::InvalidFormat(
                "Injected fault at DuringExternalMerge (final pass)".into(),
            ));
        }

        let cpi_metadata = Self::merge_runs_to_cpi(
            &active_runs,
            output_cpi_path,
            self.config.positions_per_block,
            self.config.zstd_level,
            self.config.fault_point == Some(ExternalIndexFaultPoint::BeforeCpiCompletion),
        )?;

        // Clean up final run files after CPI completion
        for consumed in &active_runs {
            if let Ok(meta) = fs::metadata(consumed) {
                self.current_temp_bytes = self.current_temp_bytes.saturating_sub(meta.len());
            }
            let _ = fs::remove_file(consumed);
        }

        Ok(ExternalIndexStats {
            games_processed: self.games_processed,
            games_indexed: self.games_indexed,
            excluded_unknown_result_games: self.excluded_unknown_result_games,
            custom_fen_games: self.custom_fen_games,
            repeated_positions_skipped: self.repeated_positions_skipped,
            raw_observations_extracted: self.raw_observations_extracted,
            runs_generated,
            merge_passes,
            peak_temp_bytes: self.peak_temp_bytes,
            cpi_metadata,
        })
    }

    fn merge_runs_to_run_file(
        input_runs: &[PathBuf],
        output_run_path: &Path,
    ) -> Result<u64, IndexError> {
        let mut writer = RunWriter::create(output_run_path)?;
        Self::k_way_merge(input_runs, |rec| writer.write_record(&rec))?;
        writer.finish()
    }

    fn merge_runs_to_cpi(
        input_runs: &[PathBuf],
        output_cpi_path: &Path,
        positions_per_block: usize,
        zstd_level: i32,
        abort_before_finish: bool,
    ) -> Result<CpiMetadata, IndexError> {
        let file = File::create(output_cpi_path)?;
        let buf_writer = BufWriter::with_capacity(256 * 1024, file);
        let temp_sparse_dir = output_cpi_path.parent();
        let mut cpi_writer = StreamingCpiWriter::new_with_temp_dir(
            buf_writer,
            positions_per_block,
            zstd_level,
            temp_sparse_dir,
        )?;

        let mut current_key: Option<u128> = None;
        let mut current_moves: Vec<(CompactMove, u32, u32, u32)> = Vec::new();

        Self::k_way_merge(input_runs, |rec| {
            if let Some(k) = current_key {
                if rec.key != k {
                    cpi_writer.write_position(PositionKey(k), &current_moves)?;
                    current_moves.clear();
                    current_key = Some(rec.key);
                }
            } else {
                current_key = Some(rec.key);
            }
            current_moves.push((
                CompactMove(rec.compact_move),
                rec.white_wins,
                rec.draws,
                rec.black_wins,
            ));
            Ok(())
        })?;

        if let Some(k) = current_key {
            cpi_writer.write_position(PositionKey(k), &current_moves)?;
        }

        if abort_before_finish {
            return Err(IndexError::InvalidFormat(
                "Injected fault at BeforeCpiCompletion".into(),
            ));
        }

        cpi_writer.finish()
    }

    fn k_way_merge<F>(input_runs: &[PathBuf], mut emit: F) -> Result<(), IndexError>
    where
        F: FnMut(RunRecord) -> Result<(), IndexError>,
    {
        if input_runs.is_empty() {
            return Ok(());
        }

        let mut readers = Vec::with_capacity(input_runs.len());
        let mut heap: BinaryHeap<Reverse<(RunRecord, usize)>> =
            BinaryHeap::with_capacity(input_runs.len());

        for (idx, run_path) in input_runs.iter().enumerate() {
            let mut reader = RunReader::open_and_verify(run_path)?;
            if let Some(first_rec) = reader.next_record()? {
                heap.push(Reverse((first_rec, idx)));
            }
            readers.push(reader);
        }

        while let Some(Reverse((mut acc, first_idx))) = heap.pop() {
            if let Some(next_rec) = readers[first_idx].next_record()? {
                heap.push(Reverse((next_rec, first_idx)));
            }

            // Coalesce all heap entries sharing the same (key, compact_move)
            while let Some(&Reverse((top_rec, top_idx))) = heap.peek() {
                if top_rec.key != acc.key || top_rec.compact_move != acc.compact_move {
                    break;
                }
                let _ = heap.pop();
                acc.white_wins = acc
                    .white_wins
                    .checked_add(top_rec.white_wins)
                    .ok_or_else(|| IndexError::Overflow("Merge white_wins overflow".into()))?;
                acc.draws = acc
                    .draws
                    .checked_add(top_rec.draws)
                    .ok_or_else(|| IndexError::Overflow("Merge draws overflow".into()))?;
                acc.black_wins = acc
                    .black_wins
                    .checked_add(top_rec.black_wins)
                    .ok_or_else(|| IndexError::Overflow("Merge black_wins overflow".into()))?;

                if let Some(next_rec) = readers[top_idx].next_record()? {
                    heap.push(Reverse((next_rec, top_idx)));
                }
            }

            emit(acc)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::{
        compute_position_key, DiskPositionIndex, ImmutablePositionIndex, SqlitePositionIndex,
    };
    use codb_core::{Collection, GameMetadata};
    use sha2::Digest;
    use shakmaty::{Chess, Move, Position, Role, Square};

    fn make_game(id: u64, result: GameResult, moves: Vec<CompactMove>) -> ParsedGame {
        ParsedGame {
            metadata: GameMetadata {
                id,
                collection: Collection::MastersOtb,
                white: "White".into(),
                black: "Black".into(),
                white_elo: Some(2650),
                black_elo: Some(2640),
                date: Some("2025.01.01".into()),
                event: Some("Test".into()),
                site: None,
                round: None,
                result,
                eco: None,
                time_control: None,
                source_id: None,
                broadcast_url: None,
            },
            moves,
            fen_start: None,
        }
    }

    fn sample_corpus() -> Vec<ParsedGame> {
        let e2e4 = CompactMove::new(12, 28, None);
        let e7e5 = CompactMove::new(52, 36, None);
        let c7c5 = CompactMove::new(50, 34, None);
        let e7e6 = CompactMove::new(52, 44, None);
        let d2d4 = CompactMove::new(11, 27, None);
        let g8f6 = CompactMove::new(62, 45, None);
        let c2c4 = CompactMove::new(10, 26, None);
        let g1f3 = CompactMove::new(6, 21, None);
        let b8c6 = CompactMove::new(57, 42, None);

        vec![
            // Game 1: 1. e4 e5 2. Nf3 Nc6 (1-0)
            make_game(1, GameResult::WhiteWins, vec![e2e4, e7e5, g1f3, b8c6]),
            // Game 2: 1. e4 e5 2. Nf3 Nc6 (1/2-1/2)
            make_game(2, GameResult::Draw, vec![e2e4, e7e5, g1f3, b8c6]),
            // Game 3: 1. e4 c5 2. Nf3 Nc6 (0-1)
            make_game(3, GameResult::BlackWins, vec![e2e4, c7c5, g1f3, b8c6]),
            // Game 4: 1. e4 e6 2. d4 (1-0)
            make_game(4, GameResult::WhiteWins, vec![e2e4, e7e6, d2d4]),
            // Game 5 (Transposition A): 1. d4 Nf6 2. c4 e6 (1-0)
            make_game(5, GameResult::WhiteWins, vec![d2d4, g8f6, c2c4, e7e6]),
            // Game 6 (Transposition B): 1. c4 e6 2. d4 Nf6 (0-1)
            make_game(6, GameResult::BlackWins, vec![c2c4, e7e6, d2d4, g8f6]),
            // Game 7 (Unknown result *): should be validated and excluded from W/D/L
            make_game(7, GameResult::Star, vec![e2e4, e7e5]),
        ]
    }

    #[test]
    fn test_golden_reference_opening_stats_and_oracle_equivalence() {
        let games = sample_corpus();
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        let cpi_path = dir.path().join("positions.cpi");

        let config = ExternalIndexConfig {
            max_plies: 40,
            memory_limit_bytes: 64, // Tiny buffer to force multiple runs and multi-pass merges
            max_merge_fan_in: 2,
            positions_per_block: 2,
            zstd_level: 3,
            fault_point: None,
        };

        let mut indexer = ExternalPositionIndexer::new(&staging, config).unwrap();
        let mut imm = ImmutablePositionIndex::new();
        let mut sqlite = SqlitePositionIndex::in_memory().unwrap();

        for g in &games {
            indexer.push_game(g).unwrap();
            let ext = extract_game_positions_detailed(g, 40).unwrap();
            sqlite.insert_batch(&ext.entries).unwrap();
            for &(k, cm, r) in &ext.entries {
                imm.record(k, cm, r);
            }
        }

        let stats = indexer.finish(&cpi_path).unwrap();
        assert_eq!(stats.games_processed, 7);
        assert_eq!(stats.games_indexed, 6);
        assert_eq!(stats.excluded_unknown_result_games, 1);
        assert!(
            stats.runs_generated >= 4,
            "Expected multiple runs, got {}",
            stats.runs_generated
        );
        assert!(
            stats.merge_passes >= 2,
            "Expected multi-pass merge, got {}",
            stats.merge_passes
        );

        let disk = DiskPositionIndex::open(&cpi_path).unwrap();
        let summary = disk.verify_integrity().unwrap();
        assert_eq!(summary.total_positions, imm.len() as u64);
        assert_eq!(summary.total_pairs, imm.unique_position_move_pairs() as u64);
        assert_eq!(
            summary.total_positions,
            sqlite.unique_positions_count().unwrap() as u64
        );
        assert_eq!(
            summary.total_pairs,
            sqlite.unique_pairs_count().unwrap() as u64
        );

        // Golden Reference Check 1: Initial Position
        // Moves played from start:
        // - e2e4: Game 1 (1-0), Game 2 (1/2), Game 3 (0-1), Game 4 (1-0) -> total=4, W=2, D=1, B=1
        // - d2d4: Game 5 (1-0) -> total=1, W=1, D=0, B=0
        // - c2c4: Game 6 (0-1) -> total=1, W=0, D=0, B=1
        // (Game 7 is '*' so excluded)
        let start_board = Chess::default();
        let start_key = compute_position_key(&start_board);
        let start_stats = disk.query(start_key).unwrap().unwrap();
        assert_eq!(start_stats.total_games, 6);
        assert_eq!(start_stats.white_wins, 3);
        assert_eq!(start_stats.draws, 1);
        assert_eq!(start_stats.black_wins, 2);
        assert_eq!(start_stats.moves.len(), 3);
        assert_eq!(start_stats.moves[0].uci, "e2e4");
        assert_eq!(start_stats.moves[0].total_games, 4);
        assert_eq!(start_stats.moves[0].white_wins, 2);
        assert_eq!(start_stats.moves[0].draws, 1);
        assert_eq!(start_stats.moves[0].black_wins, 1);

        // Golden Reference Check 2: Position after 1. e4
        // Candidate responses:
        // - e7e5: Game 1 (1-0), Game 2 (1/2) -> total=2, W=1, D=1, B=0
        // - c7c5: Game 3 (0-1) -> total=1, W=0, D=0, B=1
        // - e7e6: Game 4 (1-0) -> total=1, W=1, D=0, B=0
        let mut after_e4 = Chess::default();
        after_e4.play_unchecked(Move::Normal {
            role: Role::Pawn,
            from: Square::E2,
            to: Square::E4,
            capture: None,
            promotion: None,
        });
        let after_e4_key = compute_position_key(&after_e4);
        let e4_stats = disk.query(after_e4_key).unwrap().unwrap();
        assert_eq!(e4_stats.total_games, 4);
        assert_eq!(e4_stats.white_wins, 2);
        assert_eq!(e4_stats.draws, 1);
        assert_eq!(e4_stats.black_wins, 1);
        assert_eq!(e4_stats.moves.len(), 3);
        assert_eq!(e4_stats.moves[0].uci, "e7e5");
        assert_eq!(e4_stats.moves[0].total_games, 2);

        // Equivalence across DiskPositionIndex, ImmutablePositionIndex, and SqlitePositionIndex
        for g in &games {
            let ext = extract_game_positions_detailed(g, 40).unwrap();
            for (k, _, _) in ext.entries {
                let d_res = disk.query(k).unwrap().unwrap();
                let i_res = imm.query(k).unwrap();
                let s_res = sqlite.query_position(k).unwrap();
                assert_eq!(d_res, i_res);
                assert_eq!(d_res, s_res);
            }
        }
    }

    #[test]
    fn test_byte_for_byte_determinism_across_memory_limits_and_fan_ins() {
        let games = sample_corpus();
        let dir = tempfile::tempdir().unwrap();

        let configs = [
            // Single-run large memory budget
            ExternalIndexConfig {
                max_plies: 40,
                memory_limit_bytes: 16 * 1024 * 1024,
                max_merge_fan_in: 16,
                positions_per_block: 4,
                zstd_level: 3,
                fault_point: None,
            },
            // Multi-run tiny memory budget, fan-in = 2
            ExternalIndexConfig {
                max_plies: 40,
                memory_limit_bytes: 32,
                max_merge_fan_in: 2,
                positions_per_block: 4,
                zstd_level: 3,
                fault_point: None,
            },
            // Medium memory budget, fan-in = 3
            ExternalIndexConfig {
                max_plies: 40,
                memory_limit_bytes: 128,
                max_merge_fan_in: 3,
                positions_per_block: 4,
                zstd_level: 3,
                fault_point: None,
            },
        ];

        let mut digests = Vec::new();
        for (i, cfg) in configs.iter().enumerate() {
            let staging = dir.path().join(format!("staging_{}", i));
            let cpi_path = dir.path().join(format!("out_{}.cpi", i));
            let mut indexer = ExternalPositionIndexer::new(&staging, cfg.clone()).unwrap();
            for g in &games {
                indexer.push_game(g).unwrap();
            }
            indexer.finish(&cpi_path).unwrap();

            let bytes = fs::read(&cpi_path).unwrap();
            let hash = Sha256::digest(&bytes);
            digests.push((bytes, hash));
        }

        // Also compare against ImmutablePositionIndex::write_to_file
        let mut imm = ImmutablePositionIndex::new();
        for g in &games {
            for (k, cm, r) in extract_game_positions_detailed(g, 40).unwrap().entries {
                imm.record(k, cm, r);
            }
        }
        let imm_cpi_path = dir.path().join("imm.cpi");
        imm.write_to_file(&imm_cpi_path, 4, 3).unwrap();
        let imm_bytes = fs::read(&imm_cpi_path).unwrap();
        let imm_hash = Sha256::digest(&imm_bytes);

        for (bytes, hash) in &digests {
            assert_eq!(hash, &imm_hash, "CPI SHA-256 must be identical");
            assert_eq!(bytes, &imm_bytes, "CPI bytes must be 100% identical");
        }
    }

    #[test]
    fn test_corrupted_run_file_rejected_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let run_path = dir.path().join("test.crun");
        let mut writer = RunWriter::create(&run_path).unwrap();
        writer
            .write_record(&RunRecord {
                key: 42,
                compact_move: 100,
                white_wins: 1,
                draws: 0,
                black_wins: 0,
            })
            .unwrap();
        writer.finish().unwrap();

        // Valid run opens cleanly
        let mut reader = RunReader::open_and_verify(&run_path).unwrap();
        assert!(reader.next_record().unwrap().is_some());
        assert!(reader.next_record().unwrap().is_none());

        // Flip a bit in the payload -> SHA-256 verification must fail closed
        let mut bytes = fs::read(&run_path).unwrap();
        bytes[20] ^= 0x01;
        fs::write(&run_path, &bytes).unwrap();
        let err = RunReader::open_and_verify(&run_path).err().unwrap();
        assert!(
            matches!(err, IndexError::InvalidFormat(_)),
            "Expected InvalidFormat on corrupted run, got {:?}",
            err
        );
    }

    #[test]
    fn test_corrupted_cpi_rejection_suite() {
        let games = sample_corpus();
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        let valid_cpi = dir.path().join("valid.cpi");

        let mut indexer =
            ExternalPositionIndexer::new(&staging, ExternalIndexConfig::default()).unwrap();
        for g in &games {
            indexer.push_game(g).unwrap();
        }
        indexer.finish(&valid_cpi).unwrap();
        let base_bytes = fs::read(&valid_cpi).unwrap();

        // 1. Truncated header (< 64 bytes)
        let p1 = dir.path().join("trunc_hdr.cpi");
        fs::write(&p1, &base_bytes[..32]).unwrap();
        assert!(DiskPositionIndex::open(&p1).is_err());

        // 2. Bad magic
        let mut b2 = base_bytes.clone();
        b2[0] = b'X';
        let p2 = dir.path().join("bad_magic.cpi");
        fs::write(&p2, &b2).unwrap();
        assert!(DiskPositionIndex::open(&p2).is_err());

        // 3. Unsupported version
        let mut b3 = base_bytes.clone();
        b3[8..12].copy_from_slice(&99u32.to_be_bytes());
        let p3 = dir.path().join("bad_version.cpi");
        fs::write(&p3, &b3).unwrap();
        assert!(DiskPositionIndex::open(&p3).is_err());

        // 4. Sparse index offset/length mismatch (trailing byte)
        let mut b4 = base_bytes.clone();
        b4.push(0xFF);
        let p4 = dir.path().join("trailing.cpi");
        fs::write(&p4, &b4).unwrap();
        assert!(DiskPositionIndex::open(&p4).is_err());

        // 5. Corrupted compressed block payload
        let mut b5 = base_bytes.clone();
        b5[65] ^= 0xFF;
        b5[66] ^= 0xFF;
        let p5 = dir.path().join("corrupt_block.cpi");
        fs::write(&p5, &b5).unwrap();
        if let Ok(idx) = DiskPositionIndex::open(&p5) {
            assert!(idx.verify_integrity().is_err());
        }
    }

    #[test]
    fn test_custom_fen_and_invalid_fen_handling() {
        // Legal custom starting FEN (after 1. e4 e5)
        let mut custom_game = make_game(
            10,
            GameResult::WhiteWins,
            vec![CompactMove::new(6, 21, None)], // g1f3
        );
        custom_game.fen_start =
            Some("rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2".into());

        let ext = extract_game_positions_detailed(&custom_game, 40).unwrap();
        assert!(ext.custom_starting_fen);
        assert_eq!(ext.entries.len(), 1);
        assert_eq!(ext.entries[0].1.to_uci(), "g1f3");

        // Invalid starting FEN must fail closed
        let mut bad_fen_game = custom_game.clone();
        bad_fen_game.fen_start = Some("not-a-valid-fen".into());
        assert!(extract_game_positions_detailed(&bad_fen_game, 40).is_err());

        // Illegal move at ply 1 must fail closed even on GameResult::Star
        let illegal_star = make_game(
            11,
            GameResult::Star,
            vec![CompactMove::new(0, 63, None)], // a1h8 illegal from initial board
        );
        assert!(extract_game_positions_detailed(&illegal_star, 40).is_err());
    }

    #[test]
    fn test_zobrist_castling_en_passant_and_promotion_semantics() {
        use shakmaty::fen::Fen;

        // 1. Castling rights distinction
        let fen_with_castling: Fen = "r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w KQkq - 0 1"
            .parse()
            .unwrap();
        let fen_no_castling: Fen = "r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w - - 0 1"
            .parse()
            .unwrap();
        let pos_c1: Chess = fen_with_castling
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        let pos_c2: Chess = fen_no_castling
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        assert_ne!(
            compute_position_key(&pos_c1),
            compute_position_key(&pos_c2),
            "Positions differing in castling rights must have distinct PositionKeys"
        );

        // 2. Side-to-move distinction
        let fen_w: Fen = "r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w KQkq - 0 1"
            .parse()
            .unwrap();
        let fen_b: Fen = "r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R b KQkq - 0 1"
            .parse()
            .unwrap();
        let pos_w: Chess = fen_w
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        let pos_b: Chess = fen_b
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        assert_ne!(
            compute_position_key(&pos_w),
            compute_position_key(&pos_b),
            "Positions differing in side-to-move must have distinct PositionKeys"
        );

        // 3. En-passant legality distinction (EnPassantMode::Legal)
        // After 1. e4, e3 is set in FEN, but Black has no pawn on d4/f4 so ep is not legal:
        let fen_pseudo_ep: Fen = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1"
            .parse()
            .unwrap();
        let fen_no_ep: Fen = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1"
            .parse()
            .unwrap();
        let pos_pep: Chess = fen_pseudo_ep
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        let pos_nep: Chess = fen_no_ep
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        assert_eq!(
            compute_position_key(&pos_pep),
            compute_position_key(&pos_nep),
            "Pseudo-legal en-passant square without legal capture must not alter PositionKey"
        );

        // When Black HAS a pawn on d4 capable of capturing e4->e3 en-passant:
        let fen_legal_ep: Fen = "rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 2"
            .parse()
            .unwrap();
        let fen_legal_no_ep: Fen = "rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 2"
            .parse()
            .unwrap();
        let pos_lep: Chess = fen_legal_ep
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        let pos_lnep: Chess = fen_legal_no_ep
            .into_position(shakmaty::CastlingMode::Standard)
            .unwrap();
        assert_ne!(
            compute_position_key(&pos_lep),
            compute_position_key(&pos_lnep),
            "Legal en-passant availability must produce a distinct PositionKey"
        );

        // 4. Promotions (Queen vs Knight promotion from a7->a8)
        let mut promo_q = make_game(
            20,
            GameResult::WhiteWins,
            vec![CompactMove::new(48, 56, Some(4))], // a7a8q
        );
        promo_q.fen_start = Some("7k/P7/8/8/8/8/8/4K3 w - - 0 1".into());
        let mut promo_n = make_game(
            21,
            GameResult::Draw,
            vec![CompactMove::new(48, 56, Some(1))], // a7a8n
        );
        promo_n.fen_start = Some("7k/P7/8/8/8/8/8/4K3 w - - 0 1".into());

        let q_ext = extract_game_positions_detailed(&promo_q, 10).unwrap();
        let n_ext = extract_game_positions_detailed(&promo_n, 10).unwrap();
        assert_eq!(q_ext.entries[0].1.to_uci(), "a7a8q");
        assert_eq!(n_ext.entries[0].1.to_uci(), "a7a8n");
        assert_ne!(q_ext.entries[0].1, n_ext.entries[0].1);
    }

    #[test]
    fn test_memory_limit_configurations_and_spilling() {
        let corpus = sample_corpus();
        let dir = tempfile::tempdir().unwrap();

        // 1. Minimum production budget: 16 MiB
        let p_16m = dir.path().join("out_16m.cpi");
        let mut indexer_16m = ExternalPositionIndexer::new(
            &dir.path().join("staging_16m"),
            ExternalIndexConfig {
                max_plies: 40,
                memory_limit_bytes: 16 * 1024 * 1024,
                max_merge_fan_in: 4,
                positions_per_block: 4,
                zstd_level: 3,
                fault_point: None,
            },
        )
        .unwrap();
        for g in &corpus {
            indexer_16m.push_game(g).unwrap();
        }
        let stats_16m = indexer_16m.finish(&p_16m).unwrap();

        // 2. Intermediate budget: 64 MiB
        let p_64m = dir.path().join("out_64m.cpi");
        let mut indexer_64m = ExternalPositionIndexer::new(
            &dir.path().join("staging_64m"),
            ExternalIndexConfig {
                max_plies: 40,
                memory_limit_bytes: 64 * 1024 * 1024,
                max_merge_fan_in: 4,
                positions_per_block: 4,
                zstd_level: 3,
                fault_point: None,
            },
        )
        .unwrap();
        for g in &corpus {
            indexer_64m.push_game(g).unwrap();
        }
        let stats_64m = indexer_64m.finish(&p_64m).unwrap();

        // 3. Smallest test budget (forcing multiple runs and intermediate merge passes)
        let p_tiny = dir.path().join("out_tiny.cpi");
        let mut indexer_tiny = ExternalPositionIndexer::new(
            &dir.path().join("staging_tiny"),
            ExternalIndexConfig {
                max_plies: 40,
                memory_limit_bytes: 64, // forces 1 record per buffer
                max_merge_fan_in: 2,
                positions_per_block: 4,
                zstd_level: 3,
                fault_point: None,
            },
        )
        .unwrap();
        for g in &corpus {
            indexer_tiny.push_game(g).unwrap();
        }
        let stats_tiny = indexer_tiny.finish(&p_tiny).unwrap();

        // Verify byte-for-byte identity of CPI artifacts across all memory budgets
        let bytes_16m = fs::read(&p_16m).unwrap();
        let bytes_64m = fs::read(&p_64m).unwrap();
        let bytes_tiny = fs::read(&p_tiny).unwrap();

        assert_eq!(bytes_16m, bytes_64m);
        assert_eq!(bytes_16m, bytes_tiny);
        assert_eq!(stats_16m.cpi_metadata, stats_64m.cpi_metadata);
        assert_eq!(stats_16m.cpi_metadata, stats_tiny.cpi_metadata);
        assert!(stats_tiny.runs_generated > 1);
        assert!(stats_tiny.merge_passes > 1);
    }
}
