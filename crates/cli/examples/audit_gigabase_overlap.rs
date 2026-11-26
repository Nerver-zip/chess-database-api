use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chess_database_core::{GameResult, ParsedGame};
use chess_database_curate::dedupe::normalize_player_name;
use chess_database_storage::DatasetReader;
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

#[derive(Default, serde::Serialize)]
struct FieldAgreement {
    equal: u64,
    different: u64,
    missing_in_previous: u64,
    missing_in_candidate: u64,
    missing_in_both: u64,
}

impl FieldAgreement {
    fn observe<T: PartialEq>(&mut self, previous: Option<&T>, candidate: Option<&T>) {
        match (previous, candidate) {
            (Some(previous), Some(candidate)) if previous == candidate => self.equal += 1,
            (Some(_), Some(_)) => self.different += 1,
            (None, Some(_)) => self.missing_in_previous += 1,
            (Some(_), None) => self.missing_in_candidate += 1,
            (None, None) => self.missing_in_both += 1,
        }
    }
}

#[derive(Default, serde::Serialize)]
struct SignatureFieldAgreements {
    white_name: FieldAgreement,
    black_name: FieldAgreement,
    date: FieldAgreement,
    event: FieldAgreement,
    site: FieldAgreement,
    round: FieldAgreement,
    result: FieldAgreement,
    white_elo: FieldAgreement,
    black_elo: FieldAgreement,
    eco: FieldAgreement,
    time_control: FieldAgreement,
    source_id: FieldAgreement,
    broadcast_url: FieldAgreement,
    starting_fen: FieldAgreement,
    stable_white_name: FieldAgreement,
    stable_black_name: FieldAgreement,
    stable_date: FieldAgreement,
    stable_round: FieldAgreement,
    stable_source_id: FieldAgreement,
    stable_id_preimage: FieldAgreement,
}

#[derive(Default, serde::Serialize)]
struct SignatureCandidateEvidence {
    candidate_games: u64,
    distinct_signature_keys: u64,
    candidate_games_with_one_previous_match: u64,
    candidate_games_with_multiple_previous_matches: u64,
    previous_record_pairs_compared: u64,
    distinct_previous_records_matched: u64,
    previous_records_matched_by_multiple_candidate_games: u64,
    field_agreements: SignatureFieldAgreements,
}

#[derive(Clone)]
struct ComparisonMetadata {
    white: String,
    black: String,
    date: Option<String>,
    event: Option<String>,
    site: Option<String>,
    round: Option<String>,
    result: GameResult,
    white_elo: Option<u16>,
    black_elo: Option<u16>,
    eco: Option<String>,
    time_control: Option<String>,
    source_id: Option<String>,
    broadcast_url: Option<String>,
    starting_fen: Option<String>,
}

impl From<&ParsedGame> for ComparisonMetadata {
    fn from(game: &ParsedGame) -> Self {
        Self {
            white: game.metadata.white.clone(),
            black: game.metadata.black.clone(),
            date: game.metadata.date.clone(),
            event: game.metadata.event.clone(),
            site: game.metadata.site.clone(),
            round: game.metadata.round.clone(),
            result: game.metadata.result,
            white_elo: game.metadata.white_elo,
            black_elo: game.metadata.black_elo,
            eco: game.metadata.eco.clone(),
            time_control: game.metadata.time_control.clone(),
            source_id: game.metadata.source_id.clone(),
            broadcast_url: game.metadata.broadcast_url.clone(),
            starting_fen: game.fen_start.clone(),
        }
    }
}

