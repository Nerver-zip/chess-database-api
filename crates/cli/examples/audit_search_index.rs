//! Reconcile search postings and CPI on deterministic production-corpus probes.

use anyhow::{ensure, Context, Result};
use clap::Parser;
use opening_explorer_core::{compute_position_key, CompactMove, ParsedGame, PositionKey};
use opening_explorer_index::search::{catalog::SearchCatalog, postings::SearchPostingsReader};
use opening_explorer_storage::{
    search::{CATALOG_FILENAME, POSTINGS_FILENAME, SEARCH_DIRECTORY},
    DatasetReader,
};
use rusqlite::Connection;
use shakmaty::{fen::Fen, san::San, CastlingMode, Chess, Position};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dataset: PathBuf,
    #[arg(long, default_value_t = 128)]
    sampled_games: u64,
}

fn legacy_replay(
    game: &ParsedGame,
    limit: usize,
) -> Result<Vec<(PositionKey, Option<CompactMove>)>> {
    let mut board = match &game.fen_start {
        Some(fen) => fen.parse::<Fen>()?.into_position(CastlingMode::Standard)?,
        None => Chess::default(),
    };
    let mut reached = Vec::new();
    for ply in 0..=limit.min(game.moves.len()) {
        let key = compute_position_key(&board);
        let played = game.moves.get(ply).copied();
        reached.push((key, played));
        if let Some(played) = played {
            let next = played
                .to_shakmaty_move(&board)
                .context("Illegal stored move")?;
            board.play_unchecked(next);
        }
    }
    Ok(reached)
}

fn stats_from_postings(
    index: &SearchPostingsReader,
    catalog: &Connection,
    key: PositionKey,
    depth: usize,
) -> Result<BTreeMap<String, [u64; 3]>> {
    let mut moves = BTreeMap::<String, [u64; 3]>::new();
    let Some(entries) = index.get(key)? else {
        return Ok(moves);
    };
    let count_probe = index.get(key)?.expect("position exists").take(4097).count();
    let mut scan = catalog.prepare("SELECT ordinal, result FROM games ORDER BY ordinal")?;
    let mut rows = scan.query([])?;
    let mut point = catalog.prepare("SELECT result FROM games WHERE ordinal = ?")?;
    let mut last_ordinal = None;
    for posting in entries {
        let posting = posting?;
        if last_ordinal == Some(posting.ordinal) {
            continue;
        }
        last_ordinal = Some(posting.ordinal);
        let Some(played) = posting.played.filter(|_| usize::from(posting.ply) < depth) else {
            continue;
        };
        let result = if count_probe <= 4096 {
            point.query_row([posting.ordinal], |row| row.get::<_, String>(0))?
        } else {
            loop {
                let row = rows.next()?.context("Posting is outside catalog")?;
                let candidate: u32 = row.get(0)?;
                ensure!(candidate <= posting.ordinal, "Canonical ordinal missing");
                if candidate == posting.ordinal {
                    break row.get::<_, String>(1)?;
                }
            }
        };
        let outcome = match result.as_str() {
            "1-0" => 0,
            "1/2-1/2" => 1,
            "0-1" => 2,
            "*" => continue,
            _ => anyhow::bail!("Unknown game result in catalog"),
        };
        moves.entry(played.to_uci()).or_default()[outcome] += 1;
    }
    Ok(moves)
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(args.sampled_games > 0, "At least one sample is required");
    let start = Instant::now();
    let dataset = DatasetReader::open(&args.dataset)?;
    let search = dataset
        .manifest()
        .search_index
        .as_ref()
        .context("No search index")?;
    let cpi = dataset.open_position_index()?.context("No CPI index")?;
    let depth = dataset
        .manifest()
        .position_index
        .as_ref()
        .unwrap()
        .indexed_depth_plies;
    ensure!(search
        .coverage
        .max_ply()
        .is_none_or(|p| usize::from(p) >= depth));
    let search_root = dataset.snapshot_dir().join(SEARCH_DIRECTORY);
    let index = SearchPostingsReader::open(search_root.join(POSTINGS_FILENAME))?;
    let catalog = SearchCatalog::open(&search_root.join(CATALOG_FILENAME))?;

    let mut keys = BTreeSet::new();
    for line in [
        "",
        "e4",
        "d4",
        "e4 c5",
        "e4 e5 Nf3",
        "d4 Nf6 c4 e6",
        "Nf3 Nf6 g3 g6",
    ] {
        let mut board = Chess::default();
        for token in line.split_whitespace() {
            let played = token.parse::<San>()?.to_move(&board)?;
            board.play_unchecked(played);
        }
        keys.insert(compute_position_key(&board));
    }
    let common_probes = keys.len();
    let interval = dataset
        .total_games()
        .checked_div(args.sampled_games)
        .unwrap_or(1)
        .max(1);
    let mut checked = 0u64;
    let mut sampled = 0u64;
    for (ordinal, game) in dataset.iter_games()?.enumerate() {
        let game = game?;
        if !(ordinal as u64).is_multiple_of(interval) {
            continue;
        }
        if sampled == args.sampled_games {
            break;
        }
        let ordinal = u32::try_from(ordinal)?;
        let limit = search
            .coverage
            .max_ply()
            .map_or(game.moves.len(), usize::from);
        let reached = legacy_replay(&game, limit)?;
        for (ply, &(key, played)) in reached.iter().enumerate() {
            let entries = index
                .seek(key, ordinal)?
                .context("Missing canonical position")?;
            let mut found = false;
            for posting in entries {
                let posting = posting?;
                if posting.ordinal != ordinal || usize::from(posting.ply) > ply {
                    break;
                }
                if usize::from(posting.ply) == ply {
                    ensure!(
                        posting.played == played,
                        "Wrong continuation at ordinal {ordinal}, ply {ply}"
                    );
                    found = true;
                    break;
                }
            }
            ensure!(found, "Missing occurrence at ordinal {ordinal}, ply {ply}");
            checked += 1;
            if [2, depth / 2, depth.saturating_sub(1), depth].contains(&ply) {
                keys.insert(key);
            }
        }
        sampled += 1;
    }

    let mut compared = 0usize;
    let mut empty = 0usize;
    for key in keys {
        let observed = stats_from_postings(&index, catalog.connection(), key, depth)?;
        let expected: BTreeMap<_, _> = cpi
            .query(key)?
            .into_iter()
            .flat_map(|stats| stats.moves)
            .map(|variant| {
                (
                    variant.uci,
                    [
                        u64::from(variant.white_wins),
                        u64::from(variant.draws),
                        u64::from(variant.black_wins),
                    ],
                )
            })
            .collect();
        ensure!(observed == expected, "CPI mismatch for {}", key.to_hex());
        if expected.is_empty() {
            empty += 1;
        }
        compared += 1;
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "collection": dataset.collection(),
            "snapshot": dataset.manifest().dataset_build_id,
            "source_snapshot": search.source_snapshot,
            "game_count": dataset.total_games(),
            "cpi_depth": depth,
            "coverage": search.coverage,
            "common_probes": common_probes,
            "sampled_games": sampled,
            "verified_occurrences": checked,
            "cpi_positions_matched": compared,
            "empty_cpi_probes": empty,
            "seconds": start.elapsed().as_secs_f64(),
        }))?
    );
    Ok(())
}
