//! Exact filtered opening statistics reconstructed from position postings.

use super::control::SearchControl;
use super::postings::{PostingIter, SearchPosting, SearchPostingsReader};
use super::query::{GameFilters, QueryError, Range, SearchCondition};
use crate::position::{MoveStats, PositionStats};
use chess_database_core::{CompactMove, PositionKey};
use rusqlite::{params_from_iter, types::Value, Connection, OptionalExtension};
use std::collections::BTreeMap;

const SMALL_POSITION_THRESHOLD: usize = 512;
const MAX_AGGREGATE_WORK: usize = 250_000;
const MAX_PROJECTED_GAMES: u32 = 4_000_000;
const MAX_PROJECTED_POSTING_BYTES: usize = 20 * 1024 * 1024;

/// In-memory projection of the verified catalog. Every row occupies at most 16 bytes.
pub struct AggregateProjection {
    games: Vec<ProjectedGame>,
}

#[derive(Clone, Copy)]
struct ProjectedGame {
    date_lower: u32,
    date_upper: u32,
    white: u16,
    black: u16,
    known_ratings: u8,
    outcome: u8,
}

impl AggregateProjection {
    pub fn load(connection: &Connection, game_count: u32) -> Result<Option<Self>, QueryError> {
        if game_count > MAX_PROJECTED_GAMES {
            return Ok(None);
        }
        let mut games = Vec::with_capacity(game_count as usize);
        let mut statement = connection.prepare(
            "SELECT ordinal, date_lower, date_upper, white_rating, black_rating, result
             FROM games ORDER BY ordinal",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if row.get::<_, u32>(0)? as usize != games.len() || games.len() == game_count as usize {
                return Err(inconsistent_projection());
            }
            let white = row.get::<_, Option<u16>>(3)?;
            let black = row.get::<_, Option<u16>>(4)?;
            let outcome = outcome_slot(&row.get::<_, String>(5)?)?.map_or(3, |slot| slot as u8);
            games.push(ProjectedGame {
                date_lower: row.get::<_, Option<u32>>(1)?.unwrap_or(0),
                date_upper: row.get::<_, Option<u32>>(2)?.unwrap_or(0),
                white: white.unwrap_or_default(),
                black: black.unwrap_or_default(),
                known_ratings: u8::from(white.is_some()) | (u8::from(black.is_some()) << 1),
                outcome,
            });
        }
        if games.len() != game_count as usize {
            return Err(inconsistent_projection());
        }
        Ok(Some(Self { games }))
    }

    /// Return None when the predicate needs the SQL-backed planner.
    pub fn aggregate(
        &self,
        postings: &SearchPostingsReader,
        key: PositionKey,
        depth: usize,
        filters: &GameFilters,
        control: &SearchControl,
    ) -> Result<Option<PositionStats>, QueryError> {
        let GameFilters {
            player: None,
            opponent: None,
            white: None,
            black: None,
            event_id: None,
            event: None,
            site: None,
            eco: None,
            time_control_category: None,
            plies: Range {
                min: None,
                max: None,
            },
            ..
        } = filters
        else {
            return Ok(None);
        };
        if filters.player_rating != Range::default() {
            return Ok(None);
        }
        let Some(group) = postings.get(key)? else {
            return Ok(Some(empty_stats(key)));
        };
        if group.remaining_encoded_bytes() > MAX_PROJECTED_POSTING_BYTES {
            return Err(QueryError::WorkLimit);
        }
        self.scan(group, key, depth, filters, control).map(Some)
    }

    fn scan(
        &self,
        group: PostingIter<'_>,
        key: PositionKey,
        depth: usize,
        filters: &GameFilters,
        control: &SearchControl,
    ) -> Result<PositionStats, QueryError> {
        let result = filters.result.as_deref().map(outcome_slot).transpose()?;
        let mut counts = BTreeMap::<CompactMove, [u32; 3]>::new();
        let mut previous_ordinal = None;
        for (index, posting) in group.enumerate() {
            if index.is_multiple_of(512) && control.expired() {
                return Err(QueryError::Deadline);
            }
            let posting = posting?;
            if previous_ordinal == Some(posting.ordinal) {
                continue;
            }
            previous_ordinal = Some(posting.ordinal);
            let Some(played) = posting.played.filter(|_| usize::from(posting.ply) < depth) else {
                continue;
            };
            let game = self
                .games
                .get(posting.ordinal as usize)
                .ok_or_else(inconsistent_projection)?;
            let outcome = usize::from(game.outcome);
            if outcome < 3
                && result.is_none_or(|requested| requested == Some(outcome))
                && game.matches(filters)
            {
                add_outcome(&mut counts, played, outcome)?;
            }
        }
        build_stats(key, counts)
    }
}

