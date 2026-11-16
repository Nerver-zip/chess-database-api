use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::ops::ControlFlow;
use std::path::Path;

use anyhow::{Context, Result};
use opening_explorer_curate::policy::{
    classify_gigabase_strength, parse_gigabase_date, r3_header_rejections, GigabaseDatePrecision,
    GigabaseHeaderMetadata, PolicyRejectionReason, APPROVED_GIGABASE_SOURCE_TAGS,
};
use opening_explorer_ingest::reader::{
    build_pgn_reader, is_recoverable_pgn_syntax_error, resync_after_syntax_error,
};
use pgn_reader::{RawTag, SanPlus, Visitor};
use rusqlite::{params, Connection};
use serde::Serialize;
use tempfile::NamedTempFile;

const AGGREGATE_CACHE_LIMIT: usize = 8192;
const MAX_AGGREGATE_ROWS: u64 = 32_768;
const MAX_TAGS_PER_RECORD: usize = 256;
const LOW_CARDINALITY_TAGS: &[&str] = &[
    "Source",
    "SourceQuality",
    "IdentityEvidenceWhite",
    "IdentityEvidenceBlack",
    "WhiteTitle",
    "BlackTitle",
    "Mode",
    "Result",
    "Variant",
    "Termination",
];
const TEXT_QUALITY_TAGS: &[&str] = &["White", "Black", "Event", "Site"];
const TRUSTED_IDENTITY_VALUES: &[&str] = &["Strong", "Confirmed"];

type AggregateCounts = BTreeMap<String, u64>;
type AggregateTags = BTreeMap<String, AggregateCounts>;
type AggregateGroups = BTreeMap<String, AggregateTags>;

#[derive(Default)]
struct HeaderTags {
    values: BTreeMap<String, String>,
    conflicting_fields: Vec<String>,
}

#[derive(Default)]
struct HeaderInventoryVisitor;

impl Visitor for HeaderInventoryVisitor {
    type Tags = HeaderTags;
    type Movetext = HeaderTags;
    type Output = HeaderTags;

    fn begin_tags(&mut self) -> ControlFlow<Self::Output, Self::Tags> {
        ControlFlow::Continue(HeaderTags::default())
    }

    fn tag(
        &mut self,
        tags: &mut Self::Tags,
        name: &[u8],
        value: RawTag<'_>,
    ) -> ControlFlow<Self::Output> {
        let name = String::from_utf8_lossy(name).into_owned();
        let decoded = value.decode();
        let value = String::from_utf8_lossy(&decoded).trim().to_owned();
        match tags.values.get(&name) {
            Some(previous) if previous != &value => {
                if !tags.conflicting_fields.contains(&name) {
                    tags.conflicting_fields.push(name);
                }
            }
            Some(_) => {}
            None => {
                tags.values.insert(name, value);
            }
        }
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: Self::Tags) -> ControlFlow<Self::Output, Self::Movetext> {
        ControlFlow::Continue(tags)
    }

    fn san(
        &mut self,
        _movetext: &mut Self::Movetext,
        _san_plus: SanPlus,
    ) -> ControlFlow<Self::Output> {
        ControlFlow::Continue(())
    }

    fn end_game(&mut self, movetext: Self::Movetext) -> Self::Output {
        movetext
    }
}

