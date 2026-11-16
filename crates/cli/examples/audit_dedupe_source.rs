use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use codb_core::{Collection, GameMetadata, GameSourceAttribution, ParsedGame};
use codb_curate::dedupe::{normalize_metadata, DedupeError, Deduplicator, DuplicateVerdict};
use codb_curate::policy::GigabaseOtbPolicy;
use codb_ingest::{
    build_pgn_reader, is_recoverable_pgn_syntax_error, resync_after_syntax_error,
    ShakmatyGameVisitor,
};
use codb_storage::{DatasetReader, StorageError};
use pgn_reader::Reader;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use shakmaty::fen::Fen;
use shakmaty::{CastlingMode, Chess, Position};

#[derive(Deserialize)]
struct QualificationPolicy {
    policy_id: String,
}

#[derive(Deserialize)]
struct AttributionLine {
    game_id: u64,
    source_attributions: Vec<GameSourceAttribution>,
}

#[derive(Serialize)]
struct AnonymizedComparison {
    example_index: u64,
    dedupe_disposition: String,
    canonical_game_retrieved: bool,
    source_stable_id_equals_canonical_stable_id: bool,
    normalized_metadata_key_matches: bool,
    metadata_fields_differing: Vec<String>,
    source_attribution_is_in_candidate_sidecar: bool,
    moves_match: bool,
    source_moves_are_strict_prefix_of_canonical: bool,
    canonical_moves_are_strict_prefix_of_source: bool,
    starting_position_matches: bool,
    final_position_matches: bool,
}

#[derive(Default, Serialize)]
struct OutcomeCounts {
    unique: u64,
    transposition_different_game: u64,
    confirmed_duplicate: u64,
    superseded_prior: u64,
    quarantined_stable_id_collision: u64,
}

#[derive(Serialize)]
struct SourceDedupeReport {
    schema_version: u32,
    audit_kind: String,
    method: String,
    dataset_build_id: String,
    source_pgn_filename: String,
    source_pgn_size_bytes: u64,
    gigabase_policy_id: String,
    source_records_encountered: u64,
    source_visitor_rejections: u64,
    recoverable_syntax_errors: u64,
    source_records_parsed_after_early_gates: u64,
    source_full_policy_rejections: u64,
    source_records_passing_full_gigabase_policy: u64,
    source_record_accounting_reconciles: bool,
    dedupe_outcomes: OutcomeCounts,
    dedupe_outcomes_reconcile: bool,
    comparison_records: u64,
    anonymized_examples_emitted: u64,
    candidate_lookup_failures: u64,
    metadata_drift_records: u64,
    metadata_drift_with_same_normalized_key: u64,
    metadata_drift_with_same_moves: u64,
    metadata_drift_with_different_moves: u64,
    source_moves_prefix_of_canonical_records: u64,
    canonical_moves_prefix_of_source_records: u64,
    confirmed_duplicates_with_source_prefix: u64,
    source_prefix_records_with_starting_position_match: u64,
    source_prefix_records_with_same_final_position: u64,
    source_prefix_records_with_different_final_position: u64,
    source_prefix_records_not_confirmed_duplicates: u64,
    source_prefix_records_validated_as_truncation: u64,
    source_prefix_case_validated_as_truncation: bool,
    prefix_classification_reconciles: bool,
    source_attribution_missing_from_candidate_sidecar: u64,
    stable_id_collision_metadata_drift_records: u64,
    confirmed_duplicate_metadata_drift_records: u64,
    anonymized_comparisons: Vec<AnonymizedComparison>,
    report_contains_player_or_event_values: bool,
    report_contains_stable_game_id_values: bool,
    report_contains_raw_pgn: bool,
    source_scan_duration_ms: u128,
    limitations: Vec<String>,
}

