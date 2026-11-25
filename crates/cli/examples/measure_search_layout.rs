//! Measure candidate position-posting layouts on a contiguous sample of canonical games.
use anyhow::{ensure, Result};
use chess_database_index::search::visit_position_occurrences;
use chess_database_storage::DatasetReader;
use clap::Parser;
use rusqlite::{params, Connection};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dataset: PathBuf,
    #[arg(long, default_value_t = 25_000)]
    games: u32,
    #[arg(long, default_value_t = 0)]
    skip: usize,
    #[arg(long)]
    depth: Option<u16>,
    #[arg(long)]
    sqlite: bool,
    #[arg(long)]
    temp_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct Occurrence {
    key: u128,
    game: u32,
    ply: u16,
    played: u16,
}

fn write_varint(mut value: u32, writer: &mut impl Write) -> io::Result<usize> {
    let mut bytes = 1;
    while value >= 128 {
        writer.write_all(&[value as u8 | 0x80])?;
        bytes += 1;
        value >>= 7;
    }
    writer.write_all(&[value as u8])?;
    Ok(bytes)
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(args.games > 0, "sample must contain at least one game");
    let reader = DatasetReader::open(&args.dataset)?;
    let mut occurrences = Vec::new();
    let replay_start = Instant::now();
    let mut games = 0;
    for game in reader
        .iter_games()?
        .skip(args.skip)
        .take(args.games as usize)
    {
        visit_position_occurrences(&game?, args.depth, |occurrence| {
            occurrences.push(Occurrence {
                key: occurrence.key.0,
                game: games,
                ply: occurrence.ply,
                played: occurrence.played.map_or(u16::MAX, |cm| cm.0),
            });
            Ok(())
        })?;
        games += 1;
    }
    ensure!(games > 0, "no games found at sample offset");
    let replay_seconds = replay_start.elapsed().as_secs_f64();
    let sort_start = Instant::now();
    occurrences.sort_unstable();
    let sort_seconds = sort_start.elapsed().as_secs_f64();

    let encode_start = Instant::now();
    // Count actual compressed output with a writer that does not allocate or persist the stream.
    #[derive(Default)]
    struct Count(u64);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut grouped = zstd::stream::write::Encoder::new(Count::default(), 3)?;
    let mut fixed_counted = zstd::stream::write::Encoder::new(Count::default(), 3)?;
    let mut directory_bytes = 0;
    let mut posting_bytes = 0;
    let mut previous_key = None;
    let mut previous_game = 0;
    for &Occurrence {
        key,
        game,
        ply,
        played,
    } in &occurrences
    {
        if previous_key != Some(key) {
            directory_bytes += 24; // 128-bit key plus 64-bit postings offset.
            previous_key = Some(key);
            previous_game = 0;
        }
        let mut bytes = [0; 24];
        bytes[..16].copy_from_slice(&key.to_be_bytes());
        bytes[16..20].copy_from_slice(&game.to_be_bytes());
        bytes[20..22].copy_from_slice(&ply.to_be_bytes());
        bytes[22..].copy_from_slice(&played.to_be_bytes());
        fixed_counted.write_all(&bytes)?;
        posting_bytes += write_varint(game - previous_game, &mut grouped)?;
        posting_bytes += write_varint(ply.into(), &mut grouped)?;
        grouped.write_all(&played.to_le_bytes())?;
        posting_bytes += 2;
        previous_game = game;
    }
    let fixed_zstd_bytes = fixed_counted.finish()?.0;
    let postings_zstd_bytes = grouped.finish()?.0;
    let encode_seconds = encode_start.elapsed().as_secs_f64();

    let mut sqlite = None;
    if args.sqlite {
        let named = match args.temp_dir {
            Some(path) => tempfile::NamedTempFile::new_in(path),
            None => tempfile::NamedTempFile::new(),
        }?;
        let mut conn = Connection::open(named.path())?;
        let start = Instant::now();
        conn.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-16384;
             CREATE TABLE posting(key BLOB, game INTEGER, ply INTEGER, played INTEGER,
                 PRIMARY KEY(key,game,ply)) WITHOUT ROWID;",
        )?;
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare("INSERT INTO posting VALUES (?1,?2,?3,?4)")?;
            for record in &occurrences {
                insert.execute(params![
                    &record.key.to_be_bytes(),
                    record.game,
                    record.ply,
                    record.played,
                ])?;
            }
        }
        tx.commit()?;
        sqlite = Some(serde_json::json!({
            "file_bytes": named.as_file().metadata()?.len(),
            "write_seconds_after_sort": start.elapsed().as_secs_f64(),
        }));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "collection": reader.collection(),
            "dataset_build_id": reader.manifest().dataset_build_id,
            "games": games, "sample_offset": args.skip, "depth": args.depth,
            "occurrences": occurrences.len(), "unique_positions": directory_bytes / 24,
            "fixed_bytes": occurrences.len() * 24,
            "fixed_unseekable_zstd_bytes": fixed_zstd_bytes,
            "grouped_directory_bytes": directory_bytes,
            "grouped_posting_bytes": posting_bytes,
            "grouped_postings_unseekable_zstd_bytes": postings_zstd_bytes,
            "replay_seconds": replay_seconds, "sort_seconds": sort_seconds,
            "encode_seconds": encode_seconds, "sqlite": sqlite,
        }))?
    );
    Ok(())
}
