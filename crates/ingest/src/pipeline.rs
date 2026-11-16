use codb_core::{Collection, IngestionCompletionStatus, IngestionStats, ParsedGame};
use codb_curate::dedupe::{DedupeError, Deduplicator, DuplicateVerdict};
use codb_curate::policy::{EliteOnlinePolicy, GigabaseOtbPolicy, MastersOtbPolicy, PolicyVerdict};
use std::collections::HashSet;
use std::io::Read;
use thiserror::Error;

use crate::reader::{
    build_pgn_reader, is_recoverable_pgn_syntax_error, resync_after_syntax_error, IngestError,
    ShakmatyGameVisitor,
};

#[derive(Error, Debug)]
pub enum PipelineError {
    #[error("Ingest error: {0}")]
    Ingest(#[from] IngestError),
    #[error("Fatal IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Deduplication error: {0}")]
    Dedupe(#[from] codb_curate::dedupe::DedupeError),
    #[error("Consumer error: {0}")]
    Consumer(String),
}

/// Abstract sink for processed canonical games.
pub trait GameConsumer {
    fn accept_game(&mut self, game: ParsedGame) -> Result<(), String>;
    fn supersede_game(&mut self, old_id: u64, new_game: ParsedGame) -> Result<(), String>;

    /// Records additional source attribution for a duplicate of the retained canonical game.
    fn record_duplicate_source(
        &mut self,
        _canonical_id: u64,
        _duplicate_game: ParsedGame,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// In-memory consumer for benchmarks, fixtures, and tests.
#[derive(Default)]
pub struct MemoryGameConsumer {
    pub games: Vec<ParsedGame>,
}

impl MemoryGameConsumer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl GameConsumer for MemoryGameConsumer {
    fn accept_game(&mut self, game: ParsedGame) -> Result<(), String> {
        self.games.push(game);
        Ok(())
    }

    fn supersede_game(&mut self, old_id: u64, new_game: ParsedGame) -> Result<(), String> {
        if let Some(pos) = self.games.iter().position(|g| g.metadata.id == old_id) {
            self.games[pos] = new_game;
            Ok(())
        } else {
            Err(format!(
                "Superseded game ID {} not found in memory consumer",
                old_id
            ))
        }
    }
}

/// Incremental streaming pipeline that processes PGN streams into a GameConsumer
/// with bounded memory consumption.
pub struct CuratedPipeline<C: GameConsumer> {
    collection: Collection,
    deduplicator: Deduplicator,
    masters_policy: MastersOtbPolicy,
    elite_policy: EliteOnlinePolicy,
    gigabase_policy: Option<GigabaseOtbPolicy>,
    pub consumer: C,
    pub stats: IngestionStats,
    /// Breakdown of `stats.rejected_by_policy` by stable reason category.
    pub policy_rejection_reasons: std::collections::BTreeMap<String, u64>,
    /// Breakdown of `stats.quarantined` by stable reason category.
    pub quarantine_reasons: std::collections::BTreeMap<String, u64>,
    pub transpositions_distinct: u64,
    pub max_games: Option<usize>,
    consecutive_syntax_errors: usize,
    pub log_progress: bool,
}

impl<C: GameConsumer> CuratedPipeline<C> {
    pub fn new(collection: Collection, deduplicator: Deduplicator, consumer: C) -> Self {
        CuratedPipeline {
            collection,
            deduplicator,
            masters_policy: MastersOtbPolicy::default(),
            elite_policy: EliteOnlinePolicy::default(),
            gigabase_policy: None,
            consumer,
            stats: IngestionStats::default(),
            policy_rejection_reasons: Default::default(),
            quarantine_reasons: Default::default(),
            transpositions_distinct: 0,
            max_games: None,
            consecutive_syntax_errors: 0,
            log_progress: false,
        }
    }

    pub fn with_max_games(mut self, max: Option<usize>) -> Self {
        self.max_games = max;
        self
    }

    pub fn with_log_progress(mut self, log: bool) -> Self {
        self.log_progress = log;
        self
    }

    /// Applies the source, identity, play-mode, and historical-rating gates for a Gigabase OTB build.
    pub fn with_gigabase_source_qualification(mut self, policy: GigabaseOtbPolicy) -> Self {
        self.gigabase_policy = Some(policy);
        self
    }

    /// Applies event-level Masters OTB qualification from a reproducible audit.
    pub fn with_masters_event_qualification(
        mut self,
        verified_online_tournament_slugs: HashSet<String>,
        verified_otb_tournament_slugs: HashSet<String>,
        require_verified_otb_event: bool,
    ) -> Self {
        self.masters_policy = self.masters_policy.with_event_qualification(
            verified_online_tournament_slugs,
            verified_otb_tournament_slugs,
            require_verified_otb_event,
        );
        self
    }

    /// Ingests games from a PGN stream incrementally.
    /// Can be called multiple times across multiple source files while preserving
    /// cross-file deduplication, prefix supersession, and accounting invariants.
    pub fn process_stream<R: Read>(&mut self, reader: R) -> Result<(), PipelineError> {
        let mut pgn_reader = build_pgn_reader(reader);
        let mut visitor =
            ShakmatyGameVisitor::new(self.collection, 1).with_early_policy_filter(true);
        if let Some(policy) = self.gigabase_policy.clone() {
            visitor = visitor.with_gigabase_policy(policy);
        }

        loop {
            // Check if limit reached
            if let Some(max) = self.max_games {
                if self.stats.accepted_unique as usize >= max {
                    self.stats.completion_status = IngestionCompletionStatus::PartialLimitReached;
                    return Ok(());
                }
            }

            match pgn_reader.read_game(&mut visitor) {
                Ok(Some(result)) => {
                    self.consecutive_syntax_errors = 0;
                    self.stats.total_encountered += 1;

                    match result {
                        Ok(game) => {
                            let verdict = if let Some(policy) = &self.gigabase_policy {
                                if policy.is_r3() {
                                    match policy.evaluate_r3_game(&game) {
                                        Some(reason) => PolicyVerdict::Rejected(reason),
                                        None => PolicyVerdict::Accepted,
                                    }
                                } else {
                                    match policy.evaluate_source_metadata(
                                        game.metadata.source_attribution.as_ref(),
                                        game.metadata.date.as_deref(),
                                    ) {
                                        Ok(()) => self.masters_policy.evaluate(&game),
                                        Err(reason) => PolicyVerdict::Rejected(reason),
                                    }
                                }
                            } else {
                                match self.collection {
                                    Collection::MastersOtb => self.masters_policy.evaluate(&game),
                                    Collection::EliteOnline => self.elite_policy.evaluate(&game),
                                }
                            };

                            if let PolicyVerdict::Rejected(reason) = verdict {
                                self.stats.rejected_by_policy += 1;
                                *self
                                    .policy_rejection_reasons
                                    .entry(reason.category().to_string())
                                    .or_insert(0) += 1;
                            } else {
                                let dedupe_verdict = match self.deduplicator.evaluate(&game) {
                                    Ok(verdict) => Some(verdict),
                                    Err(DedupeError::IdCollision { .. })
                                        if self.gigabase_policy.is_some() =>
                                    {
                                        self.stats.quarantined += 1;
                                        *self
                                            .quarantine_reasons
                                            .entry("stable_game_id_collision".to_owned())
                                            .or_insert(0) += 1;
                                        None
                                    }
                                    Err(error) => return Err(PipelineError::Dedupe(error)),
                                };
                                if let Some(dedupe_verdict) = dedupe_verdict {
                                    match dedupe_verdict {
                                        DuplicateVerdict::Unique => {
                                            self.stats.accepted_unique += 1;
                                            self.consumer
                                                .accept_game(game)
                                                .map_err(PipelineError::Consumer)?;
                                        }
                                        DuplicateVerdict::TranspositionDifferentGame => {
                                            self.stats.accepted_unique += 1;
                                            self.transpositions_distinct += 1;
                                            self.consumer
                                                .accept_game(game)
                                                .map_err(PipelineError::Consumer)?;
                                        }
                                        DuplicateVerdict::ConfirmedDuplicate {
                                            canonical_id,
                                            ..
                                        } => {
                                            self.stats.confirmed_duplicates += 1;
                                            self.consumer
                                                .record_duplicate_source(canonical_id, game)
                                                .map_err(PipelineError::Consumer)?;
                                        }
                                        DuplicateVerdict::SupersedesPrevious { old_id, .. } => {
                                            self.stats.superseded_prior += 1;
                                            self.consumer
                                                .supersede_game(old_id, game)
                                                .map_err(PipelineError::Consumer)?;
                                        }
                                    }
                                }
                            }
                        }
                        Err(IngestError::RejectedByPolicy(reason)) => {
                            self.stats.rejected_by_policy += 1;
                            *self
                                .policy_rejection_reasons
                                .entry(
                                    codb_curate::policy::early_filter_category(&reason).to_string(),
                                )
                                .or_insert(0) += 1;
                        }
                        Err(IngestError::UnsupportedVariant(_))
                        | Err(IngestError::NonStandardStartingPosition(_)) => {
                            self.stats.rejected_variant += 1;
                        }
                        Err(IngestError::IllegalMove { .. }) => {
                            self.stats.rejected_illegal += 1;
                        }
                        Err(IngestError::MalformedPgn(_)) => {
                            self.stats.rejected_malformed += 1;
                        }
                        Err(other_err) => {
                            return Err(PipelineError::Ingest(other_err));
                        }
                    }
                }
                Ok(None) => {
                    // Clean EOF for this stream
                    break;
                }
                Err(io_err) => {
                    if is_recoverable_pgn_syntax_error(&io_err) {
                        self.stats.total_encountered += 1;
                        self.stats.rejected_malformed += 1;
                        if !resync_after_syntax_error(
                            &mut pgn_reader,
                            &mut self.consecutive_syntax_errors,
                        )? {
                            break;
                        }
                    } else {
                        // Fail closed on fatal I/O or decompression error
                        return Err(PipelineError::Io(io_err));
                    }
                }
            }

            if self.log_progress && self.stats.total_encountered.is_multiple_of(5000) {
                eprintln!(
                    "  [Progress] Encountered: {:>7}, Accepted: {:>6}, Policy Rejected: {:>6}, Duplicates: {:>5}, Illegal: {:>3}",
                    self.stats.total_encountered,
                    self.stats.accepted_unique,
                    self.stats.rejected_by_policy,
                    self.stats.confirmed_duplicates,
                    self.stats.rejected_illegal
                );
            }
        }

        if self.stats.completion_status != IngestionCompletionStatus::PartialLimitReached {
            self.stats.completion_status = IngestionCompletionStatus::CompleteEof;
        }

        Ok(())
    }
}