fn metadata_differences(source: &GameMetadata, canonical: &GameMetadata) -> Vec<String> {
    let mut differences = Vec::new();
    macro_rules! compare_field {
        ($field:ident, $label:literal) => {
            if source.$field != canonical.$field {
                differences.push($label.to_owned());
            }
        };
    }
    compare_field!(id, "stable_game_id");
    compare_field!(collection, "collection");
    compare_field!(white, "white_player_text");
    compare_field!(black, "black_player_text");
    compare_field!(white_elo, "white_elo");
    compare_field!(black_elo, "black_elo");
    compare_field!(date, "date");
    compare_field!(event, "event_text");
    compare_field!(site, "site_text");
    compare_field!(round, "round");
    compare_field!(result, "result");
    compare_field!(eco, "eco");
    compare_field!(time_control, "time_control");
    compare_field!(source_id, "source_id_text");
    compare_field!(broadcast_url, "broadcast_url_text");
    differences
}

fn replay_final_position(game: &ParsedGame) -> Result<Chess> {
    let mut board = match game.fen_start.as_deref() {
        Some(fen) => {
            let parsed: Fen = fen.parse().context("parsing source game starting FEN")?;
            parsed
                .into_position(CastlingMode::Standard)
                .map_err(|error| anyhow!("invalid source game starting FEN: {error:?}"))?
        }
        None => Chess::default(),
    };
    for (ply, compact_move) in game.moves.iter().copied().enumerate() {
        let played = compact_move.to_shakmaty_move(&board).ok_or_else(|| {
            anyhow!(
                "illegal stored move at ply {} ({})",
                ply + 1,
                compact_move.to_uci()
            )
        })?;
        board.play_unchecked(played);
    }
    Ok(board)
}

fn is_strict_prefix(shorter: &[codb_core::CompactMove], longer: &[codb_core::CompactMove]) -> bool {
    shorter.len() < longer.len() && longer.starts_with(shorter)
}

fn comparison(
    example_index: u64,
    disposition: &str,
    source: &ParsedGame,
    canonical: Option<&ParsedGame>,
    source_attributions: &[GameSourceAttribution],
) -> Result<AnonymizedComparison> {
    let Some(canonical) = canonical else {
        return Ok(AnonymizedComparison {
            example_index,
            dedupe_disposition: disposition.to_owned(),
            canonical_game_retrieved: false,
            source_stable_id_equals_canonical_stable_id: false,
            normalized_metadata_key_matches: false,
            metadata_fields_differing: Vec::new(),
            source_attribution_is_in_candidate_sidecar: false,
            moves_match: false,
            source_moves_are_strict_prefix_of_canonical: false,
            canonical_moves_are_strict_prefix_of_source: false,
            starting_position_matches: false,
            final_position_matches: false,
        });
    };
    let attribution_matches = source
        .metadata
        .source_attribution
        .as_ref()
        .is_some_and(|attribution| source_attributions.contains(attribution));
    Ok(AnonymizedComparison {
        example_index,
        dedupe_disposition: disposition.to_owned(),
        canonical_game_retrieved: true,
        source_stable_id_equals_canonical_stable_id: source.metadata.id == canonical.metadata.id,
        normalized_metadata_key_matches: normalize_metadata(source)
            == normalize_metadata(canonical),
        metadata_fields_differing: metadata_differences(&source.metadata, &canonical.metadata),
        source_attribution_is_in_candidate_sidecar: attribution_matches,
        moves_match: source.moves == canonical.moves,
        source_moves_are_strict_prefix_of_canonical: is_strict_prefix(
            &source.moves,
            &canonical.moves,
        ),
        canonical_moves_are_strict_prefix_of_source: is_strict_prefix(
            &canonical.moves,
            &source.moves,
        ),
        starting_position_matches: source.fen_start == canonical.fen_start,
        final_position_matches: replay_final_position(source)? == replay_final_position(canonical)?,
    })
}