fn inconsistent_projection() -> QueryError {
    crate::position::IndexError::InvalidFormat(
        "catalog projection ordinals do not match the snapshot".into(),
    )
    .into()
}

impl ProjectedGame {
    fn matches(self, filters: &GameFilters) -> bool {
        let white = (self.known_ratings & 1 != 0).then_some(u32::from(self.white));
        let black = (self.known_ratings & 2 != 0).then_some(u32::from(self.black));
        let sum = white.zip(black).map(|(white, black)| white + black);
        let year = (self.date_lower != 0).then_some(self.date_lower / 10_000);
        within(filters.white_rating, white, 1)
            && within(filters.black_rating, black, 1)
            && within(filters.both_ratings, white, 1)
            && within(filters.both_ratings, black, 1)
            && (within(filters.either_rating, white, 1) || within(filters.either_rating, black, 1))
            && within(filters.average_rating, sum, 2)
            && within(filters.year, year, 1)
            && filters
                .date
                .min
                .is_none_or(|min| self.date_upper != 0 && self.date_upper >= min)
            && filters
                .date
                .max
                .is_none_or(|max| self.date_lower != 0 && self.date_lower <= max)
    }
}

fn within(range: Range, value: Option<u32>, multiplier: u64) -> bool {
    range == Range::default()
        || value.is_some_and(|value| {
            let value = u64::from(value);
            range
                .min
                .is_none_or(|min| value >= u64::from(min) * multiplier)
                && range
                    .max
                    .is_none_or(|max| value <= u64::from(max) * multiplier)
        })
}

/// Reconstruct legacy-compatible CPI statistics for the filtered subset.
///
/// A game contributes at most once, using its first position occurrence before
/// `cpi_depth` that has a recorded continuation. Games with unknown results and
/// terminal positions are excluded, matching CPI v2 semantics. Callers should
/// route an unfiltered request to CPI v2 directly.
pub fn filtered_position_stats(
    connection: &Connection,
    postings: &SearchPostingsReader,
    key: PositionKey,
    cpi_depth: usize,
    filters: &GameFilters,
    control: &SearchControl,
) -> Result<PositionStats, QueryError> {
    if cpi_depth == 0 {
        return Err(crate::position::IndexError::InvalidFormat(
            "CPI coverage depth must be positive".to_owned(),
        )
        .into());
    }
    let cpi_depth = u16::try_from(cpi_depth).map_err(|_| {
        crate::position::IndexError::InvalidFormat(
            "CPI coverage depth exceeds the supported ply range".to_owned(),
        )
    })?;
    control.interrupt_sqlite(connection);
    if control.expired() {
        return Err(QueryError::Deadline);
    }
    let condition = filters.condition();

    let Some(group) = postings.get(key)? else {
        return Ok(empty_stats(key));
    };
    let small_group = group
        .take(SMALL_POSITION_THRESHOLD + 1)
        .try_fold(0usize, |count, posting| posting.map(|_| count + 1))?
        <= SMALL_POSITION_THRESHOLD;

    let mut counts = BTreeMap::<CompactMove, [u32; 3]>::new();
    if small_group {
        aggregate_small_group(
            connection,
            postings,
            key,
            cpi_depth,
            &condition,
            control,
            &mut counts,
        )?;
    } else {
        let candidate_count = count_known_candidates(connection, &condition, control)?;
        if candidate_count <= SMALL_POSITION_THRESHOLD as u64 {
            aggregate_selective_metadata(
                connection,
                postings,
                key,
                cpi_depth,
                &condition,
                control,
                &mut counts,
            )?;
        } else {
            aggregate_merge(
                connection,
                postings,
                key,
                cpi_depth,
                &condition,
                control,
                &mut counts,
            )?;
        }
    }
    build_stats(key, counts)
}