fn compare_signature_candidate(
    evidence: &mut SignatureCandidateEvidence,
    candidate: &ParsedGame,
    previous_matches: &[ComparisonMetadata],
    candidate_signature_counts: &mut HashMap<FullMetadataMoves, u64>,
    signature_key: FullMetadataMoves,
) {
    evidence.candidate_games += 1;
    *candidate_signature_counts.entry(signature_key).or_default() += 1;
    match previous_matches.len() {
        1 => evidence.candidate_games_with_one_previous_match += 1,
        count if count > 1 => evidence.candidate_games_with_multiple_previous_matches += 1,
        _ => unreachable!("signature candidates must have a previous match"),
    }
    evidence.previous_record_pairs_compared += previous_matches.len() as u64;

    for previous in previous_matches {
        let candidate_fields = &mut evidence.field_agreements;
        candidate_fields
            .white_name
            .observe(Some(&previous.white), Some(&candidate.metadata.white));
        candidate_fields
            .black_name
            .observe(Some(&previous.black), Some(&candidate.metadata.black));
        candidate_fields
            .date
            .observe(previous.date.as_ref(), candidate.metadata.date.as_ref());
        candidate_fields
            .event
            .observe(previous.event.as_ref(), candidate.metadata.event.as_ref());
        candidate_fields
            .site
            .observe(previous.site.as_ref(), candidate.metadata.site.as_ref());
        candidate_fields
            .round
            .observe(previous.round.as_ref(), candidate.metadata.round.as_ref());
        candidate_fields
            .result
            .observe(Some(&previous.result), Some(&candidate.metadata.result));
        candidate_fields.white_elo.observe(
            previous.white_elo.as_ref(),
            candidate.metadata.white_elo.as_ref(),
        );
        candidate_fields.black_elo.observe(
            previous.black_elo.as_ref(),
            candidate.metadata.black_elo.as_ref(),
        );
        candidate_fields
            .eco
            .observe(previous.eco.as_ref(), candidate.metadata.eco.as_ref());
        candidate_fields.time_control.observe(
            previous.time_control.as_ref(),
            candidate.metadata.time_control.as_ref(),
        );
        candidate_fields.source_id.observe(
            previous.source_id.as_ref(),
            candidate.metadata.source_id.as_ref(),
        );
        candidate_fields.broadcast_url.observe(
            previous.broadcast_url.as_ref(),
            candidate.metadata.broadcast_url.as_ref(),
        );
        candidate_fields
            .starting_fen
            .observe(previous.starting_fen.as_ref(), candidate.fen_start.as_ref());
        let previous_stable_white = normalized_stable_component(Some(&previous.white));
        let candidate_stable_white = normalized_stable_component(Some(&candidate.metadata.white));
        let previous_stable_black = normalized_stable_component(Some(&previous.black));
        let candidate_stable_black = normalized_stable_component(Some(&candidate.metadata.black));
        let previous_stable_date = normalized_stable_component(previous.date.as_deref());
        let candidate_stable_date = normalized_stable_component(candidate.metadata.date.as_deref());
        let previous_stable_round = normalized_stable_component(previous.round.as_deref());
        let candidate_stable_round =
            normalized_stable_component(candidate.metadata.round.as_deref());
        let previous_stable_source_id = normalized_stable_component(previous.source_id.as_deref());
        let candidate_stable_source_id =
            normalized_stable_component(candidate.metadata.source_id.as_deref());
        candidate_fields.stable_white_name.observe(
            previous_stable_white.as_ref(),
            candidate_stable_white.as_ref(),
        );
        candidate_fields.stable_black_name.observe(
            previous_stable_black.as_ref(),
            candidate_stable_black.as_ref(),
        );
        candidate_fields.stable_date.observe(
            previous_stable_date.as_ref(),
            candidate_stable_date.as_ref(),
        );
        candidate_fields.stable_round.observe(
            previous_stable_round.as_ref(),
            candidate_stable_round.as_ref(),
        );
        candidate_fields.stable_source_id.observe(
            previous_stable_source_id.as_ref(),
            candidate_stable_source_id.as_ref(),
        );
        let stable_id_preimage_matches = previous_stable_white == candidate_stable_white
            && previous_stable_black == candidate_stable_black
            && previous_stable_date == candidate_stable_date
            && previous_stable_round == candidate_stable_round
            && previous_stable_source_id == candidate_stable_source_id;
        candidate_fields
            .stable_id_preimage
            .observe(Some(&true), Some(&stable_id_preimage_matches));
    }
}