fn record_sidecar(connection: &Connection, reader: &DatasetReader) -> Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = FILE;
         PRAGMA cache_size = -256;
         CREATE TABLE attributions (
             game_id BLOB NOT NULL,
             attribution_json TEXT NOT NULL,
             PRIMARY KEY (game_id, attribution_json)
         ) WITHOUT ROWID;",
    )?;
    let sidecar = reader
        .manifest()
        .source_attribution
        .as_ref()
        .ok_or_else(|| anyhow!("candidate dataset has no source-attribution sidecar"))?;
    let path = reader.snapshot_dir().join(&sidecar.filename);
    let transaction = connection.unchecked_transaction()?;
    {
        let mut insert = transaction.prepare_cached(
            "INSERT INTO attributions (game_id, attribution_json) VALUES (?1, ?2)",
        )?;
        for (line_index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
            let line = line?;
            let record: AttributionLine = serde_json::from_str(&line)
                .with_context(|| format!("parsing source-attribution line {}", line_index + 1))?;
            let id = record.game_id.to_be_bytes();
            for attribution in record.source_attributions {
                insert.execute(params![id.as_slice(), serde_json::to_string(&attribution)?])?;
            }
        }
    }
    transaction.commit()?;
    Ok(())
}

fn attributions_for(connection: &Connection, game_id: u64) -> Result<Vec<GameSourceAttribution>> {
    let id = game_id.to_be_bytes();
    let mut statement =
        connection.prepare("SELECT attribution_json FROM attributions WHERE game_id = ?1")?;
    let rows = statement.query_map(params![id.as_slice()], |row| row.get::<_, String>(0))?;
    rows.map(|row| {
        let json = row?;
        serde_json::from_str(&json).context("parsing candidate source-attribution JSON")
    })
    .collect()
}

fn classify_outcome(counts: &mut OutcomeCounts, verdict: &Result<DuplicateVerdict, DedupeError>) {
    match verdict {
        Ok(DuplicateVerdict::Unique) => counts.unique += 1,
        Ok(DuplicateVerdict::TranspositionDifferentGame) => {
            counts.transposition_different_game += 1
        }
        Ok(DuplicateVerdict::ConfirmedDuplicate { .. }) => counts.confirmed_duplicate += 1,
        Ok(DuplicateVerdict::SupersedesPrevious { .. }) => counts.superseded_prior += 1,
        Err(DedupeError::IdCollision { .. }) => counts.quarantined_stable_id_collision += 1,
        Err(_) => unreachable!("non-collision errors are returned before classification"),
    }
}