fn aggregate_small_group(
    connection: &Connection,
    postings: &SearchPostingsReader,
    key: PositionKey,
    depth: u16,
    condition: &SearchCondition,
    control: &SearchControl,
    counts: &mut BTreeMap<CompactMove, [u32; 3]>,
) -> Result<(), QueryError> {
    let Some(group) = postings.get(key)? else {
        return Ok(());
    };
    let sql = format!(
        "SELECT result FROM games WHERE ordinal = ? AND ({}) AND result != '*'",
        condition.sql
    );
    let mut statement = connection.prepare(&sql)?;
    let mut values = Vec::with_capacity(condition.values.len() + 1);
    values.push(Value::Integer(0));
    values.extend(condition.values.iter().cloned());
    let mut previous_ordinal = None;
    for (work, posting) in group.enumerate() {
        check_work(work, control)?;
        let posting = posting?;
        if previous_ordinal == Some(posting.ordinal) {
            continue;
        }
        previous_ordinal = Some(posting.ordinal);
        let Some(mv) = qualifying_move(posting, depth) else {
            continue;
        };
        values[0] = Value::Integer(i64::from(posting.ordinal));
        let result = statement
            .query_row(params_from_iter(&values), |row| row.get::<_, String>(0))
            .optional()
            .map_err(|error| query_error(error, control))?;
        if let Some(result) = result {
            add_result(counts, mv, &result)?;
        }
    }
    Ok(())
}

fn aggregate_selective_metadata(
    connection: &Connection,
    postings: &SearchPostingsReader,
    key: PositionKey,
    depth: u16,
    condition: &SearchCondition,
    control: &SearchControl,
    counts: &mut BTreeMap<CompactMove, [u32; 3]>,
) -> Result<(), QueryError> {
    let sql = format!(
        "SELECT ordinal, result FROM games WHERE ({}) AND result != '*' ORDER BY ordinal",
        condition.sql
    );
    let mut statement = connection.prepare(&sql)?;
    let mut candidates = statement
        .query(params_from_iter(&condition.values))
        .map_err(|error| query_error(error, control))?;
    let mut work = 0usize;
    while let Some(candidate) = candidates
        .next()
        .map_err(|error| query_error(error, control))?
    {
        check_work(work, control)?;
        work += 1;
        let ordinal = u32::try_from(candidate.get::<_, i64>(0)?).map_err(|_| {
            crate::position::IndexError::InvalidFormat(
                "metadata ordinal is outside the supported range".to_owned(),
            )
        })?;
        let result: String = candidate.get(1)?;
        let Some(mut group) = postings.seek(key, ordinal)? else {
            continue;
        };
        for posting in &mut group {
            check_work(work, control)?;
            work += 1;
            let posting = posting?;
            if posting.ordinal > ordinal {
                break;
            }
            if posting.ordinal == ordinal {
                if let Some(mv) = qualifying_move(posting, depth) {
                    add_result(counts, mv, &result)?;
                }
                break;
            }
        }
    }
    Ok(())
}

