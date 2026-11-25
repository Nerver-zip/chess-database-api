//! Independently count selected Masters filtered Explorer probes from canonical games.

use anyhow::{ensure, Context, Result};
use chess_database_core::{compute_position_key, Collection, GameResult, PositionKey};
use chess_database_index::search::metadata::parse_pgn_date_bounds;
use chess_database_storage::DatasetReader;
use clap::Parser;
use serde_json::json;
use shakmaty::{fen::Fen, san::San, CastlingMode, Chess, Position};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dataset: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let reader = DatasetReader::open(&args.dataset)?;
    let manifest = reader.manifest();
    ensure!(
        manifest.collection == Collection::MastersOtb,
        "Expected Masters OTB"
    );
    let depth = manifest
        .position_index
        .as_ref()
        .context("Expected CPI")?
        .indexed_depth_plies;

    let start_key = compute_position_key(&Chess::default());
    let mut sicilian = Chess::default();
    for san in ["e4", "c5"] {
        let played = san.parse::<San>()?.to_move(&sicilian)?;
        sicilian.play_unchecked(played);
    }
    let sicilian_key = compute_position_key(&sicilian);
    let probes = [
        ("initial_rating", start_key),
        ("sicilian_rating", sicilian_key),
        ("sicilian_year", sicilian_key),
    ];
    let mut counts: [BTreeMap<String, [u64; 3]>; 3] = std::array::from_fn(|_| BTreeMap::new());
    let mut checked = 0;
    for game in reader.iter_games()? {
        let game = game?;
        checked += 1;
        let result = match game.metadata.result {
            GameResult::WhiteWins => 0,
            GameResult::Draw => 1,
            GameResult::BlackWins => 2,
            GameResult::Star => continue,
        };
        let min_rating = game
            .metadata
            .white_elo
            .zip(game.metadata.black_elo)
            .map(|(white, black)| white.min(black))
            .unwrap_or(0);
        let since_2000 = game
            .metadata
            .date
            .as_deref()
            .and_then(parse_pgn_date_bounds)
            .is_some_and(|date| date.lower_yyyymmdd >= 20000101);
        let mut pending = [min_rating >= 2200, min_rating >= 2400, since_2000];
        if !pending.iter().any(|matched| *matched) {
            continue;
        }
        let mut board = match &game.fen_start {
            Some(fen) => fen.parse::<Fen>()?.into_position(CastlingMode::Standard)?,
            None => Chess::default(),
        };
        for played in game.moves.iter().take(depth) {
            let key = compute_position_key(&board);
            for (i, (_, probe_key)) in probes.iter().enumerate() {
                if pending[i] && key == *probe_key {
                    counts[i].entry(played.to_uci()).or_default()[result] += 1;
                    pending[i] = false;
                }
            }
            if !pending.iter().any(|matched| *matched) {
                break;
            }
            board.play_unchecked(
                played
                    .to_shakmaty_move(&board)
                    .context("Illegal stored move")?,
            );
        }
    }
    ensure!(checked == manifest.total_games, "Game count mismatch");
    let counts: BTreeMap<_, _> = probes
        .iter()
        .map(|(name, _): &(&str, PositionKey)| name)
        .zip(counts)
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "dataset_version": manifest.dataset_build_id,
            "games_replayed": checked,
            "cpi_depth_plies": depth,
            "counts": counts,
        }))?
    );
    Ok(())
}
