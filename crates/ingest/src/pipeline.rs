use codb_core::{Collection, IngestionCompletionStatus, IngestionStats, ParsedGame};
use codb_curate::dedupe::{Deduplicator, DuplicateVerdict};
use codb_curate::policy::{EliteOnlinePolicy, MastersOtbPolicy, PolicyVerdict};
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
    #[error("Consumer error: {0}")]
    Consumer(String),
}

/// Abstract sink for processed canonical games.
pub trait GameConsumer {
    fn accept_game(&mut self, game: ParsedGame) -> Result<(), String>;
    fn supersede_game(&mut self, old_id: u64, new_game: ParsedGame) -> Result<(), String>;
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
    pub consumer: C,
    pub stats: IngestionStats,
    pub transpositions_distinct: u64,
    pub max_games: Option<usize>,
    consecutive_syntax_errors: usize,
}

impl<C: GameConsumer> CuratedPipeline<C> {
    pub fn new(collection: Collection, deduplicator: Deduplicator, consumer: C) -> Self {
        CuratedPipeline {
            collection,
            deduplicator,
            masters_policy: MastersOtbPolicy::default(),
            elite_policy: EliteOnlinePolicy::default(),
            consumer,
            stats: IngestionStats::default(),
            transpositions_distinct: 0,
            max_games: None,
            consecutive_syntax_errors: 0,
        }
    }

    pub fn with_max_games(mut self, max: Option<usize>) -> Self {
        self.max_games = max;
        self
    }

    /// Ingests games from a PGN stream incrementally.
    /// Can be called multiple times across multiple source files while preserving
    /// cross-file deduplication, prefix supersession, and accounting invariants.
    pub fn process_stream<R: Read>(&mut self, reader: R) -> Result<(), PipelineError> {
        let mut pgn_reader = build_pgn_reader(reader);
        let mut visitor = ShakmatyGameVisitor::new(self.collection, 1);

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
                            let passes_policy = match self.collection {
                                Collection::MastersOtb => {
                                    self.masters_policy.evaluate(&game) == PolicyVerdict::Accepted
                                }
                                Collection::EliteOnline => {
                                    self.elite_policy.evaluate(&game) == PolicyVerdict::Accepted
                                }
                            };

                            if !passes_policy {
                                self.stats.rejected_by_policy += 1;
                            } else {
                                match self.deduplicator.evaluate(&game) {
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
                                    DuplicateVerdict::ConfirmedDuplicate { .. } => {
                                        self.stats.confirmed_duplicates += 1;
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
        }

        if self.stats.completion_status != IngestionCompletionStatus::PartialLimitReached {
            self.stats.completion_status = IngestionCompletionStatus::CompleteEof;
        }

        Ok(())
    }
}