fn aggregate_merge(
    connection: &Connection,
    postings: &SearchPostingsReader,
    key: PositionKey,
    depth: u16,
    condition: &SearchCondition,
    control: &SearchControl,
    counts: &mut BTreeMap<CompactMove, [u32; 3]>,
) -> Result<(), QueryError> {
    let Some(group) = postings.get(key)? else {
        return Ok(());
    };
    let sql = format!(
        "SELECT ordinal, result FROM games WHERE ({}) AND result != '*' ORDER BY ordinal",
        condition.sql
    );
    let mut statement = connection.prepare(&sql)?;
    let mut candidates = statement
        .query(params_from_iter(&condition.values))
        .map_err(|error| query_error(error, control))?;
    let mut work = 0usize;
    let mut candidate = next_candidate(&mut candidates, control)?;
    let mut previous_ordinal = None;
    for posting in group {
        check_work(work, control)?;
        work += 1;
        let posting = posting?;
        if previous_ordinal == Some(posting.ordinal) {
            continue;
        }
        previous_ordinal = Some(posting.ordinal);
        while candidate
            .as_ref()
            .is_some_and(|(ordinal, _)| *ordinal < posting.ordinal)
        {
            check_work(work, control)?;
            work += 1;
            candidate = next_candidate(&mut candidates, control)?;
        }
        let Some((ordinal, result)) = candidate.as_ref() else {
            break;
        };
        if *ordinal == posting.ordinal {
            if let Some(mv) = qualifying_move(posting, depth) {
                add_result(counts, mv, result)?;
            }
            check_work(work, control)?;
            work += 1;
            candidate = next_candidate(&mut candidates, control)?;
        }
    }
    Ok(())
}

fn count_known_candidates(
    connection: &Connection,
    condition: &SearchCondition,
    control: &SearchControl,
) -> Result<u64, QueryError> {
    connection
        .query_row(
            &format!(
                "SELECT count(*) FROM (SELECT 1 FROM games WHERE ({}) AND result != '*' LIMIT {})",
                condition.sql,
                SMALL_POSITION_THRESHOLD + 1
            ),
            params_from_iter(&condition.values),
            |row| row.get(0),
        )
        .map_err(|error| query_error(error, control))
}

fn next_candidate(
    rows: &mut rusqlite::Rows<'_>,
    control: &SearchControl,
) -> Result<Option<(u32, String)>, QueryError> {
    let Some(row) = rows.next().map_err(|error| query_error(error, control))? else {
        return Ok(None);
    };
    let ordinal = u32::try_from(row.get::<_, i64>(0)?).map_err(|_| {
        crate::position::IndexError::InvalidFormat(
            "metadata ordinal is outside the supported range".to_owned(),
        )
    })?;
    Ok(Some((ordinal, row.get(1)?)))
}

fn qualifying_move(posting: SearchPosting, depth: u16) -> Option<CompactMove> {
    (posting.ply < depth).then_some(posting.played).flatten()
}

fn add_result(
    counts: &mut BTreeMap<CompactMove, [u32; 3]>,
    mv: CompactMove,
    result: &str,
) -> Result<(), QueryError> {
    if let Some(slot) = outcome_slot(result)? {
        add_outcome(counts, mv, slot)?;
    }
    Ok(())
}

fn outcome_slot(result: &str) -> Result<Option<usize>, QueryError> {
    Ok(Some(match result {
        "1-0" => 0,
        "1/2-1/2" => 1,
        "0-1" => 2,
        "*" => return Ok(None),
        other => {
            return Err(crate::position::IndexError::InvalidFormat(format!(
                "unsupported game result `{other}` in metadata catalog"
            ))
            .into());
        }
    }))
}

fn add_outcome(
    counts: &mut BTreeMap<CompactMove, [u32; 3]>,
    mv: CompactMove,
    slot: usize,
) -> Result<(), QueryError> {
    let count = counts.entry(mv).or_default();
    count[slot] = count[slot].checked_add(1).ok_or_else(|| {
        crate::position::IndexError::Overflow("filtered position count exceeds u32".to_owned())
    })?;
    Ok(())
}

