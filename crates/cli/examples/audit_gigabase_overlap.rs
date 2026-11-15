use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use codb_core::ParsedGame;
use codb_curate::dedupe::normalize_player_name;
use codb_storage::DatasetReader;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct AttributionLine {
    game_id: u64,
    source_attributions: Vec<Attribution>,
}

#[derive(Debug, Deserialize)]
struct Attribution {
    source_tag: Option<String>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct PlayerDateMoves {
    white: String,
    black: String,
    date: String,
    moves: Vec<u16>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct PlayerYearMoves {
    white: String,
    black: String,
    year: String,
    moves: Vec<u16>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct FullMetadataMoves {
    white: String,
    black: String,
    date: String,
    event: String,
    round: String,
    moves: Vec<u16>,
}

#[derive(serde::Serialize)]
struct OverlapReport {
    schema_version: u32,
    method: String,
    previous_release: ReleaseSummary,
    gigabase_evaluation: ReleaseSummary,
    candidate_source: String,
    exact_stable_game_id_overlap: u64,
    lichess_api_games_with_identical_full_move_sequence: u64,
    lichess_api_games_with_same_players_and_moves: u64,
    lichess_api_games_with_same_players_date_and_moves: u64,
    lichess_api_games_with_same_players_year_and_moves: u64,
    lichess_api_games_with_same_players_date_event_round_and_moves: u64,
    same_players_date_moves_with_event_or_round_drift: u64,
    deduplication_decision: String,
    limitations: Vec<String>,
}

#[derive(serde::Serialize)]
struct ReleaseSummary {
    dataset_build_id: String,
    total_games: u64,
}

fn metadata_key(game: &ParsedGame) -> (String, String, String, String, String) {
    let white = normalize_player_name(&game.metadata.white);
    let black = normalize_player_name(&game.metadata.black);
    let date = game
        .metadata
        .date
        .as_deref()
        .unwrap_or_default()
        .replace('-', ".");
    let event = normalize_text(game.metadata.event.as_deref().unwrap_or_default());
    let round = normalize_text(game.metadata.round.as_deref().unwrap_or_default());
    (white, black, date, event, round)
}

fn normalize_text(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}

fn moves_key(game: &ParsedGame) -> Vec<u16> {
    game.moves
        .iter()
        .map(|compact_move| compact_move.0)
        .collect()
}

fn add_game(
    game: &ParsedGame,
    ids: &mut HashSet<u64>,
    moves_only: &mut HashMap<Vec<u16>, u64>,
    players_moves: &mut HashMap<(String, String, Vec<u16>), u64>,
    players_date_moves: &mut HashMap<PlayerDateMoves, u64>,
    players_year_moves: &mut HashMap<PlayerYearMoves, u64>,
    full_metadata_moves: &mut HashMap<FullMetadataMoves, u64>,
) {
    let id = game.metadata.id;
    ids.insert(id);
    let (white, black, date, event, round) = metadata_key(game);
    let year = date.get(0..4).unwrap_or_default().to_owned();
    let moves = moves_key(game);
    *moves_only.entry(moves.clone()).or_default() += 1;
    *players_moves
        .entry((white.clone(), black.clone(), moves.clone()))
        .or_default() += 1;
    *players_date_moves
        .entry(PlayerDateMoves {
            white: white.clone(),
            black: black.clone(),
            date: date.clone(),
            moves: moves.clone(),
        })
        .or_default() += 1;
    *players_year_moves
        .entry(PlayerYearMoves {
            white: white.clone(),
            black: black.clone(),
            year,
            moves: moves.clone(),
        })
        .or_default() += 1;
    *full_metadata_moves
        .entry(FullMetadataMoves {
            white,
            black,
            date,
            event,
            round,
            moves,
        })
        .or_default() += 1;
}

fn count_matching_candidate_games<K: std::hash::Hash + Eq>(
    reader: &DatasetReader,
    source_ids: &HashSet<u64>,
    mut make_key: impl FnMut(&ParsedGame) -> K,
    old: &HashMap<K, u64>,
) -> Result<u64> {
    let mut count = 0u64;
    for item in reader.iter_games()? {
        let game = item?;
        if source_ids.contains(&game.metadata.id) && old.contains_key(&make_key(&game)) {
            count += 1;
        }
    }
    Ok(count)
}

fn summary(reader: &DatasetReader) -> ReleaseSummary {
    ReleaseSummary {
        dataset_build_id: reader.manifest().dataset_build_id.clone(),
        total_games: reader.total_games(),
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 3 {
        return Err(anyhow!(
            "usage: audit_gigabase_overlap <previous-masters-root> <gigabase-root> <report.json>"
        ));
    }
    let old_root = PathBuf::from(&args[0]);
    let new_root = PathBuf::from(&args[1]);
    let report_path = PathBuf::from(&args[2]);

    let old_reader = DatasetReader::open(&old_root)
        .with_context(|| format!("opening previous Masters release {}", old_root.display()))?;
    let new_reader = DatasetReader::open(&new_root)
        .with_context(|| format!("opening Gigabase evaluation {}", new_root.display()))?;
    let new_snapshot = new_reader.snapshot_dir();
    let attribution_path = new_snapshot.join(
        new_reader
            .manifest()
            .source_attribution
            .as_ref()
            .ok_or_else(|| anyhow!("Gigabase snapshot is missing source attribution"))?
            .filename
            .as_str(),
    );
    let mut lichess_api_ids = HashSet::new();
    for (line_number, line) in BufReader::new(File::open(&attribution_path)?)
        .lines()
        .enumerate()
    {
        let line = line?;
        let attribution: AttributionLine = serde_json::from_str(&line)
            .with_context(|| format!("parsing source attribution line {}", line_number + 1))?;
        if attribution
            .source_attributions
            .iter()
            .any(|record| record.source_tag.as_deref() == Some("LichessBroadcastAPI"))
        {
            lichess_api_ids.insert(attribution.game_id);
        }
    }

    let mut old_ids = HashSet::new();
    let mut old_moves = HashMap::new();
    let mut old_players_moves = HashMap::new();
    let mut old_players_date_moves = HashMap::new();
    let mut old_players_year_moves = HashMap::new();
    let mut old_full_metadata_moves = HashMap::new();
    for item in old_reader.iter_games()? {
        add_game(
            &item?,
            &mut old_ids,
            &mut old_moves,
            &mut old_players_moves,
            &mut old_players_date_moves,
            &mut old_players_year_moves,
            &mut old_full_metadata_moves,
        );
    }

    let mut id_overlap = 0u64;
    for item in new_reader.iter_games()? {
        let game = item?;
        if lichess_api_ids.contains(&game.metadata.id) && old_ids.contains(&game.metadata.id) {
            id_overlap += 1;
        }
    }
    let move_only =
        count_matching_candidate_games(&new_reader, &lichess_api_ids, moves_key, &old_moves)?;
    let players_moves = count_matching_candidate_games(
        &new_reader,
        &lichess_api_ids,
        |game| {
            let (white, black, _, _, _) = metadata_key(game);
            (white, black, moves_key(game))
        },
        &old_players_moves,
    )?;
    let players_date_moves = count_matching_candidate_games(
        &new_reader,
        &lichess_api_ids,
        |game| {
            let (white, black, date, _, _) = metadata_key(game);
            PlayerDateMoves {
                white,
                black,
                date,
                moves: moves_key(game),
            }
        },
        &old_players_date_moves,
    )?;
    let players_year_moves = count_matching_candidate_games(
        &new_reader,
        &lichess_api_ids,
        |game| {
            let (white, black, date, _, _) = metadata_key(game);
            PlayerYearMoves {
                white,
                black,
                year: date.get(0..4).unwrap_or_default().to_owned(),
                moves: moves_key(game),
            }
        },
        &old_players_year_moves,
    )?;
    let full_metadata_moves = count_matching_candidate_games(
        &new_reader,
        &lichess_api_ids,
        |game| {
            let (white, black, date, event, round) = metadata_key(game);
            FullMetadataMoves {
                white,
                black,
                date,
                event,
                round,
                moves: moves_key(game),
            }
        },
        &old_full_metadata_moves,
    )?;

    let event_or_round_drift = players_date_moves.saturating_sub(full_metadata_moves);
    let report = OverlapReport {
        schema_version: 1,
        method: "Stream all canonical games through DatasetReader; compare the previous Masters snapshot with new games whose sidecar includes Source=LichessBroadcastAPI. Game sequences use exact CompactMove vectors. Player names use codb-curate's existing normalization. Date comparison normalizes separators; year comparison is a broad candidate signal. Event and round remove non-alphanumeric ASCII characters and lowercase. No candidate is automatically deduplicated from this audit.".into(),
        previous_release: summary(&old_reader),
        gigabase_evaluation: summary(&new_reader),
        candidate_source: "LichessBroadcastAPI".into(),
        exact_stable_game_id_overlap: id_overlap,
        lichess_api_games_with_identical_full_move_sequence: move_only,
        lichess_api_games_with_same_players_and_moves: players_moves,
        lichess_api_games_with_same_players_date_and_moves: players_date_moves,
        lichess_api_games_with_same_players_year_and_moves: players_year_moves,
        lichess_api_games_with_same_players_date_event_round_and_moves: full_metadata_moves,
        same_players_date_moves_with_event_or_round_drift: event_or_round_drift,
        deduplication_decision: "No cross-release deletion or merge was performed. Exact stable-ID and full metadata/move matches are direct overlap signals; move-only and relaxed metadata matches require manual review because equal moves alone do not prove the games are the same event record.".into(),
        limitations: vec![
            "The source tag is publisher-supplied provenance; the audit does not independently authenticate Lichess origin.".into(),
            "Player normalization intentionally follows existing deduplication behavior and may not catch every spelling or identity mismatch.".into(),
            "No raw PGN, player names, event names, or game IDs are written to this aggregate report.".into(),
        ],
    };
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = File::create(&report_path)?;
    serde_json::to_writer_pretty(&mut output, &report)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    println!("wrote {}", report_path.display());
    Ok(())
}

fn main() -> Result<()> {
    run()
}
