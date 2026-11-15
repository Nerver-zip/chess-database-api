use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use codb_core::{Collection, GameSourceAttribution, IngestionStats, ParsedGame};
use codb_curate::policy::{GigabaseOtbPolicy, MastersOtbPolicy, PolicyVerdict};
use codb_ingest::{
    build_pgn_reader, is_recoverable_pgn_syntax_error, resync_after_syntax_error,
    ShakmatyGameVisitor,
};
use codb_storage::DatasetReader;
use pgn_reader::Reader;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use shakmaty::fen::Fen;
use shakmaty::{CastlingMode, Chess, Position};

#[derive(Deserialize)]
struct QualificationPolicy {
    source_tag_allowlist: Vec<String>,
}

#[derive(Deserialize)]
struct AttributionLine {
    game_id: u64,
    source_attributions: Vec<GameSourceAttribution>,
}

#[derive(Clone, Debug, Serialize)]
struct AnonymizedExample {
    source_tag: String,
    year_band: String,
    minimum_rating_band: String,
    game_length_band: String,
    result: String,
    identity_tier: String,
    play_mode_evidence: String,
    persisted_metadata_fields_match: bool,
    source_attribution_matches: bool,
    move_sequence_matches: bool,
    starting_position_matches: bool,
    final_position_matches: bool,
}

#[derive(Clone, Debug, Serialize)]
struct StratifiedExample {
    dimension: String,
    stratum: String,
    unique_candidate_games_in_stratum: u64,
    selection: String,
    example: AnonymizedExample,
}

#[derive(Serialize)]
struct SourceCanonicalReport {
    schema_version: u32,
    method: String,
    dataset_build_id: String,
    build_ingestion_accounting: IngestionStats,
    source_pgn_filename: String,
    source_pgn_size_bytes: u64,
    source_pgn_sha256_local: String,
    gigabase_policy_id: String,
    candidate_games: u64,
    source_records_encountered: u64,
    source_records_parsed_after_early_gates: u64,
    source_records_passing_full_masters_policy: u64,
    source_records_passing_policy_without_candidate_id: u64,
    source_record_parse_rejections: u64,
    recoverable_syntax_errors: u64,
    source_candidate_id_hits: u64,
    unique_candidate_games_matched: u64,
    repeated_source_records_for_matched_ids: u64,
    candidate_ids_with_repeated_source_records: u64,
    repeated_id_source_records_attributed_to_canonical: u64,
    repeated_id_source_records_without_canonical_attribution: u64,
    candidate_games_without_source_match: u64,
    persisted_metadata_mismatches: u64,
    duplicate_metadata_variant_records: u64,
    primary_source_attribution_mismatches: u64,
    move_sequence_mismatches: u64,
    starting_position_mismatches: u64,
    final_position_mismatches: u64,
    exact_candidate_records: u64,
    candidate_selection_accounting_reconciles: bool,
    source_scan_duration_ms: u128,
    stratified_examples: Vec<StratifiedExample>,
    limitations: Vec<String>,
}

struct ExampleChoice {
    selected_id: u64,
    count: u64,
    example: AnonymizedExample,
}

fn id_bytes(game_id: u64) -> [u8; 8] {
    game_id.to_be_bytes()
}