#[derive(Debug, Serialize)]
pub struct PgnInventoryReport {
    pub schema_version: u32,
    pub inventory_kind: &'static str,
    pub input: InputEvidence,
    pub parser: ParserAccounting,
    pub tag_presence_by_record: BTreeMap<String, u64>,
    pub categorical_tag_values: BTreeMap<String, BTreeMap<String, u64>>,
    pub year_distribution: BTreeMap<String, u64>,
    pub rating_tag_distributions: BTreeMap<String, BTreeMap<String, u64>>,
    pub rating_threshold_counts: RatingThresholdCounts,
    pub trusted_identity_both_count: u64,
    pub rating_threshold_and_trusted_identity_both_count: u64,
    pub rating_threshold_and_trusted_identity_by_source: BTreeMap<String, u64>,
    pub gigabase_policy_impact: GigabasePolicyImpact,
    pub text_quality_by_record: BTreeMap<String, BTreeMap<String, u64>>,
    pub records_missing_source_tag: u64,
    pub records_with_fen_tag: u64,
    pub records_with_variant_tag: u64,
    pub examples: Vec<RecordExample>,
    pub malformed_examples: Vec<MalformedExample>,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct GigabasePolicyImpact {
    pub schema_version: u32,
    pub policy_id: &'static str,
    pub method: &'static str,
    pub input_path: String,
    pub input_size_bytes: u64,
    pub input_sha256: String,
    pub records_encountered: u64,
    pub structurally_parsed: u64,
    pub recoverable_syntax_errors: u64,
    pub original_numeric_threshold_candidates: u64,
    pub valid_strength_rule_counts_non_overlapping: BTreeMap<String, u64>,
    pub strength_eligible_header_records: u64,
    pub strength_eligible_historical_records: u64,
    pub explicit_non_otb_mode_records: u64,
    pub contradictory_event_or_site_records: u64,
    pub records_with_conflicting_header_values: u64,
    pub conflicting_header_fields_by_tag: BTreeMap<String, u64>,
    pub explicit_non_otb_or_contradictory_records: u64,
    pub primary_header_rejection_counts: BTreeMap<String, u64>,
    pub overlapping_header_rejection_counts: BTreeMap<String, u64>,
    pub r3_header_candidate_count: u64,
    pub r3_header_candidates_by_strength_rule: BTreeMap<String, u64>,
    pub r3_header_candidates_by_date_precision: BTreeMap<String, u64>,
    pub r3_header_candidates_before_1970: u64,
    pub r3_candidates_admitted_by_removing_r2_source_allowlist: u64,
    pub r3_candidates_admitted_by_relaxing_identity_confidence: u64,
    pub r3_candidates_admitted_by_both_source_and_identity_changes: u64,
    pub source_records_by_major_category: BTreeMap<String, u64>,
    pub r3_header_candidates_by_source_tag: BTreeMap<String, u64>,
    pub r3_header_candidates_by_major_category: BTreeMap<String, u64>,
    pub header_candidate_accounting_reconciles: bool,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct InputEvidence {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct ParserAccounting {
    pub records_encountered: u64,
    pub records_structurally_parsed: u64,
    pub records_with_recoverable_syntax_errors: u64,
    pub completion: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RatingThresholdCounts {
    pub both_at_least_2200: u64,
    pub one_at_least_2400_and_other_at_least_2000: u64,
    pub either_established_threshold: u64,
    pub missing_or_unparsed_rating_pair: u64,
    pub note: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RecordExample {
    pub record_ordinal: u64,
    pub date: Option<String>,
    pub source: Option<String>,
    pub source_quality: Option<String>,
    pub white_elo_tag: Option<String>,
    pub black_elo_tag: Option<String>,
    pub white_title: Option<String>,
    pub black_title: Option<String>,
    pub white_identity_evidence: Option<String>,
    pub black_identity_evidence: Option<String>,
    pub result: Option<String>,
    pub has_fen: bool,
    pub has_variant: bool,
}

#[derive(Debug, Serialize)]
pub struct MalformedExample {
    pub record_ordinal: u64,
    pub parser_error: String,
}

#[derive(Default)]
struct Counters {
    encountered: u64,
    structurally_parsed: u64,
    recoverable_syntax_errors: u64,
    records_missing_source: u64,
    records_with_fen: u64,
    records_with_variant: u64,
    both_2200: u64,
    one_2400_other_2000: u64,
    either_threshold: u64,
    missing_rating_pair: u64,
    trusted_identity_both: u64,
    rating_and_trusted_identity_both: u64,
    policy_impact: PolicyImpactCounters,
}

#[derive(Default)]
struct PolicyImpactCounters {
    strength_rules: BTreeMap<String, u64>,
    strength_eligible: u64,
    strength_historical: u64,
    explicit_non_otb_mode_records: u64,
    contradictory_records: u64,
    conflicting_header_records: u64,
    conflicting_header_fields: BTreeMap<String, u64>,
    explicit_non_otb_or_contradictory_records: u64,
    primary_rejections: BTreeMap<String, u64>,
    overlapping_rejections: BTreeMap<String, u64>,
    candidates: u64,
    candidate_rules: BTreeMap<String, u64>,
    candidate_date_precision: BTreeMap<String, u64>,
    historical_candidates: u64,
    source_allowlist_removals: u64,
    identity_relaxations: u64,
    source_identity_overlap: u64,
    source_records_by_major_category: BTreeMap<String, u64>,
    candidates_by_source: BTreeMap<String, u64>,
    candidates_by_major_category: BTreeMap<String, u64>,
}

#[derive(Hash, Eq, PartialEq)]
struct AggregateKey {
    group: String,
    tag: String,
    value: String,
}

struct AggregateStore {
    _scratch_file: NamedTempFile,
    conn: Connection,
    pending: HashMap<AggregateKey, u64>,
}

impl AggregateStore {
    fn new(scratch_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(scratch_dir).with_context(|| {
            format!(
                "creating inventory scratch directory {}",
                scratch_dir.display()
            )
        })?;
        let scratch_file = NamedTempFile::new_in(scratch_dir).with_context(|| {
            format!(
                "creating inventory scratch file in {}",
                scratch_dir.display()
            )
        })?;
        let conn = Connection::open(scratch_file.path()).with_context(|| {
            format!(
                "opening inventory scratch database {}",
                scratch_file.path().display()
            )
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode = OFF;
             PRAGMA synchronous = OFF;
             PRAGMA temp_store = FILE;
             PRAGMA cache_size = -512;
             CREATE TABLE aggregates (
                 group_name TEXT NOT NULL,
                 tag_name TEXT NOT NULL,
                 tag_value TEXT NOT NULL,
                 item_count INTEGER NOT NULL,
                 PRIMARY KEY (group_name, tag_name, tag_value)
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            _scratch_file: scratch_file,
            conn,
            pending: HashMap::new(),
        })
    }

    fn add(&mut self, group: &str, tag: &str, value: &str) -> Result<()> {
        let key = AggregateKey {
            group: group.to_owned(),
            tag: tag.to_owned(),
            value: value.to_owned(),
        };
        *self.pending.entry(key).or_default() += 1;
        if self.pending.len() >= AGGREGATE_CACHE_LIMIT {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut statement = tx.prepare_cached(
                "INSERT INTO aggregates (group_name, tag_name, tag_value, item_count)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(group_name, tag_name, tag_value)
                 DO UPDATE SET item_count = item_count + excluded.item_count",
            )?;
            for (key, count) in self.pending.drain() {
                statement.execute(params![key.group, key.tag, key.value, count])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn load_groups(&mut self) -> Result<AggregateGroups> {
        self.flush()?;
        let aggregate_rows: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM aggregates", [], |row| row.get(0))?;
        anyhow::ensure!(
            aggregate_rows <= MAX_AGGREGATE_ROWS,
            "inventory has {aggregate_rows} distinct tag aggregates, above the bounded report limit {MAX_AGGREGATE_ROWS}"
        );
        let mut groups = BTreeMap::new();
        let mut statement = self.conn.prepare(
            "SELECT group_name, tag_name, tag_value, item_count
             FROM aggregates ORDER BY group_name, tag_name, tag_value",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let group: String = row.get(0)?;
            let tag: String = row.get(1)?;
            let value: String = row.get(2)?;
            let count: u64 = row.get(3)?;
            groups
                .entry(group)
                .or_insert_with(BTreeMap::new)
                .entry(tag)
                .or_insert_with(BTreeMap::new)
                .insert(value, count);
        }
        Ok(groups)
    }
}

pub fn inventory_pgn<R: Read>(
    reader: R,
    input_path: &Path,
    input_size_bytes: u64,
    input_sha256: String,
    scratch_dir: &Path,
) -> Result<PgnInventoryReport> {
    let mut aggregate = AggregateStore::new(scratch_dir)?;
    let mut counters = Counters::default();
    let mut examples = Vec::new();
    let mut sampled_sources = HashSet::new();
    let mut malformed_examples = Vec::new();
    let mut reader = build_pgn_reader(reader);
    let mut visitor = HeaderInventoryVisitor;
    let mut consecutive_syntax_errors = 0usize;

    loop {
        match reader.read_game(&mut visitor) {
            Ok(Some(tags)) => {
                consecutive_syntax_errors = 0;
                counters.encountered += 1;
                counters.structurally_parsed += 1;
                let ordinal = counters.encountered;
                anyhow::ensure!(
                    tags.values.len() <= MAX_TAGS_PER_RECORD,
                    "record {ordinal} has more than {MAX_TAGS_PER_RECORD} distinct header tags"
                );
                let values: HashMap<&str, &str> = tags
                    .values
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str()))
                    .collect();

                let policy_headers = inventory_gigabase_headers(&values, &tags.conflicting_fields);
                counters.policy_impact.conflicting_header_records +=
                    u64::from(!tags.conflicting_fields.is_empty());
                for field in &tags.conflicting_fields {
                    increment(
                        &mut counters.policy_impact.conflicting_header_fields,
                        field.clone(),
                    );
                }
                let source = policy_headers.source_tag.unwrap_or("<missing>");
                let major_category = major_source_category(source);
                increment(
                    &mut counters.policy_impact.source_records_by_major_category,
                    major_category.to_owned(),
                );

                let strength_rule = classify_gigabase_strength(&policy_headers);
                if let Ok(rule) = &strength_rule {
                    increment(
                        &mut counters.policy_impact.strength_rules,
                        rule.as_str().to_owned(),
                    );
                    counters.policy_impact.strength_eligible += 1;
                    if parse_gigabase_date(policy_headers.date)
                        .and_then(|date| date.year)
                        .is_some_and(|year| year < 1970)
                    {
                        counters.policy_impact.strength_historical += 1;
                    }
                }

                let rejections = r3_header_rejections(&policy_headers);
                let mut has_non_otb_mode = false;
                let mut has_contradictory_evidence = false;
                for reason in &rejections {
                    let category = reason.category().to_owned();
                    increment(&mut counters.policy_impact.overlapping_rejections, category);
                    has_non_otb_mode |=
                        matches!(reason, PolicyRejectionReason::NonOtbGigabaseMode(_));
                    has_contradictory_evidence |= matches!(
                        reason,
                        PolicyRejectionReason::ContradictoryGigabaseProvenance { .. }
                    );
                }
                counters.policy_impact.explicit_non_otb_mode_records += u64::from(has_non_otb_mode);
                counters.policy_impact.contradictory_records +=
                    u64::from(has_contradictory_evidence);
                counters
                    .policy_impact
                    .explicit_non_otb_or_contradictory_records +=
                    u64::from(has_non_otb_mode || has_contradictory_evidence);

                if let Some(reason) = rejections.first() {
                    increment(
                        &mut counters.policy_impact.primary_rejections,
                        reason.category().to_owned(),
                    );
                } else {
                    let rule = strength_rule
                        .expect("a passing record has a strength rule")
                        .as_str();
                    counters.policy_impact.candidates += 1;
                    increment(&mut counters.policy_impact.candidate_rules, rule.to_owned());
                    let date = parse_gigabase_date(policy_headers.date)
                        .expect("a passing record has a valid or unknown date");
                    let precision = match date.precision {
                        GigabaseDatePrecision::Unknown => "unknown",
                        GigabaseDatePrecision::Partial => "partial",
                        GigabaseDatePrecision::Complete => "complete",
                    };
                    increment(
                        &mut counters.policy_impact.candidate_date_precision,
                        precision.to_owned(),
                    );
                    if date.year.is_some_and(|year| year < 1970) {
                        counters.policy_impact.historical_candidates += 1;
                    }
                    let source_is_r2_allowed = APPROVED_GIGABASE_SOURCE_TAGS.contains(&source);
                    let identity_strong_both =
                        is_trusted_identity(policy_headers.white_identity_evidence)
                            && is_trusted_identity(policy_headers.black_identity_evidence);
                    counters.policy_impact.source_allowlist_removals +=
                        u64::from(!source_is_r2_allowed);
                    counters.policy_impact.identity_relaxations += u64::from(!identity_strong_both);
                    counters.policy_impact.source_identity_overlap +=
                        u64::from(!source_is_r2_allowed && !identity_strong_both);
                    increment(
                        &mut counters.policy_impact.candidates_by_source,
                        source.to_owned(),
                    );
                    increment(
                        &mut counters.policy_impact.candidates_by_major_category,
                        major_category.to_owned(),
                    );
                }

                for (name, value) in &tags.values {
                    aggregate.add("tag_presence", name, "")?;
                    if LOW_CARDINALITY_TAGS.contains(&name.as_str()) {
                        aggregate.add("categorical_tag_values", name, value)?;
                    }
                }

                for tag in TEXT_QUALITY_TAGS {
                    let quality = match values.get(tag) {
                        None => "missing",
                        Some(value) if value.trim().is_empty() => "empty",
                        Some(value) if is_placeholder(value) => "placeholder",
                        Some(_) => "populated",
                    };
                    aggregate.add("text_quality", tag, quality)?;
                }

                if let Some(date) = values.get("Date") {
                    let year = date.split(['.', '-']).next().unwrap_or_default();
                    let bucket = match year.parse::<u16>() {
                        Ok(year) if (1000..=2999).contains(&year) => year.to_string(),
                        _ if date.contains('?') || date.is_empty() => "partial_or_unknown".into(),
                        _ => "invalid".into(),
                    };
                    aggregate.add("date_year", "Date", &bucket)?;
                } else {
                    aggregate.add("date_year", "Date", "missing")?;
                }

                let white = values.get("WhiteElo").and_then(|v| v.parse::<u16>().ok());
                let black = values.get("BlackElo").and_then(|v| v.parse::<u16>().ok());
                for (tag, raw) in [
                    ("WhiteElo", values.get("WhiteElo")),
                    ("BlackElo", values.get("BlackElo")),
                ] {
                    let bucket = match raw {
                        None => "missing".to_owned(),
                        Some(value) => match value.parse::<u16>() {
                            Ok(rating) => rating_bucket(rating),
                            Err(_) => "invalid_or_unparsed".to_owned(),
                        },
                    };
                    aggregate.add("rating_tag_distribution", tag, &bucket)?;
                }
                match (white, black) {
                    (Some(white), Some(black)) => {
                        let both_2200 = white >= 2200 && black >= 2200;
                        let one_2400_other_2000 =
                            (white >= 2400 && black >= 2000) || (black >= 2400 && white >= 2000);
                        counters.both_2200 += u64::from(both_2200);
                        counters.one_2400_other_2000 += u64::from(one_2400_other_2000);
                        counters.either_threshold += u64::from(both_2200 || one_2400_other_2000);
                    }
                    _ => counters.missing_rating_pair += 1,
                }

                let trusted_identity_both = values
                    .get("IdentityEvidenceWhite")
                    .is_some_and(|value| TRUSTED_IDENTITY_VALUES.contains(value));
                let trusted_identity_both = trusted_identity_both
                    && values
                        .get("IdentityEvidenceBlack")
                        .is_some_and(|value| TRUSTED_IDENTITY_VALUES.contains(value));
                counters.trusted_identity_both += u64::from(trusted_identity_both);
                let threshold_met = white.zip(black).is_some_and(|(white, black)| {
                    (white >= 2200 && black >= 2200)
                        || (white >= 2400 && black >= 2000)
                        || (black >= 2400 && white >= 2000)
                });
                if threshold_met && trusted_identity_both {
                    counters.rating_and_trusted_identity_both += 1;
                    aggregate.add(
                        "rating_identity_source",
                        "Source",
                        values.get("Source").copied().unwrap_or("<missing>"),
                    )?;
                }

                counters.records_missing_source += u64::from(!values.contains_key("Source"));
                counters.records_with_fen += u64::from(values.contains_key("FEN"));
                counters.records_with_variant += u64::from(values.contains_key("Variant"));

                let sample_source = values.get("Source").copied().unwrap_or("<missing>");
                if examples.len() < 40 && sampled_sources.insert(sample_source.to_owned()) {
                    examples.push(RecordExample {
                        record_ordinal: ordinal,
                        date: values.get("Date").map(|value| (*value).to_owned()),
                        source: values.get("Source").map(|value| (*value).to_owned()),
                        source_quality: values
                            .get("SourceQuality")
                            .map(|value| (*value).to_owned()),
                        white_elo_tag: values.get("WhiteElo").map(|value| (*value).to_owned()),
                        black_elo_tag: values.get("BlackElo").map(|value| (*value).to_owned()),
                        white_title: values.get("WhiteTitle").map(|value| (*value).to_owned()),
                        black_title: values.get("BlackTitle").map(|value| (*value).to_owned()),
                        white_identity_evidence: values
                            .get("IdentityEvidenceWhite")
                            .map(|value| (*value).to_owned()),
                        black_identity_evidence: values
                            .get("IdentityEvidenceBlack")
                            .map(|value| (*value).to_owned()),
                        result: values.get("Result").map(|value| (*value).to_owned()),
                        has_fen: values.contains_key("FEN"),
                        has_variant: values.contains_key("Variant"),
                    });
                }
            }
            Ok(None) => break,
            Err(io_error) if is_recoverable_pgn_syntax_error(&io_error) => {
                counters.encountered += 1;
                counters.recoverable_syntax_errors += 1;
                if malformed_examples.len() < 40 {
                    malformed_examples.push(MalformedExample {
                        record_ordinal: counters.encountered,
                        parser_error: io_error.to_string(),
                    });
                }
                if !resync_after_syntax_error(&mut reader, &mut consecutive_syntax_errors)
                    .context("resynchronizing after malformed PGN syntax")?
                {
                    break;
                }
            }
            Err(io_error) => {
                anyhow::bail!("fatal PGN parser or input error: {io_error}");
            }
        }
    }

    anyhow::ensure!(
        counters.encountered == counters.structurally_parsed + counters.recoverable_syntax_errors,
        "PGN inventory accounting did not reconcile"
    );

    let groups = aggregate.load_groups()?;
    let tag_presence_by_record = groups
        .get("tag_presence")
        .map(|tags| {
            tags.iter()
                .map(|(tag, values)| (tag.clone(), values.get("").copied().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    let categorical_tag_values = groups
        .get("categorical_tag_values")
        .cloned()
        .unwrap_or_default();
    let year_distribution = groups
        .get("date_year")
        .and_then(|group| group.get("Date"))
        .cloned()
        .unwrap_or_default();
    let rating_tag_distributions = groups
        .get("rating_tag_distribution")
        .cloned()
        .unwrap_or_default();
    let text_quality_by_record = groups.get("text_quality").cloned().unwrap_or_default();
    let rating_threshold_and_trusted_identity_by_source = groups
        .get("rating_identity_source")
        .and_then(|group| group.get("Source"))
        .cloned()
        .unwrap_or_default();

    let accounted_headers = counters.policy_impact.candidates
        + counters
            .policy_impact
            .primary_rejections
            .values()
            .sum::<u64>();
    let gigabase_policy_impact = GigabasePolicyImpact {
        schema_version: 1,
        policy_id: "gigabase-otb/2026-11-r3",
        method: "full_streaming_record_header_scan_using_the_production_r3_header_policy",
        input_path: input_path.display().to_string(),
        input_size_bytes,
        input_sha256: input_sha256.clone(),
        records_encountered: counters.encountered,
        structurally_parsed: counters.structurally_parsed,
        recoverable_syntax_errors: counters.recoverable_syntax_errors,
        original_numeric_threshold_candidates: counters.either_threshold,
        valid_strength_rule_counts_non_overlapping: counters.policy_impact.strength_rules,
        strength_eligible_header_records: counters.policy_impact.strength_eligible,
        strength_eligible_historical_records: counters.policy_impact.strength_historical,
        explicit_non_otb_mode_records: counters.policy_impact.explicit_non_otb_mode_records,
        contradictory_event_or_site_records: counters.policy_impact.contradictory_records,
        records_with_conflicting_header_values: counters
            .policy_impact
            .conflicting_header_records,
        conflicting_header_fields_by_tag: counters.policy_impact.conflicting_header_fields,
        explicit_non_otb_or_contradictory_records: counters
            .policy_impact
            .explicit_non_otb_or_contradictory_records,
        primary_header_rejection_counts: counters.policy_impact.primary_rejections,
        overlapping_header_rejection_counts: counters.policy_impact.overlapping_rejections,
        r3_header_candidate_count: counters.policy_impact.candidates,
        r3_header_candidates_by_strength_rule: counters.policy_impact.candidate_rules,
        r3_header_candidates_by_date_precision: counters.policy_impact.candidate_date_precision,
        r3_header_candidates_before_1970: counters.policy_impact.historical_candidates,
        r3_candidates_admitted_by_removing_r2_source_allowlist: counters
            .policy_impact
            .source_allowlist_removals,
        r3_candidates_admitted_by_relaxing_identity_confidence: counters
            .policy_impact
            .identity_relaxations,
        r3_candidates_admitted_by_both_source_and_identity_changes: counters
            .policy_impact
            .source_identity_overlap,
        source_records_by_major_category: counters
            .policy_impact
            .source_records_by_major_category,
        r3_header_candidates_by_source_tag: counters.policy_impact.candidates_by_source,
        r3_header_candidates_by_major_category: counters
            .policy_impact
            .candidates_by_major_category,
        header_candidate_accounting_reconciles: accounted_headers
            == counters.structurally_parsed,
        limitations: vec![
            "Counts are full-source header-level candidates; moves, legality, deduplication, and retention are measured only by the production build.",
            "Strength-rule categories are mutually exclusive in priority order, while rejection reason counts overlap when multiple issues occur in one record.",
            "GM title labels and Elo values are publisher-supplied and are not independently validated as official or contemporaneous records.",
            "Unknown dates remain unknown; no dates, ratings, identities, or historical title dates are inferred.",
        ],
    };

    drop(aggregate);

    Ok(PgnInventoryReport {
        schema_version: 2,
        inventory_kind: "pgn_structural_and_header_inventory",
        input: InputEvidence {
            path: input_path.display().to_string(),
            size_bytes: input_size_bytes,
            sha256: input_sha256,
        },
        parser: ParserAccounting {
            records_encountered: counters.encountered,
            records_structurally_parsed: counters.structurally_parsed,
            records_with_recoverable_syntax_errors: counters.recoverable_syntax_errors,
            completion: "clean_eof",
        },
        tag_presence_by_record,
        categorical_tag_values,
        year_distribution,
        rating_tag_distributions,
        rating_threshold_counts: RatingThresholdCounts {
            both_at_least_2200: counters.both_2200,
            one_at_least_2400_and_other_at_least_2000: counters.one_2400_other_2000,
            either_established_threshold: counters.either_threshold,
            missing_or_unparsed_rating_pair: counters.missing_rating_pair,
            note: "Counts use PGN WhiteElo and BlackElo fields only. Their rating system and historical meaning are not established by these tags.",
        },
        trusted_identity_both_count: counters.trusted_identity_both,
        rating_threshold_and_trusted_identity_both_count: counters
            .rating_and_trusted_identity_both,
        rating_threshold_and_trusted_identity_by_source,
        gigabase_policy_impact,
        text_quality_by_record,
        records_missing_source_tag: counters.records_missing_source,
        records_with_fen_tag: counters.records_with_fen,
        records_with_variant_tag: counters.records_with_variant,
        examples,
        malformed_examples,
        limitations: vec![
            "This inventory parses PGN structure and headers but does not replay chess moves for legality.",
            "The production build must report legal, policy, variant, duplicate, supersession, and quarantine accounting separately.",
            "Presence of a FIDE ID, title, Elo field, Source tag, or identity-confidence tag does not alone prove event play mode or upstream licensing.",
        ],
    })
}

fn rating_bucket(rating: u16) -> String {
    match rating {
        0..=999 => "0000-0999".into(),
        1000..=1199 => "1000-1199".into(),
        1200..=1399 => "1200-1399".into(),
        1400..=1599 => "1400-1599".into(),
        1600..=1799 => "1600-1799".into(),
        1800..=1999 => "1800-1999".into(),
        2000..=2199 => "2000-2199".into(),
        2200..=2399 => "2200-2399".into(),
        2400..=2599 => "2400-2599".into(),
        2600..=2799 => "2600-2799".into(),
        _ => "2800+".into(),
    }
}

fn inventory_gigabase_headers<'a>(
    values: &HashMap<&'a str, &'a str>,
    conflicting_fields: &'a [String],
) -> GigabaseHeaderMetadata<'a> {
    GigabaseHeaderMetadata {
        source_tag: values.get("Source").copied(),
        white_name: values.get("White").copied(),
        black_name: values.get("Black").copied(),
        white_elo: values.get("WhiteElo").copied(),
        black_elo: values.get("BlackElo").copied(),
        white_title: values.get("WhiteTitle").copied(),
        black_title: values.get("BlackTitle").copied(),
        white_identity_evidence: values.get("IdentityEvidenceWhite").copied(),
        black_identity_evidence: values.get("IdentityEvidenceBlack").copied(),
        white_fide_id: values.get("WhiteFideId").copied(),
        black_fide_id: values.get("BlackFideId").copied(),
        mode: values.get("Mode").copied(),
        date: values.get("Date").copied(),
        event: values.get("Event").copied(),
        site: values.get("Site").copied(),
        conflicting_metadata_fields: conflicting_fields,
    }
}

fn increment(counts: &mut BTreeMap<String, u64>, key: String) {
    *counts.entry(key).or_default() += 1;
}

fn is_trusted_identity(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value.eq_ignore_ascii_case("Strong") || value.eq_ignore_ascii_case("Confirmed")
    })
}

fn major_source_category(source: &str) -> &'static str {
    match source {
        "Legacy Software Dump" => "Legacy Software Dump",
        "Independent Collection" => "Independent Collection",
        "Historical Archive" => "Historical Archive",
        "AnaSchaigorodsky" => "Academic Dataset",
        "LichessBroadcastAPI" => "Open Platform Data",
        "LEDB" => "Open Platform Data",
        "Community Project" => "Community Project",
        "VargaCsabaZoltan" => "Player Provided Dataset",
        "Britbase"
        | "CanadianChessFederation"
        | "ChessScotland"
        | "DanBase"
        | "Federscacchi"
        | "GreekBase"
        | "NiederrheinischerSchachverband"
        | "NiedersächsischerSchachverband"
        | "SchachverbandRuhrgebiet" => "Official Federation",
        "<missing>" => "Missing Source",
        _ => "Other Source",
    }
}

fn is_placeholder(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "?" | "-" | "." | "unknown" | "n/a" | "na" | "none"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn inventory_counts_headers_and_preserves_source_categories() {
        let pgn = concat!(
            "[Event \"Event A\"]\n",
            "[Site \"City\"]\n",
            "[Date \"1987.01.02\"]\n",
            "[White \"Player A\"]\n",
            "[Black \"Player B\"]\n",
            "[WhiteElo \"2450\"]\n",
            "[BlackElo \"2100\"]\n",
            "[Source \"Federation A\"]\n",
            "[IdentityEvidenceWhite \"Confirmed\"]\n",
            "[IdentityEvidenceBlack \"Strong\"]\n",
            "[Result \"1-0\"]\n\n",
            "1. e4 e5 1-0\n\n",
            "[Event \"Event B\"]\n",
            "[White \"Player C\"]\n",
            "[Black \"Player D\"]\n",
            "[WhiteElo \"?\"]\n",
            "[BlackElo \"1800\"]\n",
            "[Source \"Historical Archive\"]\n",
            "[Source \"Legacy Software Dump\"]\n",
            "[Result \"*\"]\n\n",
            "1. d4 d5 *\n\n",
        );
        let scratch = tempfile::tempdir().unwrap();
        let report = inventory_pgn(
            Cursor::new(pgn.as_bytes()),
            Path::new("fixture.pgn"),
            pgn.len() as u64,
            "0".repeat(64),
            scratch.path(),
        )
        .unwrap();

        assert_eq!(report.parser.records_encountered, 2);
        assert_eq!(report.parser.records_structurally_parsed, 2);
        assert_eq!(report.records_missing_source_tag, 0);
        assert_eq!(report.trusted_identity_both_count, 1);
        assert_eq!(report.rating_threshold_and_trusted_identity_both_count, 1);
        assert_eq!(
            report.rating_threshold_and_trusted_identity_by_source["Federation A"],
            1
        );
        assert_eq!(report.categorical_tag_values["Source"]["Federation A"], 1);
        assert_eq!(
            report.categorical_tag_values["Source"]["Historical Archive"],
            1
        );
        assert_eq!(
            report
                .rating_threshold_counts
                .one_at_least_2400_and_other_at_least_2000,
            1
        );
        assert_eq!(
            report
                .rating_threshold_counts
                .missing_or_unparsed_rating_pair,
            1
        );
        assert_eq!(report.year_distribution["1987"], 1);
        assert_eq!(report.year_distribution["missing"], 1);
        assert_eq!(report.text_quality_by_record["Site"]["populated"], 1);
        assert_eq!(report.text_quality_by_record["Site"]["missing"], 1);
        assert_eq!(report.gigabase_policy_impact.r3_header_candidate_count, 1);
        assert_eq!(
            report
                .gigabase_policy_impact
                .r3_candidates_admitted_by_removing_r2_source_allowlist,
            1
        );
        assert_eq!(
            report
                .gigabase_policy_impact
                .r3_header_candidates_by_date_precision["complete"],
            1
        );
        assert!(
            report
                .gigabase_policy_impact
                .header_candidate_accounting_reconciles
        );
        assert_eq!(
            report
                .gigabase_policy_impact
                .records_with_conflicting_header_values,
            1
        );
        assert_eq!(
            report
                .gigabase_policy_impact
                .conflicting_header_fields_by_tag["Source"],
            1
        );
    }

    #[test]
    fn rating_bins_cover_boundaries() {
        assert_eq!(rating_bucket(999), "0000-0999");
        assert_eq!(rating_bucket(1000), "1000-1199");
        assert_eq!(rating_bucket(2799), "2600-2799");
        assert_eq!(rating_bucket(2800), "2800+");
    }
}