fn normalized_stable_component(value: Option<&str>) -> Option<String> {
    value.map(|value| value.trim().to_ascii_lowercase())
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
    same_signature_candidate_evidence: SignatureCandidateEvidence,
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
    signature_evidence: &mut HashMap<FullMetadataMoves, Vec<ComparisonMetadata>>,
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
    let key = FullMetadataMoves {
        white,
        black,
        date,
        event,
        round,
        moves,
    };
    signature_evidence
        .entry(key)
        .or_default()
        .push(ComparisonMetadata::from(game));
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
    let mut old_signature_evidence = HashMap::new();
    for item in old_reader.iter_games()? {
        add_game(
            &item?,
            &mut old_ids,
            &mut old_moves,
            &mut old_players_moves,
            &mut old_players_date_moves,
            &mut old_players_year_moves,
            &mut old_signature_evidence,
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
    let mut signature_evidence = SignatureCandidateEvidence::default();
    let mut candidate_signature_counts = HashMap::new();
    for item in new_reader.iter_games()? {
        let game = item?;
        if !lichess_api_ids.contains(&game.metadata.id) {
            continue;
        }
        let (white, black, date, event, round) = metadata_key(&game);
        let key = FullMetadataMoves {
            white,
            black,
            date,
            event,
            round,
            moves: moves_key(&game),
        };
        if let Some(previous_matches) = old_signature_evidence.get(&key) {
            compare_signature_candidate(
                &mut signature_evidence,
                &game,
                previous_matches,
                &mut candidate_signature_counts,
                key,
            );
        }
    }
    signature_evidence.distinct_signature_keys = candidate_signature_counts.len() as u64;
    for (key, candidate_count) in candidate_signature_counts {
        let previous_matches = old_signature_evidence
            .get(&key)
            .expect("candidate signature was counted only after a previous match");
        signature_evidence.distinct_previous_records_matched += previous_matches.len() as u64;
        if candidate_count > 1 {
            signature_evidence.previous_records_matched_by_multiple_candidate_games +=
                previous_matches.len() as u64;
        }
    }
    let full_metadata_moves = signature_evidence.candidate_games;

    let event_or_round_drift = players_date_moves.saturating_sub(full_metadata_moves);
    let report = OverlapReport {
        schema_version: 2,
        method: "Stream all canonical games through DatasetReader; compare the previous Masters snapshot with new games whose sidecar includes Source=LichessBroadcastAPI. Game sequences use exact CompactMove vectors. Player names use opening-explorer-curate's existing normalization. Date comparison normalizes separators; year comparison is a broad candidate signal. Event and round remove non-alphanumeric ASCII characters and lowercase. For exact normalized player/date/event/round/move candidates, report match multiplicity and compare other persisted fields pairwise, distinguishing missing fields by side. Stable-ID preimage components use the same trim and ASCII lowercase rules as compute_stable_game_id; moves and collection already match by construction. Publish agreement counts only. No candidate is automatically deduplicated from this audit.".into(),
        previous_release: summary(&old_reader),
        gigabase_evaluation: summary(&new_reader),
        candidate_source: "LichessBroadcastAPI".into(),
        exact_stable_game_id_overlap: id_overlap,
        lichess_api_games_with_identical_full_move_sequence: move_only,
        lichess_api_games_with_same_players_and_moves: players_moves,
        lichess_api_games_with_same_players_date_and_moves: players_date_moves,
        lichess_api_games_with_same_players_year_and_moves: players_year_moves,
        lichess_api_games_with_same_players_date_event_round_and_moves: full_metadata_moves,
        same_signature_candidate_evidence: signature_evidence,
        same_players_date_moves_with_event_or_round_drift: event_or_round_drift,
        deduplication_decision: "No cross-release deletion or merge was performed. Exact stable-ID overlap is a direct signal; a normalized player/date/event/round/move signature is a strong manual-review candidate, not proof of game identity. Additional field-agreement counts provide context but do not replace source-level adjudication.".into(),
        limitations: vec![
            "The source tag is publisher-supplied provenance; the audit does not independently authenticate Lichess origin.".into(),
            "Player normalization intentionally follows existing deduplication behavior and may not catch every spelling or identity mismatch.".into(),
            "The prior snapshot has no comparable source-attribution sidecar; field agreement and stable-ID preimage agreement are not proof of common source identity.".into(),
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