fn canonical_id_for(
    source_id: u64,
    verdict: &Result<DuplicateVerdict, DedupeError>,
) -> Option<(u64, &'static str)> {
    match verdict {
        Ok(DuplicateVerdict::ConfirmedDuplicate { canonical_id, .. }) => {
            Some((*canonical_id, "confirmed_duplicate"))
        }
        Ok(DuplicateVerdict::SupersedesPrevious { .. }) => Some((source_id, "superseded_prior")),
        Err(DedupeError::IdCollision { .. }) => {
            Some((source_id, "quarantined_stable_id_collision"))
        }
        Ok(DuplicateVerdict::Unique | DuplicateVerdict::TranspositionDifferentGame) => None,
        Err(_) => None,
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 4 {
        return Err(anyhow!(
            "usage: audit_dedupe_source <dataset-root> <source-pgn> <qualification-policy.json> <report.json>"
        ));
    }
    let dataset_root = PathBuf::from(&args[0]);
    let source_pgn = PathBuf::from(&args[1]);
    let policy_path = PathBuf::from(&args[2]);
    let report_path = PathBuf::from(&args[3]);

    let mut reader = DatasetReader::open(&dataset_root)
        .with_context(|| format!("opening candidate dataset {}", dataset_root.display()))?;
    if reader.collection() != Collection::MastersOtb {
        return Err(anyhow!(
            "source deduplication audit requires a Masters OTB dataset"
        ));
    }
    let policy: QualificationPolicy =
        serde_json::from_reader(File::open(policy_path).context("opening policy file")?)
            .context("parsing policy file")?;
    if policy.policy_id != "gigabase-otb/2026-11-r3" {
        return Err(anyhow!("expected Gigabase r3 qualification policy"));
    }
    let gigabase_policy = GigabaseOtbPolicy::publisher_trust_r3();

    let temp = tempfile::tempdir().context("creating temporary dedupe audit storage")?;
    let connection = Connection::open(temp.path().join("candidate-attribution.sqlite"))?;
    record_sidecar(&connection, &reader)?;
    let mut deduplicator = Deduplicator::new_disk(&temp.path().join("source-dedupe.sqlite"))?;

    let source_size_bytes = fs::metadata(&source_pgn)?.len();
    let started = Instant::now();
    let source_file = File::open(&source_pgn)
        .with_context(|| format!("opening source PGN {}", source_pgn.display()))?;
    let mut pgn_reader: Reader<File> = build_pgn_reader(source_file);
    let mut visitor = ShakmatyGameVisitor::new(Collection::MastersOtb, 1)
        .with_early_policy_filter(true)
        .with_gigabase_policy(gigabase_policy.clone());
    let mut source_records_encountered = 0u64;
    let mut source_records_parsed_after_early_gates = 0u64;
    let mut source_full_policy_rejections = 0u64;
    let mut source_records_passing_full_gigabase_policy = 0u64;
    let mut recoverable_syntax_errors = 0u64;
    let mut consecutive_syntax_errors = 0usize;
    let mut outcomes = OutcomeCounts::default();
    let mut source_visitor_rejections = 0u64;
    let mut comparison_records = 0u64;
    let mut comparisons = Vec::new();
    let mut candidate_lookup_failures = 0u64;
    let mut metadata_drift_records = 0u64;
    let mut metadata_drift_with_same_normalized_key = 0u64;
    let mut metadata_drift_with_same_moves = 0u64;
    let mut metadata_drift_with_different_moves = 0u64;
    let mut source_moves_prefix_of_canonical_records = 0u64;
    let mut canonical_moves_prefix_of_source_records = 0u64;
    let mut confirmed_duplicates_with_source_prefix = 0u64;
    let mut source_prefix_records_with_starting_position_match = 0u64;
    let mut source_prefix_records_with_same_final_position = 0u64;
    let mut source_prefix_records_with_different_final_position = 0u64;
    let mut source_prefix_records_not_confirmed_duplicates = 0u64;
    let mut source_prefix_records_validated_as_truncation = 0u64;
    let mut source_attribution_missing_from_candidate_sidecar = 0u64;
    let mut stable_id_collision_metadata_drift_records = 0u64;
    let mut confirmed_duplicate_metadata_drift_records = 0u64;

    loop {
        match pgn_reader.read_game(&mut visitor) {
            Ok(Some(result)) => {
                consecutive_syntax_errors = 0;
                source_records_encountered += 1;
                let source_game = match result {
                    Ok(game) => {
                        source_records_parsed_after_early_gates += 1;
                        game
                    }
                    Err(_) => {
                        source_visitor_rejections += 1;
                        continue;
                    }
                };
                if gigabase_policy.evaluate_r3_game(&source_game).is_some() {
                    source_full_policy_rejections += 1;
                    continue;
                }
                source_records_passing_full_gigabase_policy += 1;
                let verdict = deduplicator.evaluate(&source_game);
                if let Err(error) = &verdict {
                    if !matches!(error, DedupeError::IdCollision { .. }) {
                        return Err(anyhow!("deduplicator failed during source audit: {error}"));
                    }
                }
                classify_outcome(&mut outcomes, &verdict);
                let Some((canonical_id, disposition)) =
                    canonical_id_for(source_game.metadata.id, &verdict)
                else {
                    if let Err(error) = verdict {
                        return Err(error.into());
                    }
                    continue;
                };
                let canonical = match reader.get_game(canonical_id) {
                    Ok(game) => Some(game),
                    Err(StorageError::GameNotFound(_)) => None,
                    Err(error) => return Err(error.into()),
                };
                if canonical.is_none() {
                    candidate_lookup_failures += 1;
                }
                let sidecar = attributions_for(&connection, canonical_id)?;
                let example = comparison(
                    comparison_records + 1,
                    disposition,
                    &source_game,
                    canonical.as_ref(),
                    &sidecar,
                )?;
                comparison_records += 1;
                if !example.metadata_fields_differing.is_empty() {
                    metadata_drift_records += 1;
                    if example.normalized_metadata_key_matches {
                        metadata_drift_with_same_normalized_key += 1;
                    }
                    if example.moves_match {
                        metadata_drift_with_same_moves += 1;
                    } else {
                        metadata_drift_with_different_moves += 1;
                    }
                    if example.dedupe_disposition == "quarantined_stable_id_collision" {
                        stable_id_collision_metadata_drift_records += 1;
                    }
                    if example.dedupe_disposition == "confirmed_duplicate" {
                        confirmed_duplicate_metadata_drift_records += 1;
                    }
                }
                if !example.source_attribution_is_in_candidate_sidecar {
                    source_attribution_missing_from_candidate_sidecar += 1;
                }
                if example.source_moves_are_strict_prefix_of_canonical {
                    source_moves_prefix_of_canonical_records += 1;
                    if example.dedupe_disposition == "confirmed_duplicate" {
                        confirmed_duplicates_with_source_prefix += 1;
                    } else {
                        source_prefix_records_not_confirmed_duplicates += 1;
                    }
                    if example.starting_position_matches {
                        source_prefix_records_with_starting_position_match += 1;
                    }
                    if example.final_position_matches {
                        source_prefix_records_with_same_final_position += 1;
                    } else {
                        source_prefix_records_with_different_final_position += 1;
                    }
                    if example.dedupe_disposition == "confirmed_duplicate"
                        && !example.moves_match
                        && !example.canonical_moves_are_strict_prefix_of_source
                        && example.starting_position_matches
                        && !example.final_position_matches
                    {
                        source_prefix_records_validated_as_truncation += 1;
                    }
                }
                if example.canonical_moves_are_strict_prefix_of_source {
                    canonical_moves_prefix_of_source_records += 1;
                }
                if comparisons.len() < 50 {
                    comparisons.push(example);
                }
            }
            Ok(None) => break,
            Err(error) if is_recoverable_pgn_syntax_error(&error) => {
                source_records_encountered += 1;
                recoverable_syntax_errors += 1;
                if !resync_after_syntax_error(&mut pgn_reader, &mut consecutive_syntax_errors)? {
                    break;
                }
            }
            Err(error) => return Err(error).context("reading the source PGN"),
        }
    }

    let duration_ms = started.elapsed().as_millis();
    let dedupe_outcomes_reconcile = source_records_passing_full_gigabase_policy
        == outcomes.unique
            + outcomes.transposition_different_game
            + outcomes.confirmed_duplicate
            + outcomes.superseded_prior
            + outcomes.quarantined_stable_id_collision;
    let source_record_accounting_reconciles = source_records_encountered
        == source_visitor_rejections
            + source_records_parsed_after_early_gates
            + recoverable_syntax_errors;
    let source_policy_accounting_reconciles = source_records_parsed_after_early_gates
        == source_full_policy_rejections + source_records_passing_full_gigabase_policy;
    let source_prefix_case_validated_as_truncation = source_moves_prefix_of_canonical_records > 0
        && source_prefix_records_validated_as_truncation
            == source_moves_prefix_of_canonical_records;
    let prefix_classification_reconciles = source_moves_prefix_of_canonical_records
        == confirmed_duplicates_with_source_prefix
        && source_prefix_records_not_confirmed_duplicates == 0
        && source_prefix_records_with_starting_position_match
            == source_moves_prefix_of_canonical_records
        && source_prefix_records_validated_as_truncation
            + source_prefix_records_with_same_final_position
            == source_moves_prefix_of_canonical_records
        && canonical_moves_prefix_of_source_records == 0;
    let report = SourceDedupeReport {
        schema_version: 3,
        audit_kind: "source_derived_dedupe_comparisons".to_owned(),
        method: "Stream the complete supplied source PGN through the existing Shakmaty visitor and Gigabase r3 publisher-trust header and game checks, replay every policy-passing record through the production disk-backed Deduplicator, and compare duplicate/supersession/quarantine records with their canonical CGB3 record. Comparison output contains only disposition labels, field names, strict move-prefix and board-position counts, and boolean checks; it includes no move sequences or metadata values.".to_owned(),
        dataset_build_id: reader.manifest().dataset_build_id.clone(),
        source_pgn_filename: source_pgn
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("source.pgn")
            .to_owned(),
        source_pgn_size_bytes: source_size_bytes,
        gigabase_policy_id: "gigabase-otb/2026-11-r3".to_owned(),
        source_records_encountered,
        source_visitor_rejections,
        recoverable_syntax_errors,
        source_records_parsed_after_early_gates,
        source_full_policy_rejections,
        source_records_passing_full_gigabase_policy,
        source_record_accounting_reconciles: source_record_accounting_reconciles
            && source_policy_accounting_reconciles,
        dedupe_outcomes: outcomes,
        dedupe_outcomes_reconcile,
        comparison_records,
        anonymized_examples_emitted: comparisons.len() as u64,
        candidate_lookup_failures,
        metadata_drift_records,
        metadata_drift_with_same_normalized_key,
        metadata_drift_with_same_moves,
        metadata_drift_with_different_moves,
        source_moves_prefix_of_canonical_records,
        canonical_moves_prefix_of_source_records,
        confirmed_duplicates_with_source_prefix,
        source_prefix_records_with_starting_position_match,
        source_prefix_records_with_same_final_position,
        source_prefix_records_with_different_final_position,
        source_prefix_records_not_confirmed_duplicates,
        source_prefix_records_validated_as_truncation,
        source_prefix_case_validated_as_truncation,
        prefix_classification_reconciles,
        source_attribution_missing_from_candidate_sidecar,
        stable_id_collision_metadata_drift_records,
        confirmed_duplicate_metadata_drift_records,
        anonymized_comparisons: comparisons,
        report_contains_player_or_event_values: false,
        report_contains_stable_game_id_values: false,
        report_contains_raw_pgn: false,
        source_scan_duration_ms: duration_ms,
        limitations: vec![
            "The source record parser, stable IDs, qualification policy, and deduplication outcomes use repository production implementations; this is traceability evidence, not an independent parser or deduplication algorithm.".to_owned(),
            "Metadata comparison names fields that differ but emits no player, event, site, source ID, game ID, or raw PGN values.".to_owned(),
            "A strict source move-prefix relationship is not treated as proof of truncation; starting and final positions are compared separately.".to_owned(),
            "Source-asserted OTB mode, publisher-supplied Elo provenance, publisher checksum, and upstream rights remain unresolved.".to_owned(),
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
        "scanned {} records; {} passed policy; dedupe comparisons {}; metadata drift {}; candidate misses {}; report {}",
        source_records_encountered,
        source_records_passing_full_gigabase_policy,
        report.comparison_records,
        report.metadata_drift_records,
        candidate_lookup_failures,
        report_path.display()
    );
    if recoverable_syntax_errors != 0
        || !dedupe_outcomes_reconcile
        || !source_record_accounting_reconciles
        || !source_policy_accounting_reconciles
        || !prefix_classification_reconciles
        || candidate_lookup_failures != 0
    {
        return Err(anyhow!(
            "source dedupe audit failed reconciliation or candidate retrieval"
        ));
    }
    Ok(())
}

fn main() -> Result<()> {
    run()
}

#[cfg(test)]
mod tests {
    use super::is_strict_prefix;
    use codb_core::CompactMove;

    #[test]
    fn strict_move_prefix_is_directional_and_excludes_equal_sequences() {
        let short = [CompactMove(1), CompactMove(2)];
        let long = [CompactMove(1), CompactMove(2), CompactMove(3)];

        assert!(is_strict_prefix(&short, &long));
        assert!(!is_strict_prefix(&long, &short));
        assert!(!is_strict_prefix(&short, &short));
    }
}
