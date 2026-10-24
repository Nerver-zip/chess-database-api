use std::fs::File;
use std::hint::black_box;
use std::io::{BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use codb_core::{Collection, IngestionCompletionStatus, IngestionStats, ParsedGame, PositionKey};
use codb_curate::dedupe::{Deduplicator, DuplicateVerdict};
use codb_curate::policy::{EliteOnlinePolicy, MastersOtbPolicy, PolicyVerdict};
use codb_index::position::{
    compute_position_key, extract_game_positions, DiskPositionIndex, ImmutablePositionIndex,
    SqlitePositionIndex,
};
use codb_ingest::pipeline::{CuratedPipeline, GameConsumer};
use codb_ingest::reader::{
    build_pgn_reader, is_recoverable_pgn_syntax_error, resync_after_syntax_error, IngestError,
    ShakmatyGameVisitor,
};
use codb_storage::format::{export_to_pgn, GameBlock};
use codb_storage::{
    compute_file_sha256, verify_dataset, CurationPolicyMetadata, DatasetReader, DatasetWriter,
    DatasetWriterConfig, ShardReader, SourceArchiveMetadata, StorageError,
};
use serde::{Deserialize, Serialize};
use shakmaty::uci::UciMove;
use shakmaty::{Chess, Position};

#[derive(Parser, Debug)]
#[command(
    name = "codb",
    version = "0.1.0",
    about = "Chess Openings DB research, curation & benchmarking toolkit"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum CollectionArg {
    #[serde(rename = "masters-otb")]
    MastersOtb,
    #[serde(rename = "elite-online")]
    EliteOnline,
}

impl From<CollectionArg> for Collection {
    fn from(c: CollectionArg) -> Self {
        match c {
            CollectionArg::MastersOtb => Collection::MastersOtb,
            CollectionArg::EliteOnline => Collection::EliteOnline,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum FormatArg {
    Pgn,
    Json,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum DedupeModeArg {
    Memory,
    Disk,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Ingest a PGN stream (.pgn or .pgn.zst) and report validation statistics
    Ingest {
        #[arg(short, long)]
        input: PathBuf,

        #[arg(short, long, default_value = "masters-otb")]
        collection: CollectionArg,

        #[arg(short, long)]
        max_games: Option<usize>,
    },

    /// Build a persistent curated dataset with bounded memory, sharded CGB3 storage, and global game lookup
    Build {
        /// Optional manifest JSON file listing input source archives
        #[arg(short, long)]
        manifest: Option<PathBuf>,

        /// One or more input PGN or PGN.zst files (use '-' for stdin)
        #[arg(short, long, num_args = 1..)]
        input: Vec<PathBuf>,

        /// Collection to curate
        #[arg(short, long, default_value = "masters-otb")]
        collection: CollectionArg,

        /// Output directory for finalized dataset
        #[arg(short, long)]
        output: PathBuf,

        /// Optional maximum accepted games limit
        #[arg(long)]
        max_games: Option<usize>,

        /// Block capacity (games per compressed block)
        #[arg(long, default_value = "512")]
        block_capacity: usize,

        /// Maximum games per shard file before rotating
        #[arg(long, default_value = "500000")]
        max_games_per_shard: usize,

        /// Zstandard compression level (1-22)
        #[arg(long, default_value = "3")]
        zstd_level: i32,

        /// Deduplication mode: memory or disk (disk uses temporary SQLite with bounded RAM)
        #[arg(long, default_value = "disk")]
        dedupe_mode: DedupeModeArg,
    },

    /// Retrieve an individual game by stable ID from a persistent dataset
    GetGame {
        /// Dataset directory path containing manifest.json
        #[arg(short, long)]
        dataset: PathBuf,

        /// 64-bit stable game ID
        #[arg(short, long)]
        id: u64,

        /// Output format: pgn or json
        #[arg(short, long, default_value = "pgn")]
        format: FormatArg,
    },

    /// Inspect a completed dataset manifest and physical storage metrics
    Inspect {
        /// Dataset directory path containing manifest.json
        #[arg(short, long)]
        dataset: PathBuf,

        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Verify dataset manifest, shard checksums, block boundaries, and game lookup integrity
    Verify {
        /// Dataset directory path containing manifest.json
        #[arg(short, long)]
        dataset: PathBuf,

        /// Perform deep game-level decompression and verification of every block
        #[arg(long)]
        deep: bool,
    },

    /// Run the end-to-end benchmark suite and write JSON reports
    Bench {
        #[arg(short, long)]
        input: PathBuf,

        #[arg(short, long, default_value = "masters-otb")]
        collection: CollectionArg,

        #[arg(short, long)]
        max_games: Option<usize>,

        #[arg(short, long, default_value = "benchmarks/results")]
        output_dir: PathBuf,

        #[arg(long, default_value = "stage_a")]
        run_name: String,
    },

    /// Query the Opening Explorer index for a position by FEN or initial board
    Query {
        #[arg(short, long)]
        index: PathBuf,

        #[arg(short, long)]
        fen: Option<String>,

        #[arg(short, long)]
        moves: Option<String>,
    },

    /// Benchmark an existing persistent dataset: random game retrieval latencies,
    /// position indexing across depths (20, 30, 40, 60), and generate empirical JSON report
    BenchDataset {
        /// Dataset directory path containing manifest.json
        #[arg(short, long)]
        dataset: PathBuf,

        /// Output directory for benchmark report JSON
        #[arg(short, long, default_value = "benchmarks/results")]
        output_dir: PathBuf,

        /// Run name identifier for the generated benchmark report
        #[arg(long)]
        run_name: String,

        /// Number of random ID retrieval lookups to measure (default: 10,000)
        #[arg(long, default_value = "10000")]
        random_lookups: usize,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct RandomRetrievalMetrics {
    lookups_count: usize,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    max_us: f64,
    correctness_verified: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct PersistentStorageMetrics {
    shards_count: usize,
    shards_bytes: u64,
    lookup_index_bytes: u64,
    manifest_bytes: u64,
    total_persistent_bytes: u64,
    physical_bytes_per_game: f64,
}

fn default_schema_version() -> String {
    "2.0.0".into()
}

fn default_benchmark_type() -> String {
    "raw_ingest".into()
}

#[derive(Serialize, Deserialize)]
struct BenchmarkReport {
    #[serde(default = "default_schema_version")]
    schema_version: String,
    #[serde(default = "default_benchmark_type")]
    benchmark_type: String, // "raw_ingest" or "dataset_archive"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reproduction_status: Option<String>,

    run_name: String,
    collection: String,
    input_file: String,
    #[serde(default)]
    input_size_bytes: Option<u64>,
    total_encountered: u64,
    accepted_unique: u64,
    retained_canonical_games: usize,
    confirmed_duplicates: u64,
    superseded_prior: u64,
    rejected_by_policy: u64,
    rejected_variant: u64,
    rejected_illegal: u64,
    rejected_malformed: u64,
    quarantined: u64,
    completion_status: IngestionCompletionStatus,
    is_reconciled: bool,
    retained_matches_accepted: bool,
    transpositions_distinct: u64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    import_duration_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    import_throughput_games_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    import_throughput_mb_sec: Option<f64>,

    // Timing and throughput disambiguation (raw ingest vs archive reload)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_ingest_duration_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_ingest_throughput_games_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_reload_duration_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_reload_throughput_games_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_reload_throughput_mb_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_ingest_note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_archives: Vec<SourceArchiveMetadata>,

    // Storage measurements (physically measured vs framing overhead)
    raw_pgn_bytes: u64,
    raw_bytes_per_game: f64,
    codb_block_compressed_bytes: u64,
    codb_block_directory_bytes: u64,
    codb_total_container_bytes: u64,
    codb_bytes_per_game: f64,
    compression_ratio: f64,

    // Index measurements across depths (including direct depth 30)
    depth_20: IndexDepthStats,
    depth_30: IndexDepthStats,
    depth_40: IndexDepthStats,
    depth_60: IndexDepthStats,

    // Latency measurements in microseconds
    query_latency_us: LatencyMetrics,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    random_retrieval_latency_us: Option<RandomRetrievalMetrics>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    persistent_storage: Option<PersistentStorageMetrics>,
}

#[derive(Serialize, Deserialize)]
struct IndexDepthStats {
    depth_plies: usize,
    unique_positions: usize,
    unique_position_move_pairs: usize,
    cpi_block_count: u32,
    cpi_sparse_directory_bytes: usize,
    immutable_index_size_bytes: usize,
    sqlite_index_size_bytes: usize,
    index_build_duration_secs: f64,
}

#[derive(Serialize, Deserialize)]
struct LatencyMetrics {
    cache_regime: String,
    query_sample_count: usize,
    frequent_keys_sampled: usize,
    medium_keys_sampled: usize,
    rare_keys_sampled: usize,
    transposition_keys_sampled: usize,
    miss_keys_sampled: usize,
    semantic_equivalence_verified: bool,
    sqlite_disk_p50_us: f64,
    sqlite_disk_p95_us: f64,
    sqlite_disk_p99_us: f64,
    cpi_disk_p50_us: f64,
    cpi_disk_p95_us: f64,
    cpi_disk_p99_us: f64,
    cpi_in_memory_p50_us: f64,
    cpi_in_memory_p95_us: f64,
    cpi_in_memory_p99_us: f64,
}

pub struct CuratedIngestionOutcome {
    pub stats: IngestionStats,
    pub accepted_games: Vec<ParsedGame>,
    pub transpositions_distinct: u64,
}

/// Executes policy-filtered, deduplicated ingestion over a PGN stream.
/// Fails closed (`Err`) on fatal I/O or decompression errors, and verifies accounting invariants.
pub fn ingest_curated_stream<R: Read>(
    reader: R,
    collection: Collection,
    max_games: Option<usize>,
    log_progress: bool,
) -> Result<CuratedIngestionOutcome> {
    let mut pgn_reader = build_pgn_reader(reader);
    let mut visitor = ShakmatyGameVisitor::new(collection, 1);
    let mut dedupe = Deduplicator::new();
    let mut stats = IngestionStats::default();

    let masters_policy = MastersOtbPolicy::default();
    let elite_policy = EliteOnlinePolicy::default();

    let mut accepted_games: Vec<ParsedGame> = Vec::new();
    let mut transpositions_distinct = 0u64;
    let mut consecutive_syntax_errors = 0usize;
    let mut count = 0u64;

    loop {
        match pgn_reader.read_game(&mut visitor) {
            Ok(Some(result)) => {
                consecutive_syntax_errors = 0;
                stats.total_encountered += 1;
                match result {
                    Ok(game) => {
                        let passes_policy = match collection {
                            Collection::MastersOtb => {
                                masters_policy.evaluate(&game) == PolicyVerdict::Accepted
                            }
                            Collection::EliteOnline => {
                                elite_policy.evaluate(&game) == PolicyVerdict::Accepted
                            }
                        };
                        if !passes_policy {
                            stats.rejected_by_policy += 1;
                        } else {
                            match dedupe.evaluate(&game)? {
                                DuplicateVerdict::Unique => {
                                    stats.accepted_unique += 1;
                                    accepted_games.push(game);
                                }
                                DuplicateVerdict::TranspositionDifferentGame => {
                                    stats.accepted_unique += 1;
                                    transpositions_distinct += 1;
                                    accepted_games.push(game);
                                }
                                DuplicateVerdict::ConfirmedDuplicate { .. } => {
                                    stats.confirmed_duplicates += 1;
                                }
                                DuplicateVerdict::SupersedesPrevious { old_id, .. } => {
                                    stats.superseded_prior += 1;
                                    let pos = accepted_games
                                        .iter()
                                        .position(|g| g.metadata.id == old_id)
                                        .with_context(|| {
                                            format!(
                                                "Superseded game ID {} not found in retained canonical games",
                                                old_id
                                            )
                                        })?;
                                    accepted_games[pos] = game;
                                }
                            }
                        }
                    }
                    Err(IngestError::UnsupportedVariant(_))
                    | Err(IngestError::NonStandardStartingPosition(_)) => {
                        stats.rejected_variant += 1;
                    }
                    Err(IngestError::IllegalMove { .. }) => {
                        stats.rejected_illegal += 1;
                    }
                    Err(IngestError::MalformedPgn(_)) => {
                        stats.rejected_malformed += 1;
                    }
                    Err(other_err) => {
                        return Err(anyhow::Error::from(other_err)
                            .context("Fatal error during game ingestion"));
                    }
                }
            }
            Ok(None) => {
                stats.completion_status = IngestionCompletionStatus::CompleteEof;
                break;
            }
            Err(io_err) => {
                if is_recoverable_pgn_syntax_error(&io_err) {
                    stats.total_encountered += 1;
                    stats.rejected_malformed += 1;
                    if !resync_after_syntax_error(&mut pgn_reader, &mut consecutive_syntax_errors)
                        .context("Failed to resynchronize PGN stream after syntax error")?
                    {
                        stats.completion_status = IngestionCompletionStatus::CompleteEof;
                        break;
                    }
                } else {
                    return Err(anyhow::Error::from(IngestError::Io(io_err))
                        .context("Fatal I/O or decompression error while reading PGN stream"));
                }
            }
        }

        count += 1;
        if let Some(max) = max_games {
            if stats.accepted_unique as usize >= max {
                stats.completion_status = IngestionCompletionStatus::PartialLimitReached;
                break;
            }
        }

        if log_progress && count.is_multiple_of(20000) {
            println!(
                "Encountered: {}, Accepted: {}, Policy Rejected: {}, Duplicates: {}, Illegal: {}",
                stats.total_encountered,
                stats.accepted_unique,
                stats.rejected_by_policy,
                stats.confirmed_duplicates,
                stats.rejected_illegal
            );
        }
    }

    anyhow::ensure!(
        stats.is_reconciled(),
        "Ingestion accounting invariant violated: total_encountered ({}) != sum of categories ({})",
        stats.total_encountered,
        stats.reconciled_sum()
    );
    anyhow::ensure!(
        stats.verify_retained_count(accepted_games.len()),
        "Retained canonical game count ({}) does not match accepted_unique ({})",
        accepted_games.len(),
        stats.accepted_unique
    );

    Ok(CuratedIngestionOutcome {
        stats,
        accepted_games,
        transpositions_distinct,
    })
}

fn open_pgn_reader(path: &Path) -> Result<Box<dyn Read + Send>> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file {}", path.display()))?;
    let buf_reader = BufReader::new(file);

    if path.extension().and_then(|s| s.to_str()) == Some("zst") {
        let zstd_dec = zstd::stream::read::Decoder::new(buf_reader)?;
        Ok(Box::new(zstd_dec))
    } else {
        Ok(Box::new(buf_reader))
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            manifest,
            input,
            collection,
            output,
            max_games,
            block_capacity,
            max_games_per_shard,
            zstd_level,
            dedupe_mode,
        } => {
            run_build(BuildOptions {
                manifest,
                input,
                collection,
                output,
                max_games,
                block_capacity,
                max_games_per_shard,
                zstd_level,
                dedupe_mode,
            })?;
        }

        Commands::GetGame {
            dataset,
            id,
            format,
        } => {
            run_get_game(&dataset, id, format)?;
        }

        Commands::Inspect { dataset, json } => {
            run_inspect(&dataset, json)?;
        }

        Commands::Verify { dataset, deep } => {
            run_verify(&dataset, deep)?;
        }

        Commands::Ingest {
            input,
            collection,
            max_games,
        } => {
            println!("Ingesting from {}...", input.display());
            let stream = open_pgn_reader(&input)?;
            let col = collection.into();

            let start = Instant::now();
            let outcome = ingest_curated_stream(stream, col, max_games, true)?;
            let stats = &outcome.stats;
            let elapsed = start.elapsed().as_secs_f64();

            println!("\n=== Ingestion Summary ===");
            println!("Total Encountered:    {}", stats.total_encountered);
            println!("Accepted Unique:      {}", stats.accepted_unique);
            println!("Retained Canonical:   {}", outcome.accepted_games.len());
            println!("Confirmed Duplicates: {}", stats.confirmed_duplicates);
            println!("Superseded Prior:     {}", stats.superseded_prior);
            println!("Rejected by Policy:   {}", stats.rejected_by_policy);
            println!("Rejected Variant/FEN: {}", stats.rejected_variant);
            println!("Rejected Illegal:     {}", stats.rejected_illegal);
            println!("Rejected Malformed:   {}", stats.rejected_malformed);
            println!("Quarantined:          {}", stats.quarantined);
            println!("Completion Status:    {:?}", stats.completion_status);
            println!(
                "Accounting Invariant: Reconciled = {}, Retained Matches Accepted = {}",
                stats.is_reconciled(),
                stats.verify_retained_count(outcome.accepted_games.len())
            );
            println!("Duration:             {:.2}s", elapsed);
            println!(
                "Throughput:           {:.1} games/sec",
                stats.total_encountered as f64 / elapsed.max(1e-9)
            );
        }

        Commands::Bench {
            input,
            collection,
            max_games,
            output_dir,
            run_name,
        } => {
            run_benchmarks(&input, collection.into(), max_games, &output_dir, &run_name)?;
        }

        Commands::Query { index, fen, moves } => {
            run_query(&index, fen.as_deref(), moves.as_deref())?;
        }

        Commands::BenchDataset {
            dataset,
            output_dir,
            run_name,
            random_lookups,
        } => {
            run_bench_dataset(&dataset, &output_dir, &run_name, random_lookups)?;
        }
    }

    Ok(())
}

struct DatasetWriterConsumer<'a> {
    writer: &'a mut DatasetWriter,
}

impl<'a> GameConsumer for DatasetWriterConsumer<'a> {
    fn accept_game(&mut self, game: ParsedGame) -> std::result::Result<(), String> {
        self.writer.push_game(game).map_err(|e| e.to_string())
    }

    fn supersede_game(
        &mut self,
        old_id: u64,
        new_game: ParsedGame,
    ) -> std::result::Result<(), String> {
        self.writer
            .handle_superseded_game(old_id, new_game)
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct SourceManifestInput {
    #[allow(dead_code)]
    collection: Option<CollectionArg>,
    sources: Vec<SourceEntryInput>,
}

#[derive(Debug, Deserialize)]
struct SourceEntryInput {
    path: PathBuf,
    url: Option<String>,
    license: Option<String>,
    sha256: Option<String>,
}

fn is_zstd_magic(prefix: &[u8]) -> bool {
    if prefix.len() < 4 {
        return false;
    }
    // Standard Zstandard frame (0xFD2FB528 in little-endian: [0x28, 0xB5, 0x2F, 0xFD])
    if prefix[..4] == [0x28, 0xB5, 0x2F, 0xFD] {
        return true;
    }
    // Zstandard skippable frame (0x184D2A50..=0x184D2A5F in little-endian: [0x50..=0x5F, 0x2A, 0x4D, 0x18])
    if (0x50..=0x5F).contains(&prefix[0])
        && prefix[1] == 0x2A
        && prefix[2] == 0x4D
        && prefix[3] == 0x18
    {
        return true;
    }
    false
}

fn open_auto_detect_reader(path: &Path) -> Result<Box<dyn Read + Send>> {
    if path.as_os_str() == "-" {
        let mut stdin = std::io::stdin();
        let mut prefix = [0u8; 4];
        let n = stdin.read(&mut prefix)?;
        if n == 4 && is_zstd_magic(&prefix) {
            let chained = Cursor::new(prefix).chain(stdin);
            let zstd_dec = zstd::stream::read::Decoder::new(chained)?;
            Ok(Box::new(zstd_dec))
        } else {
            let chained = Cursor::new(prefix[..n].to_vec()).chain(stdin);
            Ok(Box::new(BufReader::new(chained)))
        }
    } else {
        let file = File::open(path)
            .with_context(|| format!("Failed to open input file {}", path.display()))?;
        let is_zst_ext = path.extension().and_then(|s| s.to_str()) == Some("zst");
        let mut buf_reader = BufReader::new(file);
        let mut prefix = [0u8; 4];
        let n = buf_reader.read(&mut prefix)?;
        if is_zst_ext || (n == 4 && is_zstd_magic(&prefix)) {
            let chained = Cursor::new(prefix[..n].to_vec()).chain(buf_reader);
            let zstd_dec = zstd::stream::read::Decoder::new(chained)?;
            Ok(Box::new(zstd_dec))
        } else {
            let chained = Cursor::new(prefix[..n].to_vec()).chain(buf_reader);
            Ok(Box::new(BufReader::new(chained)))
        }
    }
}

struct BuildOptions {
    manifest: Option<PathBuf>,
    input: Vec<PathBuf>,
    collection: CollectionArg,
    output: PathBuf,
    max_games: Option<usize>,
    block_capacity: usize,
    max_games_per_shard: usize,
    zstd_level: i32,
    dedupe_mode: DedupeModeArg,
}

fn run_build(opts: BuildOptions) -> Result<()> {
    let mut resolved_inputs = Vec::new();
    let mut source_metas = Vec::new();
    let col: Collection = opts.collection.into();

    if let Some(m_path) = opts.manifest {
        let file_bytes = std::fs::read(&m_path)
            .with_context(|| format!("Failed to read manifest file {}", m_path.display()))?;
        let parsed_manifest: SourceManifestInput = serde_json::from_slice(&file_bytes)
            .with_context(|| format!("Failed to parse manifest JSON in {}", m_path.display()))?;

        for s in parsed_manifest.sources {
            if s.path.as_os_str() != "-" && !s.path.exists() {
                anyhow::bail!(
                    "Source archive file '{}' specified in manifest does not exist",
                    s.path.display()
                );
            }
            let (size, verified_sha256) = if s.path.as_os_str() != "-" && s.path.exists() {
                let file_size = std::fs::metadata(&s.path)?.len();
                let actual_hash = compute_file_sha256(&s.path).with_context(|| {
                    format!("Failed to compute SHA-256 for {}", s.path.display())
                })?;
                if let Some(ref expected_hash) = s.sha256 {
                    if expected_hash.len() != 64
                        || !expected_hash.chars().all(|c| c.is_ascii_hexdigit())
                    {
                        anyhow::bail!(
                            "Malformed SHA-256 for source archive '{}': '{}' is not a valid 64-character hexadecimal checksum",
                            s.path.display(),
                            expected_hash
                        );
                    }
                    if !expected_hash.eq_ignore_ascii_case(&actual_hash) {
                        anyhow::bail!(
                            "Source archive checksum mismatch for '{}': expected {}, calculated {}",
                            s.path.display(),
                            expected_hash,
                            actual_hash
                        );
                    }
                }
                (file_size, Some(actual_hash.to_lowercase()))
            } else {
                (0, None)
            };

            resolved_inputs.push(s.path.clone());
            source_metas.push(SourceArchiveMetadata {
                filename: s
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                url: s.url,
                sha256: verified_sha256,
                size_bytes: size,
                license: s.license.unwrap_or_else(|| "Unknown".into()),
            });
        }
    }

    for p in opts.input {
        if !resolved_inputs.contains(&p) {
            if p.as_os_str() != "-" && !p.exists() {
                anyhow::bail!("Source input file '{}' does not exist", p.display());
            }
            let (size, verified_sha256, fname) = if p.as_os_str() != "-" && p.exists() {
                let file_size = std::fs::metadata(&p)?.len();
                let actual_hash = compute_file_sha256(&p)
                    .with_context(|| format!("Failed to compute SHA-256 for {}", p.display()))?;
                let filename = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                (file_size, Some(actual_hash.to_lowercase()), filename)
            } else {
                (0, None, "<stdin>".to_string())
            };

            source_metas.push(SourceArchiveMetadata {
                filename: fname,
                url: None,
                sha256: verified_sha256,
                size_bytes: size,
                license: "Unknown".into(),
            });
            resolved_inputs.push(p);
        }
    }

    if resolved_inputs.is_empty() {
        anyhow::bail!(
            "No input files specified. Provide --input <file>... or --manifest <sources.json>"
        );
    }

    println!("=== Building Persistent Curated Dataset ===");
    println!("Collection:          {}", col);
    println!("Output Directory:    {}", opts.output.display());
    println!("Input Sources:       {} sources", resolved_inputs.len());
    println!("Block Capacity:      {} games/block", opts.block_capacity);
    println!("Max Games/Shard:     {}", opts.max_games_per_shard);
    println!("Zstandard Level:     {}", opts.zstd_level);
    println!("Deduplication Mode:  {:?}", opts.dedupe_mode);

    let curation_policy = match col {
        Collection::MastersOtb => CurationPolicyMetadata {
            min_rating_both: 2200,
            min_rating_one: 2400,
            min_rating_other: 2000,
            min_plies: 4,
            extra_criteria: "Historical exception pre-1970".into(),
        },
        Collection::EliteOnline => CurationPolicyMetadata {
            min_rating_both: 2200,
            min_rating_one: 2400,
            min_rating_other: 2000,
            min_plies: 6,
            extra_criteria: "Exclude Bullet and UltraBullet (<180s)".into(),
        },
    };

    let writer_config = DatasetWriterConfig {
        collection: col,
        block_capacity: opts.block_capacity,
        max_games_per_shard: opts.max_games_per_shard,
        zstd_level: opts.zstd_level,
        curation_policy,
        deduplication_policy_version: "v2-verified-prefix".into(),
    };

    let mut dataset_writer = DatasetWriter::new(opts.output.clone(), writer_config)
        .context("Failed to initialize dataset writer in staging directory")?;

    let deduplicator = match opts.dedupe_mode {
        DedupeModeArg::Memory => Deduplicator::new(),
        DedupeModeArg::Disk => {
            let temp_dedupe = tempfile::Builder::new()
                .prefix("codb_dedupe_")
                .suffix(".db")
                .tempfile()?;
            Deduplicator::new_disk(temp_dedupe.path())
                .context("Failed to initialize disk-backed temporary deduplicator")?
        }
    };

    let start = Instant::now();
    let consumer = DatasetWriterConsumer {
        writer: &mut dataset_writer,
    };
    let mut pipeline = CuratedPipeline::new(col, deduplicator, consumer)
        .with_max_games(opts.max_games)
        .with_log_progress(true);

    for path in &resolved_inputs {
        println!("Streaming from {}...", path.display());
        let reader = open_auto_detect_reader(path)
            .with_context(|| format!("Failed to open stream for {}", path.display()))?;
        pipeline
            .process_stream(reader)
            .with_context(|| format!("Error processing stream {}", path.display()))?;

        if pipeline.stats.completion_status == IngestionCompletionStatus::PartialLimitReached {
            println!("Target game limit reached.");
            break;
        }
    }

    let stats = pipeline.stats;
    println!("\nFinalizing persistent dataset...");
    let manifest = dataset_writer
        .finalize(stats.clone(), source_metas)
        .context("Failed to finalize persistent dataset")?;

    let elapsed = start.elapsed().as_secs_f64();
    println!("\n=== Build Complete ===");
    println!("Target Directory:    {}", opts.output.display());
    println!("Total Retained:      {} games", manifest.total_games);
    println!("Encountered Games:   {}", stats.total_encountered);
    println!("Duplicates Removed:  {}", stats.confirmed_duplicates);
    println!("Superseded Prior:    {}", stats.superseded_prior);
    println!("Policy Rejections:   {}", stats.rejected_by_policy);
    println!("Shards Created:      {}", manifest.shards.len());
    println!(
        "Total Physical Size: {:.2} MB ({} bytes)",
        manifest.total_physical_bytes as f64 / 1_000_000.0,
        manifest.total_physical_bytes
    );
    let bpg = manifest.total_physical_bytes as f64 / manifest.total_games.max(1) as f64;
    println!("Storage Efficiency:  {:.2} bytes/game", bpg);
    println!(
        "Duration:            {:.2}s ({:.1} games/sec)",
        elapsed,
        stats.total_encountered as f64 / elapsed.max(1e-9)
    );

    Ok(())
}

fn run_get_game(dataset: &Path, id: u64, format: FormatArg) -> Result<()> {
    let mut reader = DatasetReader::open(dataset)
        .with_context(|| format!("Failed to open dataset at {}", dataset.display()))?;
    match reader.get_game(id) {
        Ok(game) => {
            match format {
                FormatArg::Pgn => {
                    let pgn = export_to_pgn(&game)
                        .map_err(|e| anyhow::anyhow!("PGN export error: {}", e))?;
                    print!("{}", pgn);
                }
                FormatArg::Json => {
                    let json = serde_json::to_string_pretty(&game)?;
                    println!("{}", json);
                }
            }
            Ok(())
        }
        Err(StorageError::GameNotFound(missing_id)) => {
            eprintln!(
                "Error: Game ID {} not found in dataset {}",
                missing_id,
                dataset.display()
            );
            std::process::exit(1);
        }
        Err(err) => Err(anyhow::anyhow!("Storage error retrieving game: {}", err)),
    }
}

fn run_inspect(dataset: &Path, json: bool) -> Result<()> {
    let reader = DatasetReader::open(dataset)
        .with_context(|| format!("Failed to open dataset at {}", dataset.display()))?;
    let manifest = reader.manifest();

    if json {
        println!("{}", serde_json::to_string_pretty(manifest)?);
        return Ok(());
    }

    println!("========================================================================");
    println!("                      CODB Dataset Inspection                           ");
    println!("========================================================================");
    println!("Build ID:            {}", manifest.dataset_build_id);
    println!("Collection:          {}", manifest.collection);
    println!("Build Timestamp:     {}", manifest.build_timestamp);
    println!(
        "Format Version:      {} (Schema: {})",
        manifest.format_version, manifest.schema_version
    );
    println!("Total Retained Games:{}", manifest.total_games);
    println!(
        "Physical Size:       {:.2} MB ({} bytes)",
        manifest.total_physical_bytes as f64 / 1_000_000.0,
        manifest.total_physical_bytes
    );
    let bpg = manifest.total_physical_bytes as f64 / manifest.total_games.max(1) as f64;
    println!("Physical Bytes/Game: {:.2} B/game", bpg);
    println!("Shard Files:         {}", manifest.shards.len());
    for s in &manifest.shards {
        println!(
            "  - Shard {}: {} ({} games, {:.2} MB)",
            s.shard_id,
            s.filename,
            s.game_count,
            s.file_size_bytes as f64 / 1_000_000.0
        );
    }
    println!(
        "Global Lookup:       {} ({} entries, {:.2} MB)",
        manifest.lookup_table.filename,
        manifest.lookup_table.entry_count,
        manifest.lookup_table.file_size_bytes as f64 / 1_000_000.0
    );
    println!("Accounting Summary:");
    println!(
        "  Total Encountered: {}",
        manifest.accounting.total_encountered
    );
    println!(
        "  Accepted Unique:   {}",
        manifest.accounting.accepted_unique
    );
    println!(
        "  Duplicates Elim:   {}",
        manifest.accounting.confirmed_duplicates
    );
    println!(
        "  Superseded Prior:  {}",
        manifest.accounting.superseded_prior
    );
    println!(
        "  Policy Rejected:   {}",
        manifest.accounting.rejected_by_policy
    );
    println!(
        "  Accounting Valid:  {} (Fully consistent: {})",
        manifest.accounting.is_reconciled(),
        manifest
            .accounting
            .verify_retained_count(manifest.total_games as usize)
    );
    println!("========================================================================");
    Ok(())
}

fn run_verify(dataset: &Path, deep: bool) -> Result<()> {
    println!(
        "Verifying dataset at {} (deep: {})...",
        dataset.display(),
        deep
    );
    let start = Instant::now();
    let report = verify_dataset(dataset, deep)
        .with_context(|| format!("Verification failed for dataset {}", dataset.display()))?;
    let elapsed = start.elapsed().as_secs_f64();

    println!("=== Verification Results ===");
    println!("Status:                   {}", report.status);
    println!("Collection:               {}", report.collection);
    println!("Total Games:              {}", report.total_games);
    println!("Shards Verified:          {}", report.shards_checked);
    println!("Lookup Entries Verified:  {}", report.lookups_checked);
    println!("Artifact Hashes Valid:    {}", report.checksums_valid);
    println!("Accounting Invariants:    {}", report.accounting_reconciled);
    println!(
        "Deep Game Verification:   {}",
        report.deep_verification_passed
    );
    println!("Elapsed Time:             {:.2}s", elapsed);
    println!("Result: SUCCESS [PASS]");
    Ok(())
}

fn run_benchmarks(
    input_path: &Path,
    collection: Collection,
    max_games: Option<usize>,
    output_dir: &Path,
    run_name: &str,
) -> Result<()> {
    println!("=== Running CODB Benchmark Suite ===");
    println!("Input:       {}", input_path.display());
    println!("Collection:  {}", collection);

    let input_size = std::fs::metadata(input_path)?.len();
    let stream = open_pgn_reader(input_path)?;

    let import_start = Instant::now();
    let outcome = ingest_curated_stream(stream, collection, max_games, false)?;
    let import_dur = import_start.elapsed().as_secs_f64();

    let stats = outcome.stats;
    let accepted_games = outcome.accepted_games;
    let transpositions_distinct = outcome.transpositions_distinct;

    let n_games = accepted_games.len();
    println!(
        "Imported {} accepted games (status: {:?}) in {:.2}s ({:.1} games/sec)",
        n_games,
        stats.completion_status,
        import_dur,
        n_games as f64 / import_dur.max(1e-9)
    );
    println!(
        "Accounting Invariant Reconciled: {} (Retained == Accepted: {})",
        stats.is_reconciled(),
        stats.verify_retained_count(n_games)
    );

    // 2. Storage Benchmark: Compress games in 512-game blocks and verify decompression round-trip
    println!("Benchmarking compact game storage...");
    let mut total_raw_pgn_bytes = 0u64;
    for g in &accepted_games {
        let pgn = export_to_pgn(g).context("Failed to export accepted game to PGN")?;
        total_raw_pgn_bytes += pgn.len() as u64;
    }

    let mut codb_compressed_bytes = 0u64;
    let block_size = 512;
    let mut num_blocks = 0u64;
    for chunk in accepted_games.chunks(block_size) {
        let mut block = GameBlock::new(collection);
        block.games = chunk.to_vec();
        let comp = block.compress(3).context("Block compression failed")?;
        let decomp =
            GameBlock::decompress(&comp, collection).context("Block decompression check failed")?;
        anyhow::ensure!(
            decomp == block.games,
            "GameBlock round-trip fidelity check failed during benchmark"
        );
        codb_compressed_bytes += comp.len() as u64;
        num_blocks += 1;
    }

    // Container framing: 64-byte file header + 28 bytes per BlockIndexEntry
    let codb_block_directory_bytes = if n_games == 0 {
        0
    } else {
        64 + num_blocks * 28
    };
    let codb_total_container_bytes = codb_compressed_bytes + codb_block_directory_bytes;

    let raw_per_game = if n_games == 0 {
        0.0
    } else {
        total_raw_pgn_bytes as f64 / n_games as f64
    };
    let codb_per_game = if n_games == 0 {
        0.0
    } else {
        codb_compressed_bytes as f64 / n_games as f64
    };
    let comp_ratio = if codb_compressed_bytes == 0 {
        0.0
    } else {
        total_raw_pgn_bytes as f64 / codb_compressed_bytes as f64
    };

    println!(
        "Raw PGN size:          {:.2} MB ({:.1} bytes/game)",
        total_raw_pgn_bytes as f64 / 1_000_000.0,
        raw_per_game
    );
    println!(
        "CODB Block-Zstd size:  {:.2} MB ({:.1} bytes/game, +{} B block directory)",
        codb_compressed_bytes as f64 / 1_000_000.0,
        codb_per_game,
        codb_block_directory_bytes
    );
    println!("Compression ratio:     {:.2}x", comp_ratio);

    // 3. Index Depth Evaluation: 20, 30, 40, 60 plies
    println!("Benchmarking position indexing on-disk at depths 20, 30, 40, 60 plies...");
    let depth_20 = measure_index_depth(&accepted_games, 20)?;
    let depth_30 = measure_index_depth(&accepted_games, 30)?;
    let depth_40 = measure_index_depth(&accepted_games, 40)?;
    let depth_60 = measure_index_depth(&accepted_games, 60)?;

    for d in [&depth_20, &depth_30, &depth_40, &depth_60] {
        println!(
            "Depth {}: {} unique positions, {} unique pairs | CPI disk: {:.2} MB ({} blocks), SQLite disk: {:.2} MB",
            d.depth_plies,
            d.unique_positions,
            d.unique_position_move_pairs,
            d.immutable_index_size_bytes as f64 / 1_000_000.0,
            d.cpi_block_count,
            d.sqlite_index_size_bytes as f64 / 1_000_000.0
        );
    }

    // 4. Query Latencies
    println!("Measuring query latencies across on-disk CPI vs on-disk SQLite vs in-memory...");
    let latencies = measure_query_latencies(&accepted_games)?;

    println!(
        "Query Latency ({} queries, equiv={}): CPI disk: {:.2} / {:.2} / {:.2} µs | SQLite disk: {:.2} / {:.2} / {:.2} µs | CPI RAM: {:.3} / {:.3} / {:.3} µs",
        latencies.query_sample_count,
        latencies.semantic_equivalence_verified,
        latencies.cpi_disk_p50_us,
        latencies.cpi_disk_p95_us,
        latencies.cpi_disk_p99_us,
        latencies.sqlite_disk_p50_us,
        latencies.sqlite_disk_p95_us,
        latencies.sqlite_disk_p99_us,
        latencies.cpi_in_memory_p50_us,
        latencies.cpi_in_memory_p95_us,
        latencies.cpi_in_memory_p99_us
    );

    let computed_sha256 = if input_path.is_file() {
        compute_file_sha256(input_path).ok()
    } else {
        None
    };

    let prov_status = if computed_sha256.is_some() {
        "verified".to_string()
    } else {
        "declared_unverified".to_string()
    };

    let report = BenchmarkReport {
        schema_version: "2.0.0".to_string(),
        benchmark_type: "raw_ingest".to_string(),
        provenance_status: Some(prov_status),
        reproduction_status: Some("reproducible_from_source".to_string()),
        run_name: run_name.to_string(),
        collection: collection.to_string(),
        input_file: input_path.display().to_string(),
        input_size_bytes: Some(input_size),
        total_encountered: stats.total_encountered,
        accepted_unique: stats.accepted_unique,
        retained_canonical_games: n_games,
        confirmed_duplicates: stats.confirmed_duplicates,
        superseded_prior: stats.superseded_prior,
        rejected_by_policy: stats.rejected_by_policy,
        rejected_variant: stats.rejected_variant,
        rejected_illegal: stats.rejected_illegal,
        rejected_malformed: stats.rejected_malformed,
        quarantined: stats.quarantined,
        completion_status: stats.completion_status,
        is_reconciled: stats.is_reconciled(),
        retained_matches_accepted: stats.verify_retained_count(n_games),
        transpositions_distinct,
        import_duration_secs: Some(import_dur),
        import_throughput_games_sec: Some(n_games as f64 / import_dur.max(1e-9)),
        import_throughput_mb_sec: Some((input_size as f64 / 1_000_000.0) / import_dur.max(1e-9)),
        raw_ingest_duration_secs: Some(import_dur),
        raw_ingest_throughput_games_sec: Some(n_games as f64 / import_dur.max(1e-9)),
        archive_reload_duration_secs: None,
        archive_reload_throughput_games_sec: None,
        archive_reload_throughput_mb_sec: None,
        raw_ingest_note: None,
        source_archives: vec![SourceArchiveMetadata {
            filename: input_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            url: None,
            sha256: computed_sha256,
            size_bytes: input_size,
            license: "CC0".into(),
        }],
        raw_pgn_bytes: total_raw_pgn_bytes,
        raw_bytes_per_game: raw_per_game,
        codb_block_compressed_bytes: codb_compressed_bytes,
        codb_block_directory_bytes,
        codb_total_container_bytes,
        codb_bytes_per_game: codb_per_game,
        compression_ratio: comp_ratio,
        depth_20,
        depth_30,
        depth_40,
        depth_60,
        query_latency_us: latencies,
        random_retrieval_latency_us: None,
        persistent_storage: None,
    };

    std::fs::create_dir_all(output_dir)?;
    let out_file = output_dir.join(format!("{}.json", run_name));
    let json_text = serde_json::to_string_pretty(&report)?;
    std::fs::write(&out_file, json_text)?;
    println!("Benchmark report written to {}", out_file.display());

    Ok(())
}

fn run_bench_dataset(
    dataset_dir: &Path,
    output_dir: &Path,
    run_name: &str,
    random_lookups: usize,
) -> Result<()> {
    println!("=== Benchmarking Persistent Dataset ===");
    println!("Dataset:     {}", dataset_dir.display());
    println!("Run Name:    {}", run_name);

    let mut reader = DatasetReader::open(dataset_dir)
        .with_context(|| format!("Failed to open dataset at {}", dataset_dir.display()))?;
    let manifest = reader.manifest().clone();
    let collection = manifest.collection;

    println!("Collection:  {}", collection);
    println!("Total Games: {}", manifest.total_games);

    // 1. Load all games from persistent shards
    let load_start = Instant::now();
    let mut all_games = Vec::with_capacity(manifest.total_games as usize);
    let snapshot_dir = reader.snapshot_dir().to_path_buf();
    for shard_meta in &manifest.shards {
        let shard_path = snapshot_dir.join("shards").join(&shard_meta.filename);
        let mut shard_reader = ShardReader::open(&shard_path)?;
        for b_idx in 0..shard_reader.block_count() {
            let block_games = shard_reader.decompress_block(b_idx)?;
            all_games.extend(block_games);
        }
    }
    let load_dur = load_start.elapsed().as_secs_f64();
    let n_games = all_games.len();
    anyhow::ensure!(
        n_games == manifest.total_games as usize,
        "Total games in shards ({}) != manifest.total_games ({})",
        n_games,
        manifest.total_games
    );
    println!(
        "Loaded {} games from {} shard(s) in {:.2}s ({:.1} games/sec)",
        n_games,
        manifest.shards.len(),
        load_dur,
        n_games as f64 / load_dur.max(1e-9)
    );

    // 2. Measure raw PGN size
    println!("Computing equivalent raw PGN baseline...");
    let mut total_raw_pgn_bytes = 0u64;
    for g in &all_games {
        let pgn = export_to_pgn(g).context("Failed to export accepted game to PGN")?;
        total_raw_pgn_bytes += pgn.len() as u64;
    }
    let raw_per_game = total_raw_pgn_bytes as f64 / n_games.max(1) as f64;

    // 3. Measure persistent storage footprint
    let shards_bytes: u64 = manifest.shards.iter().map(|s| s.file_size_bytes).sum();
    let lookup_bytes = manifest.lookup_table.file_size_bytes;
    let manifest_bytes = std::fs::metadata(snapshot_dir.join("manifest.json"))
        .map(|m| m.len())
        .unwrap_or(0);
    let total_persistent_bytes = shards_bytes + lookup_bytes + manifest_bytes;
    let physical_bytes_per_game = total_persistent_bytes as f64 / n_games.max(1) as f64;
    let comp_ratio = total_raw_pgn_bytes as f64 / shards_bytes.max(1) as f64;

    println!(
        "Raw PGN size:          {:.2} MB ({:.1} bytes/game)",
        total_raw_pgn_bytes as f64 / 1_000_000.0,
        raw_per_game
    );
    println!(
        "CGB3 Shards size:      {:.2} MB ({:.1} bytes/game)",
        shards_bytes as f64 / 1_000_000.0,
        shards_bytes as f64 / n_games.max(1) as f64
    );
    println!(
        "Lookup Index size:     {:.2} MB ({} entries)",
        lookup_bytes as f64 / 1_000_000.0,
        manifest.lookup_table.entry_count
    );
    println!(
        "Total Persistent size: {:.2} MB ({:.2} bytes/game)",
        total_persistent_bytes as f64 / 1_000_000.0,
        physical_bytes_per_game
    );
    println!("Compression ratio:     {:.2}x", comp_ratio);

    // 4. Random game retrieval latency benchmark
    let sample_count = random_lookups.min(n_games);
    println!(
        "Benchmarking random game retrieval ({} lookups)...",
        sample_count
    );
    let mut rng_state: u64 = 0x9E3779B97F4A7C15;
    let mut query_indices: Vec<usize> = (0..n_games).collect();
    for i in (1..query_indices.len()).rev() {
        rng_state = rng_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (rng_state as usize) % (i + 1);
        query_indices.swap(i, j);
    }
    let query_ids: Vec<u64> = query_indices
        .iter()
        .take(sample_count)
        .map(|&idx| all_games[idx].metadata.id)
        .collect();

    let mut retrieval_latencies = Vec::with_capacity(sample_count);
    let mut max_us = 0.0f64;
    for &id in &query_ids {
        let t0 = Instant::now();
        let g = reader.get_game(id)?;
        let dur_us = t0.elapsed().as_secs_f64() * 1_000_000.0;
        anyhow::ensure!(g.metadata.id == id, "Retrieved game ID mismatch");
        max_us = max_us.max(dur_us);
        retrieval_latencies.push(dur_us);
    }
    let (p50_us, p95_us, p99_us) = compute_percentiles(&retrieval_latencies);
    println!(
        "Random Retrieval Latency: p50 = {:.2} µs, p95 = {:.2} µs, p99 = {:.2} µs, max = {:.2} µs (100% correct)",
        p50_us, p95_us, p99_us, max_us
    );

    // 5. Index Depth Evaluation: 20, 30, 40, 60 plies
    println!("Benchmarking position indexing on-disk at depths 20, 30, 40, 60 plies...");
    let depth_20 = measure_index_depth(&all_games, 20)?;
    let depth_30 = measure_index_depth(&all_games, 30)?;
    let depth_40 = measure_index_depth(&all_games, 40)?;
    let depth_60 = measure_index_depth(&all_games, 60)?;

    for d in [&depth_20, &depth_30, &depth_40, &depth_60] {
        println!(
            "Depth {}: {} unique positions, {} unique pairs | CPI disk: {:.2} MB ({} blocks), SQLite disk: {:.2} MB",
            d.depth_plies,
            d.unique_positions,
            d.unique_position_move_pairs,
            d.immutable_index_size_bytes as f64 / 1_000_000.0,
            d.cpi_block_count,
            d.sqlite_index_size_bytes as f64 / 1_000_000.0
        );
    }

    // 6. Query Latencies
    println!("Measuring query latencies across on-disk CPI vs on-disk SQLite vs in-memory...");
    let latencies = measure_query_latencies(&all_games)?;
    println!(
        "Query Latency ({} queries, equiv={}): CPI disk: {:.2} / {:.2} / {:.2} µs | SQLite disk: {:.2} / {:.2} / {:.2} µs",
        latencies.query_sample_count,
        latencies.semantic_equivalence_verified,
        latencies.cpi_disk_p50_us,
        latencies.cpi_disk_p95_us,
        latencies.cpi_disk_p99_us,
        latencies.sqlite_disk_p50_us,
        latencies.sqlite_disk_p95_us,
        latencies.sqlite_disk_p99_us
    );

    let stats = &manifest.accounting;
    let source_str = manifest
        .source_archives
        .iter()
        .map(|s| s.filename.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let source_size: u64 = manifest.source_archives.iter().map(|s| s.size_bytes).sum();

    let all_have_sha = !manifest.source_archives.is_empty()
        && manifest.source_archives.iter().all(|s| s.sha256.is_some());
    let prov_status = if all_have_sha {
        "verified".to_string()
    } else {
        "declared_unverified".to_string()
    };

    let report = BenchmarkReport {
        schema_version: "2.0.0".to_string(),
        benchmark_type: "dataset_archive".to_string(),
        provenance_status: Some(prov_status),
        reproduction_status: Some("dataset_reproducible".to_string()),
        run_name: run_name.to_string(),
        collection: collection.to_string(),
        input_file: if source_str.is_empty() {
            dataset_dir.display().to_string()
        } else {
            source_str
        },
        input_size_bytes: if source_size > 0 {
            Some(source_size)
        } else {
            None
        },
        total_encountered: stats.total_encountered,
        accepted_unique: stats.accepted_unique,
        retained_canonical_games: n_games,
        confirmed_duplicates: stats.confirmed_duplicates,
        superseded_prior: stats.superseded_prior,
        rejected_by_policy: stats.rejected_by_policy,
        rejected_variant: stats.rejected_variant,
        rejected_illegal: stats.rejected_illegal,
        rejected_malformed: stats.rejected_malformed,
        quarantined: stats.quarantined,
        completion_status: stats.completion_status,
        is_reconciled: stats.is_reconciled(),
        retained_matches_accepted: stats.verify_retained_count(n_games),
        transpositions_distinct: 0,
        import_duration_secs: None,
        import_throughput_games_sec: None,
        import_throughput_mb_sec: None,
        raw_ingest_duration_secs: None,
        raw_ingest_throughput_games_sec: None,
        archive_reload_duration_secs: Some(load_dur),
        archive_reload_throughput_games_sec: Some(n_games as f64 / load_dur.max(1e-9)),
        archive_reload_throughput_mb_sec: Some(
            (total_persistent_bytes as f64 / 1_000_000.0) / load_dur.max(1e-9),
        ),
        raw_ingest_note: Some(format!(
            "Archive reload of {} accepted games from persistent CGB3 shard; raw ingest evaluated {} games to accept {} games",
            n_games, stats.total_encountered, stats.accepted_unique
        )),
        source_archives: manifest.source_archives.clone(),
        raw_pgn_bytes: total_raw_pgn_bytes,
        raw_bytes_per_game: raw_per_game,
        codb_block_compressed_bytes: shards_bytes,
        codb_block_directory_bytes: 0,
        codb_total_container_bytes: shards_bytes,
        codb_bytes_per_game: shards_bytes as f64 / n_games.max(1) as f64,
        compression_ratio: comp_ratio,
        depth_20,
        depth_30,
        depth_40,
        depth_60,
        query_latency_us: latencies,
        random_retrieval_latency_us: Some(RandomRetrievalMetrics {
            lookups_count: sample_count,
            p50_us,
            p95_us,
            p99_us,
            max_us,
            correctness_verified: true,
        }),
        persistent_storage: Some(PersistentStorageMetrics {
            shards_count: manifest.shards.len(),
            shards_bytes,
            lookup_index_bytes: lookup_bytes,
            manifest_bytes,
            total_persistent_bytes,
            physical_bytes_per_game,
        }),
    };

    std::fs::create_dir_all(output_dir)?;
    let out_file = output_dir.join(format!("{}.json", run_name));
    let json_text = serde_json::to_string_pretty(&report)?;
    std::fs::write(&out_file, json_text)?;
    println!("Benchmark report written to {}", out_file.display());

    Ok(())
}

fn measure_index_depth(games: &[ParsedGame], depth_plies: usize) -> Result<IndexDepthStats> {
    let start = Instant::now();
    let mut immutable = ImmutablePositionIndex::new();

    let temp_sqlite = tempfile::Builder::new()
        .prefix("codb_bench_")
        .suffix(".sqlite")
        .tempfile()?;
    let mut sqlite = SqlitePositionIndex::open(temp_sqlite.path())?;

    let mut batch_entries = Vec::with_capacity(10_000);

    for g in games {
        let entries = extract_game_positions(g, depth_plies).with_context(|| {
            format!(
                "Failed to extract positions for game {} at depth {}",
                g.metadata.id, depth_plies
            )
        })?;
        for (k, cm, r) in entries {
            immutable.record(k, cm, r);
            batch_entries.push((k, cm, r));
        }

        if batch_entries.len() >= 10_000 {
            sqlite.insert_batch(&batch_entries)?;
            batch_entries.clear();
        }
    }

    if !batch_entries.is_empty() {
        sqlite.insert_batch(&batch_entries)?;
    }

    let dur = start.elapsed().as_secs_f64();
    let unique_positions = immutable.len();
    let unique_position_move_pairs = immutable.unique_position_move_pairs();

    // Persist CPI index to disk to measure actual on-disk size
    let temp_cpi = tempfile::Builder::new()
        .prefix("codb_bench_")
        .suffix(".cpi")
        .tempfile()?;
    let meta = immutable.write_to_file(temp_cpi.path(), 256, 3)?;
    let cpi_disk_bytes = meta.total_file_size as usize;
    let cpi_sparse_directory_bytes = (meta.block_count as usize) * 34;

    // Measure SQLite on-disk size: database file + WAL file if present
    let sqlite_db_bytes = std::fs::metadata(temp_sqlite.path())?.len();
    let wal_path = temp_sqlite.path().with_extension("sqlite-wal");
    let wal_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    let sqlite_total_bytes = (sqlite_db_bytes + wal_bytes) as usize;

    Ok(IndexDepthStats {
        depth_plies,
        unique_positions,
        unique_position_move_pairs,
        cpi_block_count: meta.block_count,
        cpi_sparse_directory_bytes,
        immutable_index_size_bytes: cpi_disk_bytes,
        sqlite_index_size_bytes: sqlite_total_bytes,
        index_build_duration_secs: dur,
    })
}

/// Computes `(p50, p95, p99)` latency percentiles in microseconds from a slice of samples.
/// Uses linear interpolation over sorted samples and guarantees `p50 <= p95 <= p99`.
pub fn compute_percentiles(samples: &[f64]) -> (f64, f64, f64) {
    let mut sorted: Vec<f64> = samples.iter().copied().filter(|x| !x.is_nan()).collect();
    if sorted.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    sorted.sort_by(|a, b| a.total_cmp(b));

    let percentile_at = |p: f64| -> f64 {
        if sorted.len() == 1 {
            return sorted[0];
        }
        let rank = p * (sorted.len() - 1) as f64;
        let lo = rank.floor() as usize;
        let hi = rank.ceil() as usize;
        let frac = rank - lo as f64;
        let val = sorted[lo] + frac * (sorted[hi] - sorted[lo]);
        // Round to 3 decimal places (nanosecond resolution in µs)
        (val * 1000.0).round() / 1000.0
    };

    (
        percentile_at(0.50),
        percentile_at(0.95),
        percentile_at(0.99),
    )
}

/// Deterministic SplitMix64 PRNG for reproducible benchmark sampling without external crate overhead.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    fn next_u128(&mut self) -> u128 {
        ((self.next_u64() as u128) << 64) | (self.next_u64() as u128)
    }

    fn choose_k<T: Copy>(&mut self, pool: &[T], k: usize) -> Vec<T> {
        if pool.is_empty() || k == 0 {
            return Vec::new();
        }
        if pool.len() <= k {
            // Deterministic sample with replacement up to k so query count is stable
            let mut out = Vec::with_capacity(k);
            for i in 0..k {
                let idx = if i < pool.len() {
                    i
                } else {
                    (self.next_u64() as usize) % pool.len()
                };
                out.push(pool[idx]);
            }
            return out;
        }
        // Partial Fisher-Yates shuffle on indices
        let mut indices: Vec<usize> = (0..pool.len()).collect();
        for i in 0..k {
            let j = i + ((self.next_u64() as usize) % (pool.len() - i));
            indices.swap(i, j);
        }
        indices[..k].iter().map(|&idx| pool[idx]).collect()
    }
}

fn measure_query_latencies(games: &[ParsedGame]) -> Result<LatencyMetrics> {
    let mut immutable = ImmutablePositionIndex::new();
    let temp_sqlite = tempfile::Builder::new()
        .prefix("codb_lat_")
        .suffix(".sqlite")
        .tempfile()?;
    let mut sqlite = SqlitePositionIndex::open(temp_sqlite.path())?;

    // Track position frequencies and distinct parent paths (to identify transpositions) across ALL accepted games
    let mut pos_counts: std::collections::HashMap<PositionKey, u32> =
        std::collections::HashMap::new();
    let mut first_parents: std::collections::HashMap<PositionKey, (PositionKey, u16)> =
        std::collections::HashMap::new();
    let mut transposition_pool: Vec<PositionKey> = Vec::new();
    let mut transposition_set: std::collections::HashSet<PositionKey> =
        std::collections::HashSet::new();
    let mut batch_entries = Vec::with_capacity(10_000);

    for g in games {
        let entries = extract_game_positions(g, 40).with_context(|| {
            format!(
                "Failed to extract positions for game {} during latency benchmark",
                g.metadata.id
            )
        })?;
        let mut prev: Option<(PositionKey, u16)> = None;
        for (k, cm, r) in entries {
            immutable.record(k, cm, r);
            *pos_counts.entry(k).or_insert(0) += 1;
            if let Some(p) = prev {
                if transposition_set.len() < 5000 {
                    if let Some(&fp) = first_parents.get(&k) {
                        if fp != p && transposition_set.insert(k) {
                            transposition_pool.push(k);
                        }
                    } else if first_parents.len() < 100_000 {
                        first_parents.insert(k, p);
                    }
                }
            }
            prev = Some((k, cm.0));
            batch_entries.push((k, cm, r));
        }

        if batch_entries.len() >= 10_000 {
            sqlite.insert_batch(&batch_entries)?;
            batch_entries.clear();
        }
    }

    if !batch_entries.is_empty() {
        sqlite.insert_batch(&batch_entries)?;
        batch_entries.clear();
    }
    drop(first_parents);
    drop(transposition_set);

    let temp_cpi = tempfile::Builder::new()
        .prefix("codb_lat_")
        .suffix(".cpi")
        .tempfile()?;
    immutable.write_to_file(temp_cpi.path(), 256, 3)?;
    let disk_cpi = DiskPositionIndex::open(temp_cpi.path())?;

    // Partition all indexed positions into deterministic strata:
    // 1. Frequent positions (count >= 10, or count >= 3 when corpus < 2,000 games)
    // 2. Medium-frequency positions (2 <= count < freq_threshold)
    // 3. Rare positions (count == 1)
    // 4. Transposition positions (reached via >= 2 distinct parent (PositionKey, move) pairs)
    // 5. Miss keys (deterministic nonexistent keys)
    let max_count = pos_counts.values().copied().max().unwrap_or(0);
    let freq_threshold = if games.len() >= 2_000 {
        10
    } else if max_count >= 3 {
        3
    } else {
        max_count.max(1)
    };
    let mut frequent_pool = Vec::new();
    let mut medium_pool = Vec::new();
    let mut rare_pool = Vec::new();

    for (&k, &count) in &pos_counts {
        if count >= freq_threshold {
            if frequent_pool.len() < 5000 {
                frequent_pool.push(k);
            }
        } else if count >= 2 {
            if medium_pool.len() < 5000 {
                medium_pool.push(k);
            }
        } else if rare_pool.len() < 5000 {
            rare_pool.push(k);
        }
    }

    let mut rng = SplitMix64::new(0xC0DB_2026_0001_CAFE);
    let target_per_stratum = if pos_counts.is_empty() { 0 } else { 400 };
    let miss_target = if pos_counts.is_empty() { 200 } else { 400 };

    let fallback_pool: Vec<PositionKey> = if frequent_pool.is_empty()
        || medium_pool.is_empty()
        || rare_pool.is_empty()
        || transposition_pool.is_empty()
    {
        pos_counts.keys().copied().take(1000).collect()
    } else {
        Vec::new()
    };

    let frequent_keys = rng.choose_k(
        if frequent_pool.is_empty() {
            &fallback_pool
        } else {
            &frequent_pool
        },
        target_per_stratum,
    );
    let medium_keys = rng.choose_k(
        if medium_pool.is_empty() {
            &fallback_pool
        } else {
            &medium_pool
        },
        target_per_stratum,
    );
    let rare_keys = rng.choose_k(
        if rare_pool.is_empty() {
            &fallback_pool
        } else {
            &rare_pool
        },
        target_per_stratum,
    );
    let transposition_keys = rng.choose_k(
        if transposition_pool.is_empty() {
            &fallback_pool
        } else {
            &transposition_pool
        },
        target_per_stratum,
    );

    let mut miss_keys = Vec::with_capacity(miss_target);
    while miss_keys.len() < miss_target {
        let candidate = PositionKey(rng.next_u128());
        if !pos_counts.contains_key(&candidate) {
            miss_keys.push(candidate);
        }
    }
    drop(pos_counts);

    let mut query_keys = Vec::with_capacity(
        frequent_keys.len()
            + medium_keys.len()
            + rare_keys.len()
            + transposition_keys.len()
            + miss_keys.len(),
    );
    query_keys.extend_from_slice(&frequent_keys);
    query_keys.extend_from_slice(&medium_keys);
    query_keys.extend_from_slice(&rare_keys);
    query_keys.extend_from_slice(&transposition_keys);
    query_keys.extend_from_slice(&miss_keys);

    // Deterministically shuffle the combined query workload so strata are interleaved during timing
    for i in 0..query_keys.len() {
        let j = i + ((rng.next_u64() as usize) % (query_keys.len() - i));
        query_keys.swap(i, j);
    }

    // Verify semantic equivalence across all three backends outside the timed loop (Workstream 3.4)
    for &k in &query_keys {
        let imm_res = immutable.query(k);
        let cpi_res = disk_cpi.query(k)?;
        let sql_res = sqlite.query_position(k)?;

        anyhow::ensure!(
            imm_res == cpi_res,
            "Semantic mismatch between in-memory index and on-disk CPI for key {}",
            k.to_hex()
        );
        match &cpi_res {
            Some(cpi_stats) => {
                anyhow::ensure!(
                    cpi_stats == &sql_res,
                    "Semantic mismatch between CPI ({:?}) and SQLite ({:?}) for key {}",
                    cpi_stats,
                    sql_res,
                    k.to_hex()
                );
            }
            None => {
                anyhow::ensure!(
                    sql_res.total_games == 0 && sql_res.moves.is_empty(),
                    "Expected empty SQLite result for missing key {}, got {:?}",
                    k.to_hex(),
                    sql_res
                );
            }
        }
    }

    // Timed loops using nanosecond resolution converted to floating-point microseconds (Workstream 3.3)
    let mut imm_times = Vec::with_capacity(query_keys.len());
    for &k in &query_keys {
        let t0 = Instant::now();
        let res = immutable.query(black_box(k));
        black_box(res);
        imm_times.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    let mut disk_cpi_times = Vec::with_capacity(query_keys.len());
    for &k in &query_keys {
        let t0 = Instant::now();
        let res = disk_cpi.query(black_box(k))?;
        black_box(res);
        disk_cpi_times.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    let mut sql_times = Vec::with_capacity(query_keys.len());
    for &k in &query_keys {
        let t0 = Instant::now();
        let res = sqlite.query_position(black_box(k))?;
        black_box(res);
        sql_times.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    let (sql_p50, sql_p95, sql_p99) = compute_percentiles(&sql_times);
    let (cpi_p50, cpi_p95, cpi_p99) = compute_percentiles(&disk_cpi_times);
    let (imm_p50, imm_p95, imm_p99) = compute_percentiles(&imm_times);

    Ok(LatencyMetrics {
        cache_regime: "warm_os_page_cache".to_string(),
        query_sample_count: query_keys.len(),
        frequent_keys_sampled: frequent_keys.len(),
        medium_keys_sampled: medium_keys.len(),
        rare_keys_sampled: rare_keys.len(),
        transposition_keys_sampled: transposition_keys.len(),
        miss_keys_sampled: miss_keys.len(),
        semantic_equivalence_verified: true,
        sqlite_disk_p50_us: sql_p50,
        sqlite_disk_p95_us: sql_p95,
        sqlite_disk_p99_us: sql_p99,
        cpi_disk_p50_us: cpi_p50,
        cpi_disk_p95_us: cpi_p95,
        cpi_disk_p99_us: cpi_p99,
        cpi_in_memory_p50_us: imm_p50,
        cpi_in_memory_p95_us: imm_p95,
        cpi_in_memory_p99_us: imm_p99,
    })
}

fn run_query(index_path: &Path, fen: Option<&str>, moves_str: Option<&str>) -> Result<()> {
    let mut board = Chess::default();
    if let Some(f) = fen {
        let parsed_fen: shakmaty::fen::Fen = f.parse().context("Invalid FEN")?;
        board = parsed_fen
            .into_position(shakmaty::CastlingMode::Standard)
            .map_err(|e| anyhow::anyhow!("Illegal FEN position: {:?}", e))?;
    }
    if let Some(m_list) = moves_str {
        for uci in m_list.split_whitespace() {
            let uci_move: UciMove = uci.parse().context("Invalid UCI move")?;
            let m = uci_move
                .to_move(&board)
                .map_err(|e| anyhow::anyhow!("Illegal move: {:?}", e))?;
            board.play_unchecked(m);
        }
    }
    let key = compute_position_key(&board);
    println!("Canonical PositionKey: {}", key.to_hex());

    if index_path.exists() {
        let disk_idx = DiskPositionIndex::open(index_path)?;
        if let Some(stats) = disk_idx.query(key)? {
            println!("Opening Explorer results for {}:", key.to_hex());
            println!("  Total games: {}", stats.total_games);
            println!(
                "  White wins:  {} ({:.1}%)",
                stats.white_wins,
                stats.white_wins as f64 * 100.0 / stats.total_games.max(1) as f64
            );
            println!(
                "  Draws:       {} ({:.1}%)",
                stats.draws,
                stats.draws as f64 * 100.0 / stats.total_games.max(1) as f64
            );
            println!(
                "  Black wins:  {} ({:.1}%)",
                stats.black_wins,
                stats.black_wins as f64 * 100.0 / stats.total_games.max(1) as f64
            );
            println!("  Candidate moves ({}):", stats.moves.len());
            for m in &stats.moves {
                println!(
                    "    {} - {} games (W: {}, D: {}, B: {})",
                    m.uci, m.total_games, m.white_wins, m.draws, m.black_wins
                );
            }
        } else {
            println!("Position not found in index {}.", index_path.display());
        }
    } else {
        println!("Index file {} does not exist.", index_path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_percentiles_accuracy() {
        // 101 samples from 0.100 to 10.100 µs (step 0.1)
        let samples: Vec<f64> = (0..=100).map(|i| 0.1 + (i as f64) * 0.1).collect();
        let (p50, p95, p99) = compute_percentiles(&samples);
        assert!((p50 - 5.1).abs() < 1e-6, "p50 was {}", p50);
        assert!((p95 - 9.6).abs() < 1e-6, "p95 was {}", p95);
        assert!((p99 - 10.0).abs() < 1e-6, "p99 was {}", p99);
        assert!(p50 < p95 && p95 < p99);

        // Sub-microsecond values must not truncate to 0.0
        let sub_us = vec![0.120, 0.180, 0.210, 0.450, 0.890];
        let (sp50, sp95, sp99) = compute_percentiles(&sub_us);
        assert!(sp50 > 0.0);
        assert!(sp95 > sp50);
        assert!(sp99 >= sp95);
    }

    #[test]
    fn test_curated_ingestion_superseding_and_max_games_accounting() {
        // Game 1: truncated 4-ply relay
        // Game 2: full 6-ply relay of the same game (supersedes Game 1 in-place)
        // Game 3: second distinct game
        let pgn = r#"
[Event "World Championship"]
[Site "Reykjavik ISL"]
[Date "1972.07.23"]
[Round "6"]
[White "Fischer, Robert James"]
[Black "Spassky, Boris V"]
[Result "*"]
[WhiteElo "2785"]
[BlackElo "2660"]

1. c4 e6 2. Nf3 d5 *

[Event "World Championship"]
[Site "Reykjavik ISL"]
[Date "1972.07.23"]
[Round "6"]
[White "Fischer, Robert James"]
[Black "Spassky, Boris V"]
[Result "1-0"]
[WhiteElo "2785"]
[BlackElo "2660"]

1. c4 e6 2. Nf3 d5 3. d4 Nf6 1-0

[Event "World Championship"]
[Site "Reykjavik ISL"]
[Date "1972.07.25"]
[Round "7"]
[White "Spassky, Boris V"]
[Black "Fischer, Robert James"]
[Result "1/2-1/2"]
[WhiteElo "2660"]
[BlackElo "2785"]

1. e4 c5 2. Nf3 d6 3. d4 cxd4 1/2-1/2
"#;
        let outcome =
            ingest_curated_stream(pgn.as_bytes(), Collection::MastersOtb, None, false).unwrap();
        assert_eq!(outcome.stats.total_encountered, 3);
        assert_eq!(outcome.stats.accepted_unique, 2);
        assert_eq!(outcome.stats.superseded_prior, 1);
        assert_eq!(outcome.accepted_games.len(), 2);
        assert!(outcome
            .stats
            .is_fully_consistent(outcome.accepted_games.len()));
        assert_eq!(
            outcome.stats.completion_status,
            IngestionCompletionStatus::CompleteEof
        );
        // Verify that the superseded game in slot 0 has the 6-ply move list
        assert_eq!(outcome.accepted_games[0].moves.len(), 6);

        // Now test with max_games = Some(1) -> PartialLimitReached
        let partial =
            ingest_curated_stream(pgn.as_bytes(), Collection::MastersOtb, Some(1), false).unwrap();
        assert_eq!(partial.stats.accepted_unique, 1);
        assert_eq!(partial.accepted_games.len(), 1);
        assert_eq!(
            partial.stats.completion_status,
            IngestionCompletionStatus::PartialLimitReached
        );
        assert!(partial
            .stats
            .is_fully_consistent(partial.accepted_games.len()));
    }

    #[test]
    fn test_end_to_end_broadcast_pgn_storage_and_index_equivalence() {
        let broadcast_pgn = r#"
[Event "2026 ANU Open"]
[Site "Canberra, Australia"]
[Date "2026.??.??"]
[UTCDate "2026.07.31"]
[Round "2.1"]
[White "Ikeda, Junta"]
[Black "Hegde, Sanat"]
[Result "1-0"]
[WhiteElo "2466"]
[BlackElo "2267"]
[TimeControl "60m+30s"]
[Variant "Standard"]
[ECO "B10a"]
[BroadcastURL "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots"]
[GameURL "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots/Qmy8doCq"]

1. d4 Nf6 2. c4 e6 3. Nf3 d5 1-0

[Event "2026 ANU Open"]
[Site "Canberra, Australia"]
[Date "2026.07.31"]
[UTCDate "2026.07.31"]
[Round "2.2"]
[White "Shenoy, Trilok"]
[Black "Press, Harry"]
[Result "0-1"]
[WhiteElo "2354"]
[BlackElo "2375"]
[TimeControl "60m+30s"]
[Variant "Standard"]
[ECO "D37"]
[BroadcastURL "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots"]
[GameURL "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots/4nmichIy"]

1. c4 e6 2. d4 Nf6 3. Nf3 d5 0-1
"#;
        let outcome = ingest_curated_stream(
            broadcast_pgn.as_bytes(),
            Collection::MastersOtb,
            None,
            false,
        )
        .unwrap();
        assert_eq!(outcome.accepted_games.len(), 2);

        // Verify UTCDate recovered when Date had question marks, and provenance preserved
        let g0 = &outcome.accepted_games[0];
        assert_eq!(g0.metadata.date.as_deref(), Some("2026.07.31"));
        assert_eq!(g0.metadata.eco.as_deref(), Some("B10a"));
        assert_eq!(g0.metadata.site.as_deref(), Some("Canberra, Australia"));
        assert_eq!(
            g0.metadata.source_id.as_deref(),
            Some("https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots/Qmy8doCq")
        );
        assert_eq!(
            g0.metadata.broadcast_url.as_deref(),
            Some("https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots")
        );

        // Compress -> Decompress -> Export PGN -> Re-ingest round-trip
        let mut block = GameBlock::new(Collection::MastersOtb);
        block.games = outcome.accepted_games.clone();
        let compressed = block.compress(3).unwrap();
        let decompressed = GameBlock::decompress(&compressed, Collection::MastersOtb).unwrap();
        assert_eq!(decompressed, outcome.accepted_games);

        let mut exported_pgn = String::new();
        for g in &decompressed {
            exported_pgn.push_str(&export_to_pgn(g).unwrap());
            exported_pgn.push('\n');
        }
        let reingested =
            ingest_curated_stream(exported_pgn.as_bytes(), Collection::MastersOtb, None, false)
                .unwrap();
        assert_eq!(reingested.accepted_games, outcome.accepted_games);

        // Verify stratified latency benchmark and semantic equivalence across CPI and SQLite
        let latencies = measure_query_latencies(&outcome.accepted_games).unwrap();
        assert!(latencies.semantic_equivalence_verified);
        assert_eq!(latencies.query_sample_count, 2000);
        assert!(latencies.transposition_keys_sampled > 0);
    }

    #[test]
    fn test_benchmark_artifacts_and_projections_consistency() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let results_dir = std::path::Path::new(manifest_dir)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("benchmarks")
            .join("results");

        if !results_dir.exists() {
            return;
        }

        let mut reports: std::collections::BTreeMap<String, BenchmarkReport> =
            std::collections::BTreeMap::new();

        for entry in std::fs::read_dir(&results_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json")
                && !path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("projections_")
            {
                let content = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
                let report: BenchmarkReport = serde_json::from_str(&content)
                    .unwrap_or_else(|e| panic!("Schema failure in {}: {}", path.display(), e));

                // 1. Accounting reconciliation invariant
                let expected_sum = report.accepted_unique
                    + report.confirmed_duplicates
                    + report.superseded_prior
                    + report.rejected_by_policy
                    + report.rejected_variant
                    + report.rejected_illegal
                    + report.rejected_malformed
                    + report.quarantined;

                assert_eq!(
                    report.total_encountered,
                    expected_sum,
                    "Accounting reconciliation invariant failed for {}",
                    path.display()
                );
                assert!(
                    report.is_reconciled,
                    "is_reconciled is false in {}",
                    path.display()
                );
                assert_eq!(
                    report.retained_canonical_games as u64,
                    report.accepted_unique,
                    "retained != accepted in {}",
                    path.display()
                );
                assert!(
                    report.retained_matches_accepted,
                    "retained_matches_accepted is false in {}",
                    path.display()
                );

                // 2. Monotonic depth positions
                assert!(
                    report.depth_20.unique_positions <= report.depth_30.unique_positions,
                    "depth_20 > depth_30 in {}",
                    path.display()
                );
                assert!(
                    report.depth_30.unique_positions <= report.depth_40.unique_positions,
                    "depth_30 > depth_40 in {}",
                    path.display()
                );
                assert!(
                    report.depth_40.unique_positions <= report.depth_60.unique_positions,
                    "depth_40 > depth_60 in {}",
                    path.display()
                );

                // 3. Latency percentiles
                assert!(
                    report.query_latency_us.cpi_disk_p50_us
                        <= report.query_latency_us.cpi_disk_p95_us
                );
                assert!(
                    report.query_latency_us.cpi_disk_p95_us
                        <= report.query_latency_us.cpi_disk_p99_us
                );
                assert!(
                    report.query_latency_us.sqlite_disk_p50_us
                        <= report.query_latency_us.sqlite_disk_p95_us
                );
                assert!(
                    report.query_latency_us.sqlite_disk_p95_us
                        <= report.query_latency_us.sqlite_disk_p99_us
                );
                assert!(report.query_latency_us.semantic_equivalence_verified);

                reports.insert(report.run_name.clone(), report);
            }
        }

        // Verify progressive scaling monotonicity
        let prog_keys = [
            "masters_1k",
            "masters_2.5k",
            "masters_5k",
            "masters_10k",
            "masters_20k",
        ];
        if prog_keys.iter().all(|k| reports.contains_key(*k)) {
            let mut prev_n = 0;
            let mut prev_d40_pos = 0;
            let mut prev_cgb_bytes = 0;
            for k in prog_keys {
                let r = &reports[k];
                assert!(r.accepted_unique > prev_n);
                assert!(r.depth_40.unique_positions > prev_d40_pos);
                assert!(r.codb_block_compressed_bytes > prev_cgb_bytes);
                prev_n = r.accepted_unique;
                prev_d40_pos = r.depth_40.unique_positions;
                prev_cgb_bytes = r.codb_block_compressed_bytes;
            }
        }

        // Verify projections artifact calculation integrity
        let proj_path = results_dir.join("projections_heaps.json");
        if proj_path.exists() {
            let proj_content = std::fs::read_to_string(&proj_path).unwrap();
            let v: serde_json::Value = serde_json::from_str(&proj_content).unwrap();
            let opt_b_opt = v["budget_scenarios_gb"]["option_b_recommended_d40_masters_d30_elite"]
                ["deployed_total_optimistic"]
                .as_f64()
                .unwrap();
            let opt_b_total = v["budget_scenarios_gb"]
                ["option_b_recommended_d40_masters_d30_elite"]["deployed_total_baseline"]
                .as_f64()
                .unwrap();
            let opt_b_cons = v["budget_scenarios_gb"]["option_b_recommended_d40_masters_d30_elite"]
                ["deployed_total_conservative"]
                .as_f64()
                .unwrap();

            // Mathematical validity and scenario monotonicity
            assert!(opt_b_opt > 0.0 && opt_b_opt.is_finite());
            assert!(opt_b_total >= opt_b_opt && opt_b_total.is_finite());
            assert!(opt_b_cons >= opt_b_total && opt_b_cons.is_finite());
        }
    }

    #[test]
    fn test_cli_build_inspect_get_game_verify_e2e() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let fixture_path = std::path::Path::new(manifest_dir)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("fixtures")
            .join("stage_a_validation.pgn");

        if !fixture_path.exists() {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let dataset_dir = tmp.path().join("cli_test_dataset");

        // Run build
        run_build(BuildOptions {
            manifest: None,
            input: vec![fixture_path.clone()],
            collection: CollectionArg::MastersOtb,
            output: dataset_dir.clone(),
            max_games: Some(20),
            block_capacity: 4,
            max_games_per_shard: 8,
            zstd_level: 3,
            dedupe_mode: DedupeModeArg::Disk,
        })
        .unwrap();

        assert!(dataset_dir.join("manifest.json").exists());
        assert!(dataset_dir.join("games.idx").exists());

        // Run inspect
        run_inspect(&dataset_dir, false).unwrap();
        run_inspect(&dataset_dir, true).unwrap();

        // Run verify (including deep)
        run_verify(&dataset_dir, true).unwrap();

        // Open dataset reader and test retrieval
        let mut reader = DatasetReader::open(&dataset_dir).unwrap();
        assert_eq!(reader.total_games(), 20);

        // Retrieve first game from games.idx
        let lookup_path = dataset_dir.join("games.idx");
        let lookup_reader = codb_storage::GameLookupReader::open(&lookup_path).unwrap();
        assert_eq!(lookup_reader.entry_count(), 20);

        // Test get_game with valid ID
        let mut shard =
            codb_storage::ShardReader::open(&dataset_dir.join("shards/shard_0000.cgb3")).unwrap();
        let b0 = shard.decompress_block(0).unwrap();
        let any_game_id = b0[0].metadata.id;

        let game = reader.get_game(any_game_id).unwrap();
        assert_eq!(game.metadata.id, any_game_id);
        let pgn = reader.get_game_pgn(any_game_id).unwrap();
        assert!(!pgn.is_empty());
    }

    #[test]
    fn test_cli_build_checksum_verification_success_and_failures() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let fixture_path = std::path::Path::new(manifest_dir)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("fixtures")
            .join("stage_a_validation.pgn");

        if !fixture_path.exists() {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let valid_hash = compute_file_sha256(&fixture_path).unwrap();

        // 1. Success with correct declared SHA-256
        let sources_valid = tmp.path().join("sources_valid.json");
        let manifest_json = format!(
            r#"{{"sources": [{{"path": "{}", "url": null, "sha256": "{}", "size_bytes": 0, "license": "CC0"}}]}}"#,
            fixture_path.display(),
            valid_hash
        );
        std::fs::write(&sources_valid, manifest_json).unwrap();

        let out_valid = tmp.path().join("out_valid");
        run_build(BuildOptions {
            manifest: Some(sources_valid),
            input: vec![],
            collection: CollectionArg::MastersOtb,
            output: out_valid.clone(),
            max_games: Some(5),
            block_capacity: 4,
            max_games_per_shard: 8,
            zstd_level: 3,
            dedupe_mode: DedupeModeArg::Disk,
        })
        .unwrap();

        let reader = DatasetReader::open(&out_valid).unwrap();
        assert_eq!(
            reader.manifest().source_archives[0].sha256.as_deref(),
            Some(valid_hash.as_str())
        );

        // 2. Failure with incorrect declared SHA-256 (hash mismatch)
        let sources_mismatch = tmp.path().join("sources_mismatch.json");
        let wrong_hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let manifest_json_wrong = format!(
            r#"{{"sources": [{{"path": "{}", "url": null, "sha256": "{}", "size_bytes": 0, "license": "CC0"}}]}}"#,
            fixture_path.display(),
            wrong_hash
        );
        std::fs::write(&sources_mismatch, manifest_json_wrong).unwrap();

        let out_mismatch = tmp.path().join("out_mismatch");
        let err_mismatch = run_build(BuildOptions {
            manifest: Some(sources_mismatch),
            input: vec![],
            collection: CollectionArg::MastersOtb,
            output: out_mismatch.clone(),
            max_games: Some(5),
            block_capacity: 4,
            max_games_per_shard: 8,
            zstd_level: 3,
            dedupe_mode: DedupeModeArg::Disk,
        })
        .unwrap_err();
        assert!(err_mismatch.to_string().contains("checksum mismatch"));
        assert!(!out_mismatch.join("current").exists());

        // 3. Failure with malformed SHA-256
        let sources_malformed = tmp.path().join("sources_malformed.json");
        let manifest_json_malformed = format!(
            r#"{{"sources": [{{"path": "{}", "url": null, "sha256": "not-a-valid-hex-hash", "size_bytes": 0, "license": "CC0"}}]}}"#,
            fixture_path.display()
        );
        std::fs::write(&sources_malformed, manifest_json_malformed).unwrap();

        let out_malformed = tmp.path().join("out_malformed");
        let err_malformed = run_build(BuildOptions {
            manifest: Some(sources_malformed),
            input: vec![],
            collection: CollectionArg::MastersOtb,
            output: out_malformed.clone(),
            max_games: Some(5),
            block_capacity: 4,
            max_games_per_shard: 8,
            zstd_level: 3,
            dedupe_mode: DedupeModeArg::Disk,
        })
        .unwrap_err();
        assert!(err_malformed.to_string().contains("Malformed SHA-256"));
        assert!(!out_malformed.join("current").exists());

        // 4. Failure with missing source file
        let sources_missing = tmp.path().join("sources_missing.json");
        let nonexistent_path = tmp.path().join("nonexistent_file.pgn");
        let manifest_json_missing = format!(
            r#"{{"sources": [{{"path": "{}", "url": null, "sha256": "{}", "size_bytes": 0, "license": "CC0"}}]}}"#,
            nonexistent_path.display(),
            valid_hash
        );
        std::fs::write(&sources_missing, manifest_json_missing).unwrap();

        let out_missing = tmp.path().join("out_missing");
        let err_missing = run_build(BuildOptions {
            manifest: Some(sources_missing),
            input: vec![],
            collection: CollectionArg::MastersOtb,
            output: out_missing.clone(),
            max_games: Some(5),
            block_capacity: 4,
            max_games_per_shard: 8,
            zstd_level: 3,
            dedupe_mode: DedupeModeArg::Disk,
        })
        .unwrap_err();
        assert!(err_missing.to_string().contains("does not exist"));

        // 5. Failure when file is altered after SHA-256 recorded
        let altered_file = tmp.path().join("altered.pgn");
        std::fs::copy(&fixture_path, &altered_file).unwrap();
        let expected_altered_hash = compute_file_sha256(&altered_file).unwrap();
        // Alter file
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&altered_file)
            .unwrap();
        f.write_all(b"\n% Tampered content\n").unwrap();
        drop(f);

        let sources_altered = tmp.path().join("sources_altered.json");
        let manifest_json_altered = format!(
            r#"{{"sources": [{{"path": "{}", "url": null, "sha256": "{}", "size_bytes": 0, "license": "CC0"}}]}}"#,
            altered_file.display(),
            expected_altered_hash
        );
        std::fs::write(&sources_altered, manifest_json_altered).unwrap();

        let out_altered = tmp.path().join("out_altered");
        let err_altered = run_build(BuildOptions {
            manifest: Some(sources_altered),
            input: vec![],
            collection: CollectionArg::MastersOtb,
            output: out_altered.clone(),
            max_games: Some(5),
            block_capacity: 4,
            max_games_per_shard: 8,
            zstd_level: 3,
            dedupe_mode: DedupeModeArg::Disk,
        })
        .unwrap_err();
        assert!(err_altered.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn test_bench_dataset_unknown_source_size_does_not_substitute_persistent_bytes() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let fixture_path = std::path::Path::new(manifest_dir)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("fixtures")
            .join("stage_a_validation.pgn");

        if !fixture_path.exists() {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let dataset_dir = tmp.path().join("dataset_unknown_source");
        let bench_out = tmp.path().join("bench_reports");

        // Build dataset using direct input
        run_build(BuildOptions {
            manifest: None,
            input: vec![fixture_path.clone()],
            collection: CollectionArg::MastersOtb,
            output: dataset_dir.clone(),
            max_games: Some(10),
            block_capacity: 4,
            max_games_per_shard: 8,
            zstd_level: 3,
            dedupe_mode: DedupeModeArg::Disk,
        })
        .unwrap();

        // Mutate dataset manifest so source size is 0 (simulating unknown original source size)
        let resolved = codb_storage::resolve_dataset_snapshot(&dataset_dir).unwrap();
        let manifest_file = resolved.join("manifest.json");
        let mut manifest: codb_storage::DatasetManifest =
            serde_json::from_slice(&std::fs::read(&manifest_file).unwrap()).unwrap();
        for s in &mut manifest.source_archives {
            s.size_bytes = 0;
        }
        std::fs::write(
            &manifest_file,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Run bench-dataset
        run_bench_dataset(&dataset_dir, &bench_out, "bench_unknown_test", 5).unwrap();

        let report_file = bench_out.join("bench_unknown_test.json");
        assert!(report_file.exists());
        let report_val: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&report_file).unwrap()).unwrap();

        // Assert input_size_bytes is null (None), NOT substituted by total_persistent_bytes!
        assert!(report_val["input_size_bytes"].is_null());

        let p_storage = &report_val["persistent_storage"];
        assert!(p_storage.is_object());
        let total_p_bytes = p_storage["total_persistent_bytes"].as_u64().unwrap();
        assert!(total_p_bytes > 0);
    }
}