fn build_stats(
    key: PositionKey,
    counts: BTreeMap<CompactMove, [u32; 3]>,
) -> Result<PositionStats, QueryError> {
    let mut moves = Vec::with_capacity(counts.len());
    let (mut white_wins, mut draws, mut black_wins) = (0u32, 0u32, 0u32);
    for (compact_move, outcome_counts) in counts {
        let total_games = outcome_counts[0]
            .checked_add(outcome_counts[1])
            .and_then(|value| value.checked_add(outcome_counts[2]))
            .ok_or_else(|| {
                crate::position::IndexError::Overflow("filtered move count exceeds u32".to_owned())
            })?;
        white_wins = white_wins.checked_add(outcome_counts[0]).ok_or_else(|| {
            crate::position::IndexError::Overflow("filtered white wins exceed u32".to_owned())
        })?;
        draws = draws.checked_add(outcome_counts[1]).ok_or_else(|| {
            crate::position::IndexError::Overflow("filtered draws exceed u32".to_owned())
        })?;
        black_wins = black_wins.checked_add(outcome_counts[2]).ok_or_else(|| {
            crate::position::IndexError::Overflow("filtered black wins exceed u32".to_owned())
        })?;
        moves.push(MoveStats {
            compact_move,
            uci: compact_move.to_uci(),
            white_wins: outcome_counts[0],
            draws: outcome_counts[1],
            black_wins: outcome_counts[2],
            total_games,
        });
    }
    moves.sort_by_key(|entry| (std::cmp::Reverse(entry.total_games), entry.compact_move.0));
    let total_games = white_wins
        .checked_add(draws)
        .and_then(|value| value.checked_add(black_wins))
        .ok_or_else(|| {
            crate::position::IndexError::Overflow("filtered position total exceeds u32".to_owned())
        })?;
    Ok(PositionStats {
        position_key: key.to_hex(),
        total_games,
        white_wins,
        draws,
        black_wins,
        moves,
    })
}

fn empty_stats(key: PositionKey) -> PositionStats {
    PositionStats {
        position_key: key.to_hex(),
        ..PositionStats::default()
    }
}

fn check_work(work: usize, control: &SearchControl) -> Result<(), QueryError> {
    if control.expired() {
        return Err(QueryError::Deadline);
    }
    if work >= MAX_AGGREGATE_WORK {
        return Err(QueryError::WorkLimit);
    }
    Ok(())
}

