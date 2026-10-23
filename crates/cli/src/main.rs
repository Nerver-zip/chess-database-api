use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use codb_core::{Collection, IngestionStats, ParsedGame, PositionKey};
use codb_curate::dedupe::{Deduplicator, DuplicateVerdict};
use codb_curate::policy::{EliteOnlinePolicy, MastersOtbPolicy, PolicyVerdict};
use codb_index::position::{
    compute_position_key, extract_game_positions, DiskPositionIndex, ImmutablePositionIndex,
    SqlitePositionIndex,
};
use codb_ingest::reader::{IngestError, ShakmatyGameVisitor};
use codb_storage::format::{export_to_pgn, GameBlock};
use pgn_reader::Reader;
use serde::Serialize;
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

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum CollectionArg {
    MastersOtb,
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
}

#[derive(Serialize)]
struct BenchmarkReport {
    run_name: String,
    collection: String,
    input_file: String,
    input_size_bytes: u64,
    total_encountered: u64,
    accepted_unique: u64,
    confirmed_duplicates: u64,
    superseded_prior: u64,
    rejected_by_policy: u64,
    rejected_variant: u64,
    rejected_illegal: u64,
    rejected_malformed: u64,
    quarantined: u64,
    is_reconciled: bool,
    transpositions_distinct: u64,
    import_duration_secs: f64,
    import_throughput_games_sec: f64,
    import_throughput_mb_sec: f64,

    // Storage measurements
    raw_pgn_bytes: u64,
    raw_bytes_per_game: f64,
    codb_block_compressed_bytes: u64,
    codb_bytes_per_game: f64,
    compression_ratio: f64,

    // Index measurements across depths
    depth_20: IndexDepthStats,
    depth_40: IndexDepthStats,
    depth_60: IndexDepthStats,

    // Latency measurements in microseconds
    query_latency_us: LatencyMetrics,
}

#[derive(Serialize)]
struct IndexDepthStats {
    depth_plies: usize,
    unique_positions: usize,
    unique_position_move_pairs: usize,
    immutable_index_size_bytes: usize,
    sqlite_index_size_bytes: usize,
    index_build_duration_secs: f64,
}

