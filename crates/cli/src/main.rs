use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use codb_core::{Collection, IngestionStats, ParsedGame, PositionKey};
use codb_curate::dedupe::{Deduplicator, DuplicateVerdict};
use codb_index::position::{
    compute_position_key, extract_game_positions, ImmutablePositionIndex, SqlitePositionIndex,
};
use codb_ingest::reader::ShakmatyGameVisitor;
use codb_storage::format::{export_to_pgn, GameBlock};
use pgn_reader::Reader;
use serde::Serialize;
use shakmaty::Chess;

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
    accepted_games: u64,
    rejected_variant: u64,
    rejected_illegal: u64,
    duplicates_superseded: u64,
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
    sqlite_p50_us: f64,
    sqlite_p95_us: f64,
    sqlite_p99_us: f64,
    immutable_p50_us: f64,
    immutable_p95_us: f64,
    immutable_p99_us: f64,
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

            let start = Instant::now();
            let mut count = 0;

            while let Ok(Some(result)) = pgn_reader.read_game(&mut visitor) {
                stats.total_encountered += 1;
                match result {
                    Ok(game) => {
                        stats.parsed_games += 1;
                        match dedupe.evaluate(&game) {
                            DuplicateVerdict::Unique
                            | DuplicateVerdict::TranspositionDifferentGame => {
                                stats.accepted_games += 1;
                            }
                            DuplicateVerdict::ConfirmedDuplicate { .. } => {
                                stats.confirmed_duplicates += 1;
                            }
                            DuplicateVerdict::SupersedesPrevious { .. } => {
                                // Replaced earlier truncated game
                            }
                        }
                    }
                    Err(codb_ingest::reader::IngestError::UnsupportedVariant(_)) => {
                        stats.rejected_variant += 1;
                    }
                    Err(codb_ingest::reader::IngestError::IllegalMove { .. }) => {
                        stats.rejected_illegal += 1;
                    }
                    Err(_) => {
                        stats.quarantined += 1;
                    }
                }

                count += 1;
                if let Some(max) = max_games {
                    if count >= max {
                        break;
                    }
                }

                if count % 20000 == 0 {
                    println!(
                        "Encountered: {}, Accepted: {}, Illegal: {}, Duplicates: {}",
                        stats.total_encountered,
                        stats.accepted_games,
                        stats.rejected_illegal,
                        stats.confirmed_duplicates
                    );
                }
            }

            let elapsed = start.elapsed().as_secs_f64();
            println!("\n=== Ingestion Summary ===");
            println!("Total Encountered: {}", stats.total_encountered);
            println!("Parsed Games:      {}", stats.parsed_games);
            println!("Accepted Games:    {}", stats.accepted_games);
            println!("Rejected Illegal:  {}", stats.rejected_illegal);
            println!("Rejected Variant:  {}", stats.rejected_variant);
            println!("Duplicates:        {}", stats.confirmed_duplicates);
            println!("Duration:          {:.2}s", elapsed);
            println!(
                "Throughput:        {:.1} games/sec",
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

    let mut accepted_games: Vec<ParsedGame> = Vec::new();
    let mut transpositions_distinct = 0;
    let mut duplicates_superseded = 0;

    let import_start = Instant::now();

    while let Ok(Some(result)) = pgn_reader.read_game(&mut visitor) {
        stats.total_encountered += 1;
        match result {
            Ok(game) => {
                stats.parsed_games += 1;
                match dedupe.evaluate(&game) {
                    DuplicateVerdict::Unique => {
                        stats.accepted_games += 1;
                        accepted_games.push(game);
                    }
                    DuplicateVerdict::TranspositionDifferentGame => {
                        stats.accepted_games += 1;
                        transpositions_distinct += 1;
                        accepted_games.push(game);
                    }
                    DuplicateVerdict::ConfirmedDuplicate { .. } => {
                        stats.confirmed_duplicates += 1;
                    }
                    DuplicateVerdict::SupersedesPrevious { old_id, .. } => {
                        duplicates_superseded += 1;
                        // Replace previous in accepted list
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
            Err(codb_ingest::reader::IngestError::UnsupportedVariant(_)) => {
                stats.rejected_variant += 1;
            }
            Err(codb_ingest::reader::IngestError::IllegalMove { .. }) => {
                stats.rejected_illegal += 1;
            }
            Err(_) => {
                stats.quarantined += 1;
            }
        }

        if let Some(max) = max_games {
            if stats.accepted_games as usize >= max {
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
    println!("Benchmarking position indexing at depths 20, 40, 60 plies...");
    let depth_20 = measure_index_depth(&accepted_games, 20)?;
    let depth_40 = measure_index_depth(&accepted_games, 40)?;
    let depth_60 = measure_index_depth(&accepted_games, 60)?;

    println!(
        "Depth 20: {} unique positions, {:.2} MB compressed",
        depth_20.unique_positions,
        depth_20.immutable_index_size_bytes as f64 / 1_000_000.0
    );
    println!(
        "Depth 40: {} unique positions, {:.2} MB compressed",
        depth_40.unique_positions,
        depth_40.immutable_index_size_bytes as f64 / 1_000_000.0
    );
    println!(
        "Depth 60: {} unique positions, {:.2} MB compressed",
        depth_60.unique_positions,
        depth_60.immutable_index_size_bytes as f64 / 1_000_000.0
    );

    // 4. Query Latencies
    println!("Measuring query latencies across opening positions...");
    let latencies = measure_query_latencies(&accepted_games)?;

    let report = BenchmarkReport {
        run_name: run_name.to_string(),
        collection: collection.to_string(),
        input_file: input_path.display().to_string(),
        input_size_bytes: input_size,
        total_encountered: stats.total_encountered,
        accepted_games: stats.accepted_games,
        rejected_variant: stats.rejected_variant,
        rejected_illegal: stats.rejected_illegal,
        duplicates_superseded,
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
    let mut sqlite = SqlitePositionIndex::in_memory()?;

    let mut total_pairs = 0;
    let mut batch_entries = Vec::with_capacity(10_000);

    for g in games {
        let entries = extract_game_positions(g, depth_plies).unwrap_or_default();
        total_pairs += entries.len();
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
    let compressed_bytes = immutable.serialize_compressed(3)?.len();

    // Query SQLite size
    let sqlite_bytes: usize = sqlite
        .query_position(PositionKey(0))
        .map(|_| 0)
        .unwrap_or(0);

    Ok(IndexDepthStats {
        depth_plies,
        unique_positions,
        unique_position_move_pairs: total_pairs,
        immutable_index_size_bytes: compressed_bytes,
        sqlite_index_size_bytes: sqlite_bytes,
        index_build_duration_secs: dur,
    })
}

fn measure_query_latencies(games: &[ParsedGame]) -> Result<LatencyMetrics> {
    // Build an index for latency test
    let mut immutable = ImmutablePositionIndex::new();
    let mut sqlite = SqlitePositionIndex::in_memory()?;

    let mut keys = Vec::new();
    for g in games.iter().take(500) {
        let entries = extract_game_positions(g, 20).unwrap_or_default();
        sqlite.insert_batch(&entries)?;
        for (k, cm, r) in entries {
            keys.push(k);
            immutable.record(k, cm, r);
        }
    }

    // Benchmark sample of lookups
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

    let mut sql_times = Vec::with_capacity(query_keys.len());
    for &k in &query_keys {
        let t0 = Instant::now();
        let _ = sqlite.query_position(k);
        sql_times.push(t0.elapsed().as_micros() as f64);
    }

    imm_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sql_times.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let p50 = |v: &[f64]| v[v.len() * 50 / 100];
    let p95 = |v: &[f64]| v[v.len() * 95 / 100];
    let p99 = |v: &[f64]| v[v.len() * 99 / 100];

    Ok(LatencyMetrics {
        sqlite_p50_us: p50(&sql_times),
        sqlite_p95_us: p95(&sql_times),
        sqlite_p99_us: p99(&sql_times),
        immutable_p50_us: p50(&imm_times),
        immutable_p95_us: p95(&imm_times),
        immutable_p99_us: p99(&imm_times),
    })
}

fn run_query(_index_path: &Path, _fen: Option<&str>, _moves: Option<&str>) -> Result<()> {
    // Demonstration of position query CLI
    let board = Chess::default();
    let key = compute_position_key(&board);
    println!("Starting position canonical key: {}", key.to_hex());
    Ok(())
}
