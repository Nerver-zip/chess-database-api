use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chess_database_core::{compute_position_key, CompactMove, GameResult, PositionKey};
use chess_database_index::position::{MoveStats, PositionStats};
use chess_database_storage::DatasetReader;
use serde::Serialize;
use shakmaty::fen::Fen;
use shakmaty::san::San;
use shakmaty::{CastlingMode, Chess, Position};

const TARGETS: &[(&str, &str)] = &[
    ("Open Game", "e4,e5"),
    ("Sicilian Defense", "e4,c5"),
    ("French Defense", "e4,e6,d4,d5"),
    ("Caro-Kann Defense", "e4,c6,d4,d5"),
    ("Queen's Gambit Declined", "d4,d5,c4,e6"),
    ("King's Indian Defense", "d4,Nf6,c4,g6"),
    ("English Opening", "c4,e5"),
    ("Ruy Lopez", "e4,e5,Nf3,Nc6,Bb5"),
    ("Italian Game", "e4,e5,Nf3,Nc6,Bc4"),
    ("King's Gambit Accepted", "e4,e5,f4,exf4,Nf3"),
    ("Budapest Gambit", "d4,Nf6,c4,e5,dxe5,Ng4"),
    ("Scandinavian Defense", "e4,d5,exd5,Qxd5,Nc3,Qa5"),
    ("Alekhine Defense", "e4,Nf6,e5,Nd5,d4,d6"),
    ("Dutch Defense", "d4,f5,c4,Nf6,g3"),
    ("Nimzo-Indian Defense", "d4,Nf6,c4,e6,Nc3,Bb4"),
    ("Benko Gambit", "d4,Nf6,c4,c5,d5,b5"),
    ("Trompowsky Attack", "d4,Nf6,Bg5,e6,e3"),
    ("Ruy Lopez transposition", "e4,e5,Bb5,Nc6,Nf3"),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
struct OutcomeCounts {
    white_wins: u64,
    draws: u64,
    black_wins: u64,
}

impl OutcomeCounts {
    fn add_result(&mut self, result: GameResult) {
        match result {
            GameResult::WhiteWins => self.white_wins += 1,
            GameResult::Draw => self.draws += 1,
            GameResult::BlackWins => self.black_wins += 1,
            GameResult::Star => {}
        }
    }

    fn total(self) -> u64 {
        self.white_wins + self.draws + self.black_wins
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct OracleMove {
    uci: String,
    white_wins: u64,
    draws: u64,
    black_wins: u64,
    total_games: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
struct OraclePosition {
    total_games: u64,
    white_wins: u64,
    draws: u64,
    black_wins: u64,
    moves: Vec<OracleMove>,
}

#[derive(Serialize)]
struct TargetReport {
    opening: &'static str,
    moves_san: &'static str,
    position_key: String,
    oracle: OraclePosition,
    cpi: Option<PositionStats>,
    exact_match: bool,
}

#[derive(Serialize)]
struct OracleReport {
    schema_version: u32,
    method: String,
    dataset_build_id: String,
    dataset_games: u64,
    games_iterated: u64,
    indexed_depth_plies: usize,
    known_result_games_seen: u64,
    unknown_result_games_seen: u64,
    target_positions: usize,
    target_lines: Vec<TargetReport>,
    limitations: Vec<String>,
}

type MoveCounts = BTreeMap<CompactMove, OutcomeCounts>;

fn parse_san_line(moves: &str) -> Result<(Chess, PositionKey)> {
    let mut board = Chess::default();
    for token in moves.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let san = San::from_ascii(token.as_bytes())
            .with_context(|| format!("parsing target SAN move {token:?} in {moves:?}"))?;
        let played = san
            .to_move(&board)
            .map_err(|error| anyhow!("illegal target SAN move {token:?}: {error:?}"))?;
        board.play_unchecked(played);
    }
    let key = compute_position_key(&board);
    Ok((board, key))
}

fn initial_board(game: &chess_database_core::ParsedGame) -> Result<Chess> {
    let Some(fen) = game.fen_start.as_deref() else {
        return Ok(Chess::default());
    };
    let parsed: Fen = fen
        .parse()
        .context("parsing canonical game's starting FEN")?;
    parsed
        .into_position(CastlingMode::Standard)
        .map_err(|error| anyhow!("invalid canonical starting FEN: {error:?}"))
}

fn oracle_position(moves: &MoveCounts) -> OraclePosition {
    let mut result = OraclePosition::default();
    for (compact_move, counts) in moves {
        result.white_wins += counts.white_wins;
        result.draws += counts.draws;
        result.black_wins += counts.black_wins;
        result.moves.push(OracleMove {
            uci: compact_move.to_uci(),
            white_wins: counts.white_wins,
            draws: counts.draws,
            black_wins: counts.black_wins,
            total_games: counts.total(),
        });
    }
    result.total_games = result.white_wins + result.draws + result.black_wins;
    result
}

fn cpi_move_map(stats: &PositionStats) -> BTreeMap<CompactMove, OutcomeCounts> {
    stats
        .moves
        .iter()
        .map(|item: &MoveStats| {
            (
                item.compact_move,
                OutcomeCounts {
                    white_wins: item.white_wins as u64,
                    draws: item.draws as u64,
                    black_wins: item.black_wins as u64,
                },
            )
        })
        .collect()
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 2 {
        return Err(anyhow!(
            "usage: audit_cpi_oracle <dataset-root> <report.json>"
        ));
    }
    let dataset_root = PathBuf::from(&args[0]);
    let report_path = PathBuf::from(&args[1]);
    let reader = DatasetReader::open(&dataset_root)
        .with_context(|| format!("opening dataset {}", dataset_root.display()))?;
    let index_meta = reader
        .manifest()
        .position_index
        .as_ref()
        .ok_or_else(|| anyhow!("dataset has no published CPI position index"))?;
    let max_plies = index_meta.indexed_depth_plies;

    let mut targets_by_key: HashMap<PositionKey, Vec<usize>> = HashMap::new();
    let mut target_keys = Vec::with_capacity(TARGETS.len());
    for (index, (_, moves)) in TARGETS.iter().enumerate() {
        let (_, key) = parse_san_line(moves)?;
        target_keys.push(key);
        targets_by_key.entry(key).or_default().push(index);
    }
    let mut oracle_counts: Vec<MoveCounts> = (0..TARGETS.len()).map(|_| BTreeMap::new()).collect();
    let mut games_iterated = 0u64;
    let mut known_result_games_seen = 0u64;
    let mut unknown_result_games_seen = 0u64;

    for item in reader.iter_games()? {
        let game = item?;
        games_iterated += 1;
        if game.metadata.result == GameResult::Star {
            unknown_result_games_seen += 1;
        } else {
            known_result_games_seen += 1;
        }

        let limit = game.moves.len().min(max_plies);
        let mut board = initial_board(&game)?;
        let mut seen_positions = HashSet::with_capacity(limit.min(64));
        for (ply, compact_move) in game.moves[..limit].iter().copied().enumerate() {
            let key = compute_position_key(&board);
            let first_visit = seen_positions.insert(key);
            let played = compact_move.to_shakmaty_move(&board).ok_or_else(|| {
                anyhow!(
                    "illegal canonical move at ply {} ({}) in stored game {}",
                    ply + 1,
                    compact_move.to_uci(),
                    game.metadata.id
                )
            })?;

            if first_visit && game.metadata.result != GameResult::Star {
                if let Some(target_indices) = targets_by_key.get(&key) {
                    for &target_index in target_indices {
                        oracle_counts[target_index]
                            .entry(compact_move)
                            .or_default()
                            .add_result(game.metadata.result);
                    }
                }
            }
            board.play_unchecked(played);
        }
    }

    if games_iterated != reader.total_games() {
        return Err(anyhow!(
            "streamed {} games, manifest reports {}",
            games_iterated,
            reader.total_games()
        ));
    }

    let mut target_lines = Vec::with_capacity(TARGETS.len());
    let mut mismatch_count = 0usize;
    for (index, (opening, moves_san)) in TARGETS.iter().enumerate() {
        let key = target_keys[index];
        let cpi = reader.query_position(key)?;
        let oracle = oracle_position(&oracle_counts[index]);
        let cpi_map = cpi.as_ref().map(cpi_move_map).unwrap_or_default();
        let exact_match = oracle_counts[index] == cpi_map;
        if !exact_match {
            mismatch_count += 1;
        }
        target_lines.push(TargetReport {
            opening,
            moves_san,
            position_key: key.to_hex(),
            oracle,
            cpi,
            exact_match,
        });
    }

    let report = OracleReport {
        schema_version: 1,
        method: "Stream every canonical CGB3 game from DatasetReader, replay up to the published CPI depth with Shakmaty, and independently aggregate first-visit position/next-move WDL counts for fixed opening positions. Compare the move-keyed aggregates against direct CPI v2 lookups. The scan validates every replayed move and excludes unknown-result games from WDL counts.".into(),
        dataset_build_id: reader.manifest().dataset_build_id.clone(),
        dataset_games: reader.total_games(),
        games_iterated,
        indexed_depth_plies: max_plies,
        known_result_games_seen,
        unknown_result_games_seen,
        target_positions: target_keys.iter().copied().collect::<HashSet<_>>().len(),
        target_lines,
        limitations: vec![
            "The oracle starts from canonical CGB3 records, not the original source PGN, so it does not independently verify source-to-canonical mapping or upstream metadata preservation.".into(),
            "It shares the repository's compute_position_key function with CPI extraction; move replay and aggregation are independent, but the position-key encoding itself is not independently reimplemented.".into(),
            "Only the listed positions are independently aggregated; this is not a complete re-indexing of every position.".into(),
        ],
    };

    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = File::create(&report_path)?;
    serde_json::to_writer_pretty(&mut output, &report)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    println!(
        "audited {} games and {} target lines; {} mismatches; wrote {}",
        games_iterated,
        TARGETS.len(),
        mismatch_count,
        report_path.display()
    );
    if mismatch_count != 0 {
        return Err(anyhow!(
            "CPI oracle found {mismatch_count} mismatching target lines"
        ));
    }
    Ok(())
}

fn main() -> Result<()> {
    run()
}