#[derive(Serialize)]
struct LatencyMetrics {
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
        Commands::Ingest {
            input,
            collection,
            max_games,
        } => {
            println!("Ingesting from {}...", input.display());
            let stream = open_pgn_reader(&input)?;
            let col = collection.into();

            let mut pgn_reader = Reader::new(stream);
            let mut visitor = ShakmatyGameVisitor::new(col, 1);
            let mut dedupe = Deduplicator::new();
            let mut stats = IngestionStats::default();

            let masters_policy = MastersOtbPolicy::default();
            let elite_policy = EliteOnlinePolicy::default();

            let start = Instant::now();
            let mut count = 0;

            loop {
                match pgn_reader.read_game(&mut visitor) {
                    Ok(Some(result)) => {
                        stats.total_encountered += 1;
                        match result {
                            Ok(game) => {
                                let passes_policy = match col {
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
                                    match dedupe.evaluate(&game) {
                                        DuplicateVerdict::Unique
                                        | DuplicateVerdict::TranspositionDifferentGame => {
                                            stats.accepted_unique += 1;
                                        }
                                        DuplicateVerdict::ConfirmedDuplicate { .. } => {
                                            stats.confirmed_duplicates += 1;
                                        }
                                        DuplicateVerdict::SupersedesPrevious { .. } => {
                                            stats.superseded_prior += 1;
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
                            Err(_) => {
                                stats.quarantined += 1;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(io_err) => {
                        if io_err.kind() == std::io::ErrorKind::InvalidData {
                            stats.total_encountered += 1;
                            stats.rejected_malformed += 1;
                            if let Ok(has_more) = pgn_reader.has_more() {
                                if !has_more {
                                    break;
                                }
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }

                count += 1;
                if let Some(max) = max_games {
                    if stats.accepted_unique as usize >= max {
                        break;
                    }
                }

                if count % 20000 == 0 {
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

            let elapsed = start.elapsed().as_secs_f64();
            println!("\n=== Ingestion Summary ===");
            println!("Total Encountered:    {}", stats.total_encountered);
            println!("Accepted Unique:      {}", stats.accepted_unique);
            println!("Confirmed Duplicates: {}", stats.confirmed_duplicates);
            println!("Superseded Prior:     {}", stats.superseded_prior);
            println!("Rejected by Policy:   {}", stats.rejected_by_policy);
            println!("Rejected Variant/FEN: {}", stats.rejected_variant);
            println!("Rejected Illegal:     {}", stats.rejected_illegal);
            println!("Rejected Malformed:   {}", stats.rejected_malformed);
            println!("Quarantined:          {}", stats.quarantined);
            println!(
                "Accounting Invariant: Reconciled = {}",
                stats.is_reconciled()
            );
            println!("Duration:             {:.2}s", elapsed);
            println!(
                "Throughput:           {:.1} games/sec",
                stats.total_encountered as f64 / elapsed
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
    }

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
    let mut pgn_reader = Reader::new(stream);
    let mut visitor = ShakmatyGameVisitor::new(collection, 1);
    let mut dedupe = Deduplicator::new();
    let mut stats = IngestionStats::default();

    let masters_policy = MastersOtbPolicy::default();
    let elite_policy = EliteOnlinePolicy::default();

    let mut accepted_games: Vec<ParsedGame> = Vec::new();
    let mut transpositions_distinct = 0;

    let import_start = Instant::now();

    loop {
        match pgn_reader.read_game(&mut visitor) {
            Ok(Some(result)) => {
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
                            match dedupe.evaluate(&game) {
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
                                    if let Some(pos) =
                                        accepted_games.iter().position(|g| g.metadata.id == old_id)
                                    {
                                        accepted_games[pos] = game;
                                    } else {
                                        accepted_games.push(game);
                                    }
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
                    Err(_) => {
                        stats.quarantined += 1;
                    }
                }
            }
            Ok(None) => break,
            Err(io_err) => {
                if io_err.kind() == std::io::ErrorKind::InvalidData {
                    stats.total_encountered += 1;
                    stats.rejected_malformed += 1;
                    if let Ok(has_more) = pgn_reader.has_more() {
                        if !has_more {
                            break;
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }

        if let Some(max) = max_games {
            if stats.accepted_unique as usize >= max {
                break;
            }
        }
    }

    let import_dur = import_start.elapsed().as_secs_f64();
    let n_games = accepted_games.len();
    println!(
        "Imported {} accepted games in {:.2}s ({:.1} games/sec)",
        n_games,
        import_dur,
        n_games as f64 / import_dur
    );
    println!("Accounting Invariant Reconciled: {}", stats.is_reconciled());

    // 2. Storage Benchmark: Compress games in 512-game blocks
    println!("Benchmarking compact game storage...");
    let mut total_raw_pgn_bytes = 0u64;
    for g in &accepted_games {
        let pgn = export_to_pgn(g).unwrap_or_default();
        total_raw_pgn_bytes += pgn.len() as u64;
    }

    let mut codb_compressed_bytes = 0u64;
    let block_size = 512;
    for chunk in accepted_games.chunks(block_size) {
        let mut block = GameBlock::new(collection);
        block.games = chunk.to_vec();
        let comp = block.compress(3).expect("block compression failed");
        codb_compressed_bytes += comp.len() as u64;
    }

    let raw_per_game = total_raw_pgn_bytes as f64 / n_games.max(1) as f64;
    let codb_per_game = codb_compressed_bytes as f64 / n_games.max(1) as f64;
    let comp_ratio = total_raw_pgn_bytes as f64 / codb_compressed_bytes.max(1) as f64;

    println!(
        "Raw PGN size:          {:.2} MB ({:.1} bytes/game)",
        total_raw_pgn_bytes as f64 / 1_000_000.0,
        raw_per_game
    );
    println!(
        "CODB Block-Zstd size:  {:.2} MB ({:.1} bytes/game)",
        codb_compressed_bytes as f64 / 1_000_000.0,
        codb_per_game
    );
    println!("Compression ratio:     {:.2}x", comp_ratio);

    // 3. Index Depth Evaluation: 20, 40, 60 plies
    println!("Benchmarking position indexing on-disk at depths 20, 40, 60 plies...");
    let depth_20 = measure_index_depth(&accepted_games, 20)?;
    let depth_40 = measure_index_depth(&accepted_games, 40)?;
    let depth_60 = measure_index_depth(&accepted_games, 60)?;

    println!(
        "Depth 20: {} unique positions, {} unique pairs | CPI disk: {:.2} MB, SQLite disk: {:.2} MB",
        depth_20.unique_positions,
        depth_20.unique_position_move_pairs,
        depth_20.immutable_index_size_bytes as f64 / 1_000_000.0,
        depth_20.sqlite_index_size_bytes as f64 / 1_000_000.0
    );
    println!(
        "Depth 40: {} unique positions, {} unique pairs | CPI disk: {:.2} MB, SQLite disk: {:.2} MB",
        depth_40.unique_positions,
        depth_40.unique_position_move_pairs,
        depth_40.immutable_index_size_bytes as f64 / 1_000_000.0,
        depth_40.sqlite_index_size_bytes as f64 / 1_000_000.0
    );
    println!(
        "Depth 60: {} unique positions, {} unique pairs | CPI disk: {:.2} MB, SQLite disk: {:.2} MB",
        depth_60.unique_positions,
        depth_60.unique_position_move_pairs,
        depth_60.immutable_index_size_bytes as f64 / 1_000_000.0,
        depth_60.sqlite_index_size_bytes as f64 / 1_000_000.0
    );

    // 4. Query Latencies
    println!("Measuring query latencies across on-disk CPI vs on-disk SQLite vs in-memory...");
    let latencies = measure_query_latencies(&accepted_games)?;

    println!(
        "Query Latency (p50 / p95 / p99): CPI disk: {:.1} / {:.1} / {:.1} µs | SQLite disk: {:.1} / {:.1} / {:.1} µs | CPI RAM: {:.2} / {:.2} / {:.2} µs",
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

    let report = BenchmarkReport {
        run_name: run_name.to_string(),
        collection: collection.to_string(),
        input_file: input_path.display().to_string(),
        input_size_bytes: input_size,
        total_encountered: stats.total_encountered,
        accepted_unique: stats.accepted_unique,
        confirmed_duplicates: stats.confirmed_duplicates,
        superseded_prior: stats.superseded_prior,
        rejected_by_policy: stats.rejected_by_policy,
        rejected_variant: stats.rejected_variant,
        rejected_illegal: stats.rejected_illegal,
        rejected_malformed: stats.rejected_malformed,
        quarantined: stats.quarantined,
        is_reconciled: stats.is_reconciled(),
        transpositions_distinct,
        import_duration_secs: import_dur,
        import_throughput_games_sec: n_games as f64 / import_dur,
        import_throughput_mb_sec: (input_size as f64 / 1_000_000.0) / import_dur,
        raw_pgn_bytes: total_raw_pgn_bytes,
        raw_bytes_per_game: raw_per_game,
        codb_block_compressed_bytes: codb_compressed_bytes,
        codb_bytes_per_game: codb_per_game,
        compression_ratio: comp_ratio,
        depth_20,
        depth_40,
        depth_60,
        query_latency_us: latencies,
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
        let entries = extract_game_positions(g, depth_plies).unwrap_or_default();
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

    // Measure SQLite on-disk size: database file + WAL file if present
    let sqlite_db_bytes = std::fs::metadata(temp_sqlite.path())?.len();
    let wal_path = temp_sqlite.path().with_extension("sqlite-wal");
    let wal_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    let sqlite_total_bytes = (sqlite_db_bytes + wal_bytes) as usize;

    Ok(IndexDepthStats {
        depth_plies,
        unique_positions,
        unique_position_move_pairs,
        immutable_index_size_bytes: cpi_disk_bytes,
        sqlite_index_size_bytes: sqlite_total_bytes,
        index_build_duration_secs: dur,
    })
}

fn measure_query_latencies(games: &[ParsedGame]) -> Result<LatencyMetrics> {
    let mut immutable = ImmutablePositionIndex::new();
    let temp_sqlite = tempfile::Builder::new()
        .prefix("codb_lat_")
        .suffix(".sqlite")
        .tempfile()?;
    let mut sqlite = SqlitePositionIndex::open(temp_sqlite.path())?;

    let mut keys = Vec::new();
    for g in games.iter().take(500) {
        let entries = extract_game_positions(g, 20).unwrap_or_default();
        sqlite.insert_batch(&entries)?;
        for (k, cm, r) in entries {
            keys.push(k);
            immutable.record(k, cm, r);
        }
    }

    let temp_cpi = tempfile::Builder::new()
        .prefix("codb_lat_")
        .suffix(".cpi")
        .tempfile()?;
    immutable.write_to_file(temp_cpi.path(), 256, 3)?;
    let disk_cpi = DiskPositionIndex::open(temp_cpi.path())?;

    let query_keys = if keys.is_empty() {
        vec![PositionKey(0)]
    } else {
        keys.into_iter().take(200).collect()
    };

    let mut imm_times = Vec::with_capacity(query_keys.len());
    for &k in &query_keys {
        let t0 = Instant::now();
        let _ = immutable.query(k);
        imm_times.push(t0.elapsed().as_micros() as f64);
    }

    let mut disk_cpi_times = Vec::with_capacity(query_keys.len());
    for &k in &query_keys {
        let t0 = Instant::now();
        let _ = disk_cpi.query(k)?;
        disk_cpi_times.push(t0.elapsed().as_micros() as f64);
    }

    let mut sql_times = Vec::with_capacity(query_keys.len());
    for &k in &query_keys {
        let t0 = Instant::now();
        let _ = sqlite.query_position(k)?;
        sql_times.push(t0.elapsed().as_micros() as f64);
    }

    imm_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    disk_cpi_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sql_times.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let p50 = |v: &[f64]| v[v.len() * 50 / 100];
    let p95 = |v: &[f64]| v[v.len() * 95 / 100];
    let p99 = |v: &[f64]| v[v.len() * 99 / 100];

    Ok(LatencyMetrics {
        sqlite_disk_p50_us: p50(&sql_times),
        sqlite_disk_p95_us: p95(&sql_times),
        sqlite_disk_p99_us: p99(&sql_times),
        cpi_disk_p50_us: p50(&disk_cpi_times),
        cpi_disk_p95_us: p95(&disk_cpi_times),
        cpi_disk_p99_us: p99(&disk_cpi_times),
        cpi_in_memory_p50_us: p50(&imm_times),
        cpi_in_memory_p95_us: p50(&imm_times),
        cpi_in_memory_p99_us: p50(&imm_times),
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
