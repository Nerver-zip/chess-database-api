//! Exact filtered opening statistics reconstructed from position postings.

use super::control::SearchControl;
use super::postings::{SearchPosting, SearchPostingsReader};
use super::query::{GameFilters, QueryError, SearchCondition};
use crate::position::{MoveStats, PositionStats};
use opening_explorer_core::{CompactMove, PositionKey};
use rusqlite::{params_from_iter, types::Value, Connection, OptionalExtension};
use std::collections::BTreeMap;

const SMALL_POSITION_THRESHOLD: usize = 512;
const MAX_AGGREGATE_WORK: usize = 250_000;

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
    let count = counts.entry(mv).or_default();
    let slot = match result {
        "1-0" => 0,
        "1/2-1/2" => 1,
        "0-1" => 2,
        "*" => return Ok(()),
        other => {
            return Err(crate::position::IndexError::InvalidFormat(format!(
                "unsupported game result `{other}` in metadata catalog"
            ))
            .into());
        }
    };
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
    use opening_explorer_core::GameResult;

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
}