fn persisted_metadata_fields_match(source: &ParsedGame, stored: &ParsedGame) -> bool {
    let left = &source.metadata;
    let right = &stored.metadata;
    left.id == right.id
        && left.collection == right.collection
        && left.white == right.white
        && left.black == right.black
        && left.white_elo == right.white_elo
        && left.black_elo == right.black_elo
        && left.date == right.date
        && left.event == right.event
        && left.site == right.site
        && left.round == right.round
        && left.result == right.result
        && left.eco == right.eco
        && left.time_control == right.time_control
        && left.source_id == right.source_id
        && left.broadcast_url == right.broadcast_url
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

fn year_band(game: &ParsedGame) -> String {
    game.metadata
        .date
        .as_deref()
        .and_then(|value| value.get(0..4))
        .and_then(|year| year.parse::<u16>().ok())
        .map(|year| format!("{}s", year / 10 * 10))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn minimum_rating_band(game: &ParsedGame) -> String {
    match game.metadata.white_elo.zip(game.metadata.black_elo) {
        Some((white, black)) => match white.min(black) {
            0..=2199 => "2000-2199".to_owned(),
            2200..=2399 => "2200-2399".to_owned(),
            _ => "2400+".to_owned(),
        },
        None => "unknown".to_owned(),
    }
}

fn game_length_band(game: &ParsedGame) -> String {
    match game.moves.len() {
        0..=39 => "20-39_plies".to_owned(),
        40..=79 => "40-79_plies".to_owned(),
        _ => "80+_plies".to_owned(),
    }
}

fn identity_tier(attribution: &GameSourceAttribution) -> String {
    format!(
        "{}+{}",
        attribution
            .white_identity_evidence
            .as_deref()
            .unwrap_or("missing"),
        attribution
            .black_identity_evidence
            .as_deref()
            .unwrap_or("missing")
    )
}

fn strata(game: &ParsedGame, attribution: &GameSourceAttribution) -> Vec<(String, String)> {
    let tag = attribution
        .source_tag
        .clone()
        .unwrap_or_else(|| "missing".to_owned());
    let year = year_band(game);
    let rating = minimum_rating_band(game);
    let length = game_length_band(game);
    let identity = identity_tier(attribution);
    let result = game.metadata.result.to_str().to_owned();
    let mode = if attribution.mode.is_some() {
        "explicit_mode"
    } else {
        "publisher_archive_assertion"
    };
    vec![
        ("source_tag".to_owned(), tag),
        ("year_band".to_owned(), year),
        ("minimum_rating_band".to_owned(), rating),
        ("game_length_band".to_owned(), length),
        ("identity_tier".to_owned(), identity),
        ("result".to_owned(), result),
        ("play_mode_evidence".to_owned(), mode.to_owned()),
    ]
}

fn example_summary(
    game: &ParsedGame,
    attribution: &GameSourceAttribution,
    persisted_metadata_matches: bool,
    source_attribution_matches: bool,
    move_sequence_matches: bool,
    starting_position_matches: bool,
    final_position_matches: bool,
) -> AnonymizedExample {
    AnonymizedExample {
        source_tag: attribution
            .source_tag
            .clone()
            .unwrap_or_else(|| "missing".to_owned()),
        year_band: year_band(game),
        minimum_rating_band: minimum_rating_band(game),
        game_length_band: game_length_band(game),
        result: game.metadata.result.to_str().to_owned(),
        identity_tier: identity_tier(attribution),
        play_mode_evidence: if attribution.mode.is_some() {
            "explicit_mode".to_owned()
        } else {
            "publisher_archive_assertion".to_owned()
        },
        persisted_metadata_fields_match: persisted_metadata_matches,
        source_attribution_matches,
        move_sequence_matches,
        starting_position_matches,
        final_position_matches,
    }
}

fn record_candidate_games(connection: &mut Connection, reader: &DatasetReader) -> Result<u64> {
    connection.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = FILE;
         PRAGMA cache_size = -8192;
         CREATE TABLE candidates (
             game_id BLOB PRIMARY KEY NOT NULL,
             source_attributions_json TEXT NOT NULL DEFAULT '[]',
             matched_count INTEGER NOT NULL DEFAULT 0
         ) WITHOUT ROWID;",
    )?;

    let mut count = 0u64;
    let transaction = connection.transaction()?;
    {
        let mut insert =
            transaction.prepare_cached("INSERT INTO candidates (game_id) VALUES (?1)")?;
        for item in reader.iter_games()? {
            let game = item?;
            let bytes = id_bytes(game.metadata.id);
            insert.execute(params![bytes.as_slice()])?;
            count += 1;
        }
    }
    transaction.commit()?;
    Ok(count)
}

fn attach_source_attribution(connection: &mut Connection, reader: &DatasetReader) -> Result<u64> {
    let metadata = reader
        .manifest()
        .source_attribution
        .as_ref()
        .ok_or_else(|| anyhow!("candidate dataset has no source-attribution sidecar"))?;
    let path = reader.snapshot_dir().join(&metadata.filename);
    let transaction = connection.transaction()?;
    let mut count = 0u64;
    {
        let mut update = transaction.prepare_cached(
            "UPDATE candidates SET source_attributions_json = ?1 WHERE game_id = ?2",
        )?;
        for (line_index, line) in BufReader::new(File::open(&path)?).lines().enumerate() {
            let line = line?;
            let attribution: AttributionLine = serde_json::from_str(&line).with_context(|| {
                format!("parsing candidate attribution line {}", line_index + 1)
            })?;
            let bytes = id_bytes(attribution.game_id);
            let json = serde_json::to_string(&attribution.source_attributions)?;
            if update.execute(params![json, bytes.as_slice()])? != 1 {
                return Err(anyhow!(
                    "source-attribution ID does not resolve to one candidate game"
                ));
            }
            count += 1;
        }
    }
    transaction.commit()?;
    Ok(count)
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 5 {
        return Err(anyhow!(
            "usage: audit_source_canonical <dataset-root> <source-pgn> <qualification-policy.json> <report.json> <source-sha256-local>"
        ));
    }
    let dataset_root = PathBuf::from(&args[0]);
    let source_pgn = PathBuf::from(&args[1]);
    let policy_path = PathBuf::from(&args[2]);
    let report_path = PathBuf::from(&args[3]);
    let source_pgn_sha256_local = args[4].to_ascii_lowercase();
    if source_pgn_sha256_local.len() != 64
        || !source_pgn_sha256_local
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(anyhow!(
            "source archive SHA-256 must be 64 hexadecimal characters"
        ));
    }

    let mut reader = DatasetReader::open(&dataset_root)
        .with_context(|| format!("opening candidate dataset {}", dataset_root.display()))?;
    if reader.collection() != Collection::MastersOtb {
        return Err(anyhow!("source audit requires a Masters OTB dataset"));
    }

    let policy: QualificationPolicy =
        serde_json::from_reader(File::open(&policy_path).context("opening qualification policy")?)
            .context("parsing qualification policy")?;
    let gigabase_policy = GigabaseOtbPolicy::new(policy.source_tag_allowlist.into_iter().collect());
    let masters_policy = MastersOtbPolicy::default();

    let temp_dir = tempfile::tempdir().context("creating bounded-memory audit database")?;
    let mut connection = Connection::open(temp_dir.path().join("source-candidates.sqlite"))?;
    let candidate_games = record_candidate_games(&mut connection, &reader)?;
    if candidate_games != reader.total_games() {
        return Err(anyhow!(
            "streamed {} candidate games, manifest reports {}",
            candidate_games,
            reader.total_games()
        ));
    }
    let attributed_games = attach_source_attribution(&mut connection, &reader)?;
    if attributed_games != candidate_games {
        return Err(anyhow!(
            "{} candidate games but {} attribution records",
            candidate_games,
            attributed_games
        ));
    }

    let source_size_bytes = fs::metadata(&source_pgn)?.len();
    let scan_started = Instant::now();
    let source_file = File::open(&source_pgn)
        .with_context(|| format!("opening source PGN {}", source_pgn.display()))?;
    let mut pgn_reader: Reader<File> = build_pgn_reader(source_file);
    let mut visitor = ShakmatyGameVisitor::new(Collection::MastersOtb, 1)
        .with_early_policy_filter(true)
        .with_gigabase_policy(gigabase_policy);

    let mut source_records_encountered = 0u64;
    let mut source_records_parsed_after_early_gates = 0u64;
    let mut source_records_passing_full_masters_policy = 0u64;
    let mut source_record_parse_rejections = 0u64;
    let mut recoverable_syntax_errors = 0u64;
    let mut source_candidate_id_hits = 0u64;
    let mut unique_candidate_games_matched = 0u64;
    let mut repeated_source_records_for_matched_ids = 0u64;
    let mut repeated_id_source_records_attributed_to_canonical = 0u64;
    let mut repeated_id_source_records_without_canonical_attribution = 0u64;
    let mut persisted_metadata_mismatches = 0u64;
    let mut duplicate_metadata_variant_records = 0u64;
    let mut primary_source_attribution_mismatches = 0u64;
    let mut move_sequence_mismatches = 0u64;
    let mut starting_position_mismatches = 0u64;
    let mut final_position_mismatches = 0u64;
    let mut exact_candidate_records = 0u64;
    let mut consecutive_syntax_errors = 0usize;
    let mut examples: BTreeMap<(String, String), ExampleChoice> = BTreeMap::new();

    connection.execute_batch("BEGIN IMMEDIATE")?;
    let mut candidate_lookup = connection.prepare_cached(
        "SELECT source_attributions_json, matched_count FROM candidates WHERE game_id = ?1",
    )?;
    let mut mark_match = connection.prepare_cached(
        "UPDATE candidates SET matched_count = matched_count + 1 WHERE game_id = ?1",
    )?;

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
                        source_record_parse_rejections += 1;
                        continue;
                    }
                };

                if let PolicyVerdict::Rejected(_) = masters_policy.evaluate(&source_game) {
                    continue;
                }
                source_records_passing_full_masters_policy += 1;

                let id = id_bytes(source_game.metadata.id);
                let candidate: Option<(String, u64)> = candidate_lookup
                    .query_row(params![id.as_slice()], |row| {
                        Ok((row.get(0)?, row.get::<_, u64>(1)?))
                    })
                    .optional()?;
                let Some((attributions_json, previous_matches)) = candidate else {
                    continue;
                };
                source_candidate_id_hits += 1;
                let source_attributions: Vec<GameSourceAttribution> =
                    serde_json::from_str(&attributions_json)
                        .context("parsing candidate source-attribution entry")?;
                let canonical_game = reader.get_game(source_game.metadata.id)?;

                let metadata_matches =
                    persisted_metadata_fields_match(&source_game, &canonical_game);
                let moves_match = source_game.moves == canonical_game.moves;
                let starting_position_matches = source_game.fen_start == canonical_game.fen_start;
                let source_attribution_matches = source_game
                    .metadata
                    .source_attribution
                    .as_ref()
                    .is_some_and(|source| source_attributions.contains(source));
                let final_position_matches =
                    replay_final_position(&source_game)? == replay_final_position(&canonical_game)?;

                if previous_matches == 0 {
                    persisted_metadata_mismatches += u64::from(!metadata_matches);
                } else {
                    duplicate_metadata_variant_records += u64::from(!metadata_matches);
                }
                if previous_matches == 0 {
                    primary_source_attribution_mismatches += u64::from(!source_attribution_matches);
                }
                move_sequence_mismatches += u64::from(!moves_match);
                starting_position_mismatches += u64::from(!starting_position_matches);
                final_position_mismatches += u64::from(!final_position_matches);
                if previous_matches == 0 {
                    exact_candidate_records += u64::from(
                        metadata_matches
                            && source_attribution_matches
                            && moves_match
                            && starting_position_matches
                            && final_position_matches,
                    );
                }

                mark_match.execute(params![id.as_slice()])?;
                if previous_matches == 0 {
                    unique_candidate_games_matched += 1;
                    if let Some(attribution) = source_game.metadata.source_attribution.as_ref() {
                        let summary = example_summary(
                            &source_game,
                            attribution,
                            metadata_matches,
                            source_attribution_matches,
                            moves_match,
                            starting_position_matches,
                            final_position_matches,
                        );
                        for (dimension, stratum) in strata(&source_game, attribution) {
                            let key = (dimension, stratum);
                            match examples.get_mut(&key) {
                                Some(choice) => {
                                    choice.count += 1;
                                    if source_game.metadata.id < choice.selected_id {
                                        choice.selected_id = source_game.metadata.id;
                                        choice.example = summary.clone();
                                    }
                                }
                                None => {
                                    examples.insert(
                                        key,
                                        ExampleChoice {
                                            selected_id: source_game.metadata.id,
                                            count: 1,
                                            example: summary.clone(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                } else {
                    repeated_source_records_for_matched_ids += 1;
                    if source_attribution_matches {
                        repeated_id_source_records_attributed_to_canonical += 1;
                    } else {
                        repeated_id_source_records_without_canonical_attribution += 1;
                    }
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
            Err(error) => return Err(error).context("reading complete source PGN"),
        }
    }
    drop(mark_match);
    drop(candidate_lookup);
    connection.execute_batch("COMMIT")?;
    let source_scan_duration_ms = scan_started.elapsed().as_millis();

    let candidate_games_without_source_match: u64 = connection.query_row(
        "SELECT COUNT(*) FROM candidates WHERE matched_count = 0",
        [],
        |row| row.get(0),
    )?;
    let duplicate_candidate_ids: u64 = connection.query_row(
        "SELECT COUNT(*) FROM candidates WHERE matched_count > 1",
        [],
        |row| row.get(0),
    )?;
    let source_records_passing_policy_without_candidate_id =
        source_records_passing_full_masters_policy.saturating_sub(source_candidate_id_hits);
    let build_ingestion_accounting = reader.manifest().accounting.clone();
    let candidate_selection_accounting_reconciles = source_records_passing_full_masters_policy
        == build_ingestion_accounting.accepted_unique
            + build_ingestion_accounting.confirmed_duplicates
            + build_ingestion_accounting.superseded_prior
            + build_ingestion_accounting.quarantined;

    let stratified_examples: Vec<StratifiedExample> = examples
        .into_iter()
        .map(|((dimension, stratum), choice)| StratifiedExample {
            dimension,
            stratum,
            unique_candidate_games_in_stratum: choice.count,
            selection: "Anonymized representative with the lowest stable game ID in this stratum; the ID and player/event metadata are omitted.".to_owned(),
            example: choice.example,
        })
        .collect();

    let report = SourceCanonicalReport {
        schema_version: 1,
        method: "Stream the supplied source PGN with the repository's Shakmaty visitor and the exact source-tag allowlist, match parsed stable IDs against candidate IDs held in a temporary SQLite table, retrieve each matched canonical CGB3 game, and compare persisted metadata, sidecar attribution, compact moves, starting FEN, and final legal board. The report contains only anonymized stratum summaries.".to_owned(),
        dataset_build_id: reader.manifest().dataset_build_id.clone(),
        build_ingestion_accounting,
        source_pgn_filename: source_pgn
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("source.pgn")
            .to_owned(),
        source_pgn_size_bytes: source_size_bytes,
        source_pgn_sha256_local,
        gigabase_policy_id: "gigabase-otb/2026-11-r2".to_owned(),
        candidate_games,
        source_records_encountered,
        source_records_parsed_after_early_gates,
        source_records_passing_full_masters_policy,
        source_records_passing_policy_without_candidate_id,
        source_record_parse_rejections,
        recoverable_syntax_errors,
        source_candidate_id_hits,
        unique_candidate_games_matched,
        repeated_source_records_for_matched_ids,
        candidate_ids_with_repeated_source_records: duplicate_candidate_ids,
        repeated_id_source_records_attributed_to_canonical,
        repeated_id_source_records_without_canonical_attribution,
        candidate_games_without_source_match,
        persisted_metadata_mismatches,
        duplicate_metadata_variant_records,
        primary_source_attribution_mismatches,
        move_sequence_mismatches,
        starting_position_mismatches,
        final_position_mismatches,
        exact_candidate_records,
        candidate_selection_accounting_reconciles,
        source_scan_duration_ms,
        stratified_examples,
        limitations: vec![
            "Source parsing and stable game ID calculation use the repository's existing Shakmaty visitor and core ID function; this audit verifies source-to-storage traceability and serialization but is not an independent PGN parser.".to_owned(),
            "The temporary SQLite candidate-ID table keeps audit memory bounded and uses stable content-derived IDs rather than source record offsets.".to_owned(),
            "Repeated stable-ID hits can represent confirmed duplicates or quarantined collisions. The audit reports sidecar-attributed and unattributed repeats separately and reconciles the aggregate build accounting, but does not independently reproduce every deduplication verdict.".to_owned(),
            "The source PGN SHA-256 is supplied from a separate local file hash check before the scan; this runner does not recompute it while parsing.".to_owned(),
            "The source's heterogeneous upstream rights and archive-level OTB assertion remain unresolved; exact source matching does not establish legal rights or independent event play mode.".to_owned(),
            "Publisher-supplied Elo remains unverified as contemporaneous official FIDE Elo.".to_owned(),
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
        "matched {}/{} candidate games from {} source records; {} exact records; {} missing candidates; {} repeated source hits; report {}",
        unique_candidate_games_matched,
        candidate_games,
        source_records_encountered,
        exact_candidate_records,
        candidate_games_without_source_match,
        repeated_source_records_for_matched_ids,
        report_path.display()
    );

    if unique_candidate_games_matched != candidate_games
        || candidate_games_without_source_match != 0
        || persisted_metadata_mismatches != 0
        || primary_source_attribution_mismatches != 0
        || move_sequence_mismatches != 0
        || starting_position_mismatches != 0
        || final_position_mismatches != 0
        || exact_candidate_records != unique_candidate_games_matched
        || !candidate_selection_accounting_reconciles
    {
        return Err(anyhow!(
            "source-to-canonical audit failed one or more identity, duplication, or content checks"
        ));
    }
    Ok(())
}

fn main() -> Result<()> {
    run()
}