fn query_error(error: rusqlite::Error, control: &SearchControl) -> QueryError {
    if control.expired() {
        QueryError::Deadline
    } else {
        QueryError::Sqlite(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::postings::{SearchPosting, SearchPostingsWriter};
    use chess_database_core::GameResult;

    #[test]
    fn repeated_positions_unknown_results_and_depth_match_cpi_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE games(ordinal INTEGER PRIMARY KEY, result TEXT NOT NULL);
                 INSERT INTO games VALUES (0, '1-0'), (1, '0-1'), (2, '*'), (3, '1/2-1/2'), (4, '0-1');",
            )
            .unwrap();
        let key = PositionKey(7);
        let move_a = CompactMove::new(12, 28, None);
        let move_b = CompactMove::new(11, 27, None);
        let output = directory.path().join("search.postings");
        let temporary = directory.path().join("temporary");
        let mut writer = SearchPostingsWriter::create(&output, 5, &temporary).unwrap();
        for posting in [
            SearchPosting {
                key,
                ordinal: 0,
                ply: 1,
                played: Some(move_a),
            },
            SearchPosting {
                key,
                ordinal: 0,
                ply: 3,
                played: Some(move_b),
            },
            SearchPosting {
                key,
                ordinal: 1,
                ply: 0,
                played: None,
            },
            SearchPosting {
                key,
                ordinal: 2,
                ply: 2,
                played: Some(move_b),
            },
            SearchPosting {
                key,
                ordinal: 3,
                ply: 5,
                played: Some(move_a),
            },
            SearchPosting {
                key,
                ordinal: 4,
                ply: 4,
                played: Some(move_b),
            },
        ] {
            writer.push(posting).unwrap();
        }
        writer.finish().unwrap();
        let postings = SearchPostingsReader::open(output).unwrap();
        let control = SearchControl::new(std::time::Duration::from_secs(5));
        let filters = GameFilters::default();
        let actual =
            filtered_position_stats(&connection, &postings, key, 5, &filters, &control).unwrap();

        let mut cpi_oracle = crate::position::ImmutablePositionIndex::new();
        cpi_oracle.record(key, move_a, GameResult::WhiteWins);
        cpi_oracle.record(key, move_b, GameResult::BlackWins);
        assert_eq!(actual, cpi_oracle.query(key).unwrap());
    }

    #[test]
    fn metadata_filter_excludes_nonmatching_known_results() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE games(ordinal INTEGER PRIMARY KEY, result TEXT NOT NULL, year INTEGER NOT NULL);
                 INSERT INTO games VALUES (0, '1-0', 2020), (1, '0-1', 2021);",
            )
            .unwrap();
        let key = PositionKey(9);
        let played = CompactMove::new(12, 28, None);
        let output = directory.path().join("search.postings");
        let temporary = directory.path().join("temporary");
        let mut writer = SearchPostingsWriter::create(&output, 2, &temporary).unwrap();
        writer
            .push(SearchPosting {
                key,
                ordinal: 0,
                ply: 0,
                played: Some(played),
            })
            .unwrap();
        writer
            .push(SearchPosting {
                key,
                ordinal: 1,
                ply: 0,
                played: Some(played),
            })
            .unwrap();
        writer.finish().unwrap();
        let postings = SearchPostingsReader::open(output).unwrap();
        let filters = GameFilters {
            year: super::super::query::Range {
                min: Some(2021),
                max: Some(2021),
            },
            ..GameFilters::default()
        };
        let stats = filtered_position_stats(
            &connection,
            &postings,
            key,
            40,
            &filters,
            &SearchControl::new(std::time::Duration::from_secs(5)),
        )
        .unwrap();
        assert_eq!(stats.total_games, 1);
        assert_eq!(stats.black_wins, 1);
    }

    #[test]
    fn large_position_group_supports_selective_seek_and_broad_merge() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE games(ordinal INTEGER PRIMARY KEY, result TEXT NOT NULL, year INTEGER NOT NULL);",
            )
            .unwrap();
        let key = PositionKey(11);
        let played = CompactMove::new(12, 28, None);
        let output = directory.path().join("search.postings");
        let temporary = directory.path().join("temporary");
        let mut writer = SearchPostingsWriter::create(&output, 600, &temporary).unwrap();
        for ordinal in 0..600u32 {
            writer
                .push(SearchPosting {
                    key,
                    ordinal,
                    ply: 0,
                    played: Some(played),
                })
                .unwrap();
            connection
                .execute(
                    "INSERT INTO games VALUES (?1, '1-0', ?2)",
                    (i64::from(ordinal), 2000 + i64::from(ordinal)),
                )
                .unwrap();
        }
        writer.finish().unwrap();
        let postings = SearchPostingsReader::open(output).unwrap();
        let control = SearchControl::new(std::time::Duration::from_secs(5));

        let selective = GameFilters {
            year: super::super::query::Range {
                min: Some(2599),
                max: Some(2599),
            },
            ..GameFilters::default()
        };
        let selective_stats =
            filtered_position_stats(&connection, &postings, key, 40, &selective, &control).unwrap();
        assert_eq!(selective_stats.total_games, 1);
        assert_eq!(selective_stats.white_wins, 1);

        let broad_stats = filtered_position_stats(
            &connection,
            &postings,
            key,
            40,
            &GameFilters::default(),
            &control,
        )
        .unwrap();
        assert_eq!(broad_stats.total_games, 600);
        assert_eq!(broad_stats.white_wins, 600);
    }

    #[test]
    fn projected_filters_match_sql_for_nulls_partial_dates_and_outcomes() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE games (ordinal INTEGER PRIMARY KEY, result TEXT, white_rating INTEGER,
                                 black_rating INTEGER, date_lower INTEGER, date_upper INTEGER, year INTEGER,
                                 average_rating_sum INTEGER);
             INSERT INTO games VALUES
                (0, '1-0', 2300, 2400, 20200101, 20201231, 2020, 4700),
                (1, '0-1', NULL, 2500, 20200201, 20200229, 2020, NULL),
                (2, '1/2-1/2', 2450, 2200, NULL, NULL, NULL, 4650),
                (3, '*', 2500, 2500, 20200301, 20200301, 2020, 5000);",
        ).unwrap();
        let output = directory.path().join("positions.postings");
        let mut writer =
            SearchPostingsWriter::create(&output, 4, directory.path().join("spool")).unwrap();
        let key = PositionKey(1);
        for ordinal in 0..4 {
            writer
                .push(SearchPosting {
                    key,
                    ordinal,
                    ply: 0,
                    played: Some(CompactMove::new(12, 28, None)),
                })
                .unwrap();
        }
        writer.finish().unwrap();
        let postings = SearchPostingsReader::open(output).unwrap();
        let projected = AggregateProjection::load(&connection, 4).unwrap().unwrap();
        let control = SearchControl::new(std::time::Duration::from_secs(5));
        let range = |min, max| Range { min, max };
        for filters in [
            GameFilters {
                both_ratings: range(Some(2200), None),
                ..Default::default()
            },
            GameFilters {
                either_rating: range(Some(2450), Some(2500)),
                ..Default::default()
            },
            GameFilters {
                average_rating: range(Some(2325), Some(2350)),
                ..Default::default()
            },
            GameFilters {
                year: range(Some(2020), None),
                date: range(Some(20200228), Some(20200228)),
                ..Default::default()
            },
            GameFilters {
                result: Some("0-1".into()),
                ..Default::default()
            },
            GameFilters {
                result: Some("*".into()),
                ..Default::default()
            },
        ] {
            let expected =
                filtered_position_stats(&connection, &postings, key, 30, &filters, &control)
                    .unwrap();
            assert_eq!(
                projected
                    .aggregate(&postings, key, 30, &filters, &control)
                    .unwrap(),
                Some(expected)
            );
        }
    }

    #[test]
    fn projected_common_filters_avoid_the_sql_merge_budget() {
        const GAMES: u32 = 130_000;
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE games(ordinal INTEGER PRIMARY KEY, result TEXT, white_rating INTEGER,
                                black_rating INTEGER, date_lower INTEGER, date_upper INTEGER,
                                event_norm TEXT);
             INSERT INTO games
             WITH RECURSIVE ids(n) AS (SELECT 0 UNION ALL SELECT n+1 FROM ids WHERE n+1 < {GAMES})
             SELECT n, '1-0', 2400, 2300, 20200101, 20201231, 'event' FROM ids;"
            ))
            .unwrap();
        let output = directory.path().join("positions.postings");
        let key = PositionKey(2);
        let mut writer =
            SearchPostingsWriter::create(&output, u64::from(GAMES), directory.path().join("spool"))
                .unwrap();
        for ordinal in 0..GAMES {
            writer
                .push(SearchPosting {
                    key,
                    ordinal,
                    ply: 0,
                    played: Some(CompactMove::new(12, 28, None)),
                })
                .unwrap();
        }
        writer.finish().unwrap();
        let postings = SearchPostingsReader::open(output).unwrap();
        let projected = AggregateProjection::load(&connection, GAMES)
            .unwrap()
            .unwrap();
        let control = SearchControl::new(std::time::Duration::from_secs(10));
        let simple = GameFilters {
            both_ratings: Range {
                min: Some(2200),
                max: None,
            },
            ..Default::default()
        };
        assert!(matches!(
            filtered_position_stats(&connection, &postings, key, 30, &simple, &control),
            Err(QueryError::WorkLimit)
        ));
        let actual = projected
            .aggregate(&postings, key, 30, &simple, &control)
            .unwrap()
            .unwrap();
        assert_eq!(actual.total_games, GAMES);
        assert_eq!(actual.white_wins, GAMES);
        let complex = GameFilters {
            event: Some("event".into()),
            ..simple.clone()
        };
        assert!(projected
            .aggregate(&postings, key, 30, &complex, &control)
            .unwrap()
            .is_none());
        assert!(matches!(
            filtered_position_stats(&connection, &postings, key, 30, &complex, &control),
            Err(QueryError::WorkLimit)
        ));
        control.cancel();
        assert!(matches!(
            projected.aggregate(&postings, key, 30, &simple, &control),
            Err(QueryError::Deadline)
        ));
    }
}
