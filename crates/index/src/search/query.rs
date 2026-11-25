//! Queries over the derived metadata and occurrence indexes.

use super::control::SearchControl;
use super::postings::{SearchPosting, SearchPostingsReader};
use crate::position::IndexError;
use opening_explorer_core::{CompactMove, GameSourceAttribution, PositionKey};
use rusqlite::{params_from_iter, types::Value, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

const SUMMARY_COLUMNS: &str =
    "game_id, white_name, black_name, white_rating, black_rating, result, date_original,
     event_name, site_name, round_name, eco, time_control, time_control_category, plies,
     starting_fen, source_id, broadcast_url, white_player_id, black_player_id, event_id,
     white_fide_id, black_fide_id";

const SMALL_POSITION_THRESHOLD: usize = 512;
const MAX_POSITION_WORK: usize = 250_000;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameSort {
    #[default]
    Ordinal,
    DateAsc,
    DateDesc,
    WhiteRating,
    BlackRating,
    AverageRating,
    GameLength,
}

impl GameSort {
    fn expression(self) -> &'static str {
        match self {
            Self::Ordinal => "ordinal",
            Self::DateAsc => "coalesce(date_lower,100000000)",
            Self::DateDesc => "coalesce(date_lower,-1)",
            Self::WhiteRating => "coalesce(white_rating,-1)",
            Self::BlackRating => "coalesce(black_rating,-1)",
            Self::AverageRating => "coalesce(average_rating_sum,-1)",
            Self::GameLength => "plies",
        }
    }

    fn ascending(self) -> bool {
        matches!(self, Self::Ordinal | Self::DateAsc)
    }

    fn comparator(self) -> &'static str {
        if self.ascending() {
            ">"
        } else {
            "<"
        }
    }

    fn order(self) -> String {
        let direction = if self.ascending() { "ASC" } else { "DESC" };
        format!("{} {direction}, ordinal {direction}", self.expression())
    }

    fn compare(self, left: PageKey, right: PageKey) -> std::cmp::Ordering {
        let order = (left.value, left.ordinal).cmp(&(right.value, right.ordinal));
        if self.ascending() {
            order
        } else {
            order.reverse()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerSide {
    #[default]
    Either,
    White,
    Black,
}

#[derive(Clone, Debug, Serialize)]
pub enum PlayerSelector {
    Name(String),
    Id(u64),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Range {
    pub min: Option<u32>,
    pub max: Option<u32>,
}

impl Range {
    fn constrain(self, predicates: &mut Predicates, column: &str, multiplier: u32) {
        if let Some(min) = self.min {
            predicates.add(
                format!("{column} >= ?"),
                i64::from(min) * i64::from(multiplier),
            );
        }
        if let Some(max) = self.max {
            predicates.add(
                format!("{column} <= ?"),
                i64::from(max) * i64::from(multiplier),
            );
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GameFilters {
    pub player: Option<PlayerSelector>,
    pub player_side: PlayerSide,
    pub opponent: Option<PlayerSelector>,
    pub white: Option<PlayerSelector>,
    pub black: Option<PlayerSelector>,
    pub white_rating: Range,
    pub black_rating: Range,
    pub player_rating: Range,
    pub both_ratings: Range,
    pub either_rating: Range,
    pub average_rating: Range,
    pub year: Range,
    pub date: Range,
    pub result: Option<String>,
    pub event_id: Option<u64>,
    pub event: Option<String>,
    pub site: Option<String>,
    pub eco: Option<String>,
    pub time_control_category: Option<String>,
    pub plies: Range,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PositionFilter {
    pub key: PositionKey,
    pub continuation: Option<CompactMove>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PositionMatch {
    pub ply: u16,
    pub continuation: Option<CompactMove>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageKey {
    pub value: i64,
    pub ordinal: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct GameSummary {
    pub id: String,
    pub white: String,
    pub black: String,
    pub white_rating: Option<u16>,
    pub black_rating: Option<u16>,
    pub result: String,
    pub date: Option<String>,
    pub event: Option<String>,
    pub site: Option<String>,
    pub round: Option<String>,
    pub eco: Option<String>,
    pub time_control: Option<String>,
    pub time_control_category: String,
    pub plies: u16,
    pub starting_fen: Option<String>,
    pub source_id: Option<String>,
    pub broadcast_url: Option<String>,
    pub white_player_id: String,
    pub black_player_id: String,
    pub event_id: Option<String>,
    pub white_fide_id: Option<String>,
    pub black_fide_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_match: Option<GamePositionMatch>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GamePositionMatch {
    pub ply: u16,
    pub continuation: Option<String>,
}

#[derive(Debug)]
pub struct GamePage {
    pub games: Vec<GameSummary>,
    pub after: Option<PageKey>,
    pub total: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("Metadata catalog: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Position postings: {0}")]
    Index(#[from] IndexError),
    #[error("Stored source attribution is invalid")]
    Attribution(#[from] serde_json::Error),
    #[error("Query work limit exceeded")]
    WorkLimit,
    #[error("Query deadline exceeded")]
    Deadline,
}

#[derive(Default)]
struct Predicates {
    sql: Vec<String>,
    values: Vec<Value>,
}

impl Predicates {
    fn add(&mut self, sql: impl Into<String>, value: impl Into<Value>) {
        self.sql.push(sql.into());
        self.values.push(value.into());
    }

    fn include(&mut self, predicates: Self) {
        self.sql.extend(predicates.sql);
        self.values.extend(predicates.values);
    }

    fn and(mut self, other: Self) -> Self {
        self.include(other);
        self
    }

    fn or(mut self, other: Self) -> Self {
        let sql = format!(
            "(({}) OR ({}))",
            self.sql.join(" AND "),
            other.sql.join(" AND ")
        );
        self.values.extend(other.values);
        self.sql = vec![sql];
        self
    }

    fn clause(&self) -> String {
        if self.sql.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.sql.join(" AND "))
        }
    }
}

fn player_predicate(selector: &PlayerSelector, side: &'static str, out: &mut Predicates) {
    match selector {
        PlayerSelector::Name(name) => out.add(format!("{side}_name_norm = ?"), name.clone()),
        PlayerSelector::Id(id) => out.add(format!("{side}_player_id = ?"), *id as i64),
    }
}

pub struct SearchCondition {
    pub sql: String,
    pub values: Vec<Value>,
}

impl GameFilters {
    pub fn condition(&self) -> SearchCondition {
        let predicates = self.predicates();
        SearchCondition {
            sql: if predicates.sql.is_empty() {
                "1".into()
            } else {
                predicates.sql.join(" AND ")
            },
            values: predicates.values,
        }
    }

    fn predicates(&self) -> Predicates {
        let mut out = Predicates::default();
        if let Some(player) = &self.player {
            let match_side = |side, other| {
                let mut match_side = Predicates::default();
                player_predicate(player, side, &mut match_side);
                if let Some(opponent) = &self.opponent {
                    player_predicate(opponent, other, &mut match_side);
                }
                self.player_rating
                    .constrain(&mut match_side, &format!("{side}_rating"), 1);
                match_side
            };
            let selected = match self.player_side {
                PlayerSide::White => match_side("white", "black"),
                PlayerSide::Black => match_side("black", "white"),
                PlayerSide::Either => match_side("white", "black").or(match_side("black", "white")),
            };
            out.include(selected);
        }
        if let Some(white) = &self.white {
            player_predicate(white, "white", &mut out);
        }
        if let Some(black) = &self.black {
            player_predicate(black, "black", &mut out);
        }
        self.white_rating.constrain(&mut out, "white_rating", 1);
        self.black_rating.constrain(&mut out, "black_rating", 1);
        self.both_ratings.constrain(&mut out, "white_rating", 1);
        self.both_ratings.constrain(&mut out, "black_rating", 1);
        if self.either_rating != Range::default() {
            let mut white = Predicates::default();
            let mut black = Predicates::default();
            self.either_rating.constrain(&mut white, "white_rating", 1);
            self.either_rating.constrain(&mut black, "black_rating", 1);
            out.include(white.or(black));
        }
        self.average_rating
            .constrain(&mut out, "average_rating_sum", 2);
        self.year.constrain(&mut out, "year", 1);
        self.plies.constrain(&mut out, "plies", 1);
        if let Some(min) = self.date.min {
            out.add("date_upper >= ?", i64::from(min));
        }
        if let Some(max) = self.date.max {
            out.add("date_lower <= ?", i64::from(max));
        }
        if let Some(id) = self.event_id {
            out.add("event_id = ?", id as i64);
        }
        for (value, sql) in [
            (&self.result, "result = ?"),
            (&self.event, "event_norm = ?"),
            (&self.site, "site_norm = ?"),
            (&self.eco, "eco = ?"),
            (&self.time_control_category, "time_control_category = ?"),
        ] {
            if let Some(value) = value {
                out.add(sql, value.clone());
            }
        }
        out
    }
}

fn map_summary(row: &Row<'_>, start: usize) -> rusqlite::Result<GameSummary> {
    let id: [u8; 8] = row.get(start)?;
    let col = |offset| start + offset;
    Ok(GameSummary {
        id: u64::from_be_bytes(id).to_string(),
        white: row.get(col(1))?,
        black: row.get(col(2))?,
        white_rating: row.get(col(3))?,
        black_rating: row.get(col(4))?,
        result: row.get(col(5))?,
        date: row.get(col(6))?,
        event: row.get(col(7))?,
        site: row.get(col(8))?,
        round: row.get(col(9))?,
        eco: row.get(col(10))?,
        time_control: row.get(col(11))?,
        time_control_category: row.get(col(12))?,
        plies: row.get(col(13))?,
        starting_fen: row.get(col(14))?,
        source_id: row.get(col(15))?,
        broadcast_url: row.get(col(16))?,
        white_player_id: row.get::<_, u64>(col(17))?.to_string(),
        black_player_id: row.get::<_, u64>(col(18))?.to_string(),
        event_id: row.get::<_, Option<u64>>(col(19))?.map(|id| id.to_string()),
        white_fide_id: row.get(col(20))?,
        black_fide_id: row.get(col(21))?,
        position_match: None,
    })
}

pub fn game_metadata(connection: &Connection, id: u64) -> Result<Option<GameSummary>, QueryError> {
    Ok(connection
        .query_row(
            &format!("SELECT {SUMMARY_COLUMNS} FROM games WHERE game_id = ?"),
            [id.to_be_bytes()],
            |row| map_summary(row, 0),
        )
        .optional()?)
}

pub fn source_attributions(
    connection: &Connection,
    id: u64,
) -> Result<Vec<GameSourceAttribution>, QueryError> {
    let row = connection
        .query_row(
            "SELECT attributions_json FROM source_attributions WHERE game_id = ?",
            [id.to_be_bytes()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match row {
        Some(json) => Ok(serde_json::from_str(&json)?),
        None => Ok(Vec::new()),
    }
}

fn matching_posting(
    postings: &SearchPostingsReader,
    position: PositionFilter,
    ordinal: u32,
    control: &SearchControl,
) -> Result<Option<SearchPosting>, QueryError> {
    let Some(mut group) = postings.seek(position.key, ordinal)? else {
        return Ok(None);
    };
    for posting in &mut group {
        if control.expired() {
            return Err(QueryError::Deadline);
        }
        let posting = posting?;
        if posting.ordinal > ordinal {
            break;
        }
        if position
            .continuation
            .is_none_or(|played| Some(played) == posting.played)
        {
            return Ok(Some(posting));
        }
    }
    Ok(None)
}

fn apply_posting(summary: &mut GameSummary, posting: SearchPosting) {
    summary.position_match = Some(GamePositionMatch {
        ply: posting.ply,
        continuation: posting.played.map(|played| played.to_uci()),
    });
}

#[derive(Clone, Copy, Debug)]
pub struct GamePageRequest<'a> {
    pub filters: &'a GameFilters,
    pub position: Option<PositionFilter>,
    pub sort: GameSort,
    pub after: Option<PageKey>,
    pub page_size: usize,
    pub exact_count: bool,
}

pub fn search_games(
    connection: &Connection,
    postings: &SearchPostingsReader,
    request: GamePageRequest<'_>,
    control: &SearchControl,
) -> Result<GamePage, QueryError> {
    control.interrupt_sqlite(connection);
    let predicates = request.filters.predicates();
    let page = match request.position {
        Some(position) => {
            let Some(group) = postings.get(position.key)? else {
                return Ok(GamePage {
                    games: Vec::new(),
                    after: None,
                    total: request.exact_count.then_some(0),
                });
            };
            let small_group =
                group.take(SMALL_POSITION_THRESHOLD + 1).count() <= SMALL_POSITION_THRESHOLD;
            if small_group || (predicates.sql.is_empty() && request.sort == GameSort::Ordinal) {
                position_first(connection, postings, request, position, predicates, control)?
            } else {
                metadata_first(connection, postings, request, predicates, control)?
            }
        }
        None => metadata_first(connection, postings, request, predicates, control)?,
    };
    Ok(page)
}

fn make_page(mut rows: Vec<(PageKey, GameSummary)>, limit: usize, total: Option<u64>) -> GamePage {
    let after = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| row.0)
    } else {
        None
    };
    GamePage {
        games: rows.into_iter().map(|(_, row)| row).collect(),
        after,
        total,
    }
}

fn position_first(
    connection: &Connection,
    postings: &SearchPostingsReader,
    request: GamePageRequest<'_>,
    position: PositionFilter,
    predicates: Predicates,
    control: &SearchControl,
) -> Result<GamePage, QueryError> {
    let sort = request.sort.expression();
    let sql = format!(
        "SELECT ordinal, {sort}, {SUMMARY_COLUMNS} FROM games WHERE ordinal = ?{}",
        predicates
            .sql
            .iter()
            .map(|clause| format!(" AND ({clause})"))
            .collect::<String>()
    );
    let mut statement = connection.prepare(&sql)?;
    let mut values = vec![Value::Integer(0)];
    values.extend(predicates.values);
    let begin = if request.sort == GameSort::Ordinal && !request.exact_count {
        request
            .after
            .map_or(0, |after| after.ordinal.saturating_add(1))
    } else {
        0
    };
    let Some(iter) = postings.seek(position.key, begin)? else {
        return Ok(make_page(
            Vec::new(),
            request.page_size,
            request.exact_count.then_some(0),
        ));
    };
    let mut accepted = None;
    let mut rows = Vec::new();
    let mut total = 0;
    for (work, posting) in iter.enumerate() {
        if work >= MAX_POSITION_WORK {
            return Err(QueryError::WorkLimit);
        }
        if work.is_multiple_of(256) && control.expired() {
            return Err(QueryError::Deadline);
        }
        let posting = posting?;
        if accepted == Some(posting.ordinal)
            || position
                .continuation
                .is_some_and(|played| posting.played != Some(played))
        {
            continue;
        }
        accepted = Some(posting.ordinal);
        values[0] = Value::Integer(i64::from(posting.ordinal));
        let found = statement
            .query_row(params_from_iter(&values), |row| {
                Ok((
                    PageKey {
                        ordinal: row.get(0)?,
                        value: row.get(1)?,
                    },
                    map_summary(row, 2)?,
                ))
            })
            .optional()?;
        if let Some((key, mut summary)) = found {
            total += 1;
            if request
                .after
                .is_none_or(|after| request.sort.compare(key, after).is_gt())
                && (request.sort != GameSort::Ordinal || rows.len() <= request.page_size)
            {
                apply_posting(&mut summary, posting);
                rows.push((key, summary));
            }
            if request.sort == GameSort::Ordinal
                && !request.exact_count
                && rows.len() > request.page_size
            {
                break;
            }
        }
    }
    if request.sort != GameSort::Ordinal {
        rows.sort_unstable_by(|lhs, rhs| request.sort.compare(lhs.0, rhs.0));
    }
    Ok(make_page(
        rows,
        request.page_size,
        request.exact_count.then_some(total),
    ))
}

fn metadata_first(
    connection: &Connection,
    postings: &SearchPostingsReader,
    request: GamePageRequest<'_>,
    predicates: Predicates,
    control: &SearchControl,
) -> Result<GamePage, QueryError> {
    let mut total = None;
    if request.exact_count && request.position.is_none() {
        total = Some(connection.query_row(
            &format!("SELECT count(*) FROM games{}", predicates.clause()),
            params_from_iter(&predicates.values),
            |row| row.get(0),
        )?);
    }

    let sort = request.sort.expression();
    let mut paging = Predicates::default();
    if let Some(after) = request
        .after
        .filter(|_| !request.exact_count || request.position.is_none())
    {
        let cmp = request.sort.comparator();
        if request.sort == GameSort::Ordinal {
            paging.add(format!("ordinal {cmp} ?"), i64::from(after.ordinal));
        } else {
            paging.add(format!("({sort}, ordinal) {cmp} (?, ?)"), after.value);
            paging.values.push(Value::Integer(i64::from(after.ordinal)));
        }
    }
    let predicates = predicates.and(paging);
    let mut sql = format!(
        "SELECT ordinal, {sort}, {SUMMARY_COLUMNS} FROM games{} ORDER BY {}",
        predicates.clause(),
        request.sort.order()
    );
    if request.position.is_none() {
        sql.push_str(&format!(" LIMIT {}", request.page_size.saturating_add(1)));
    }
    let mut statement = connection.prepare(&sql)?;
    let mut candidates = statement.query(params_from_iter(&predicates.values))?;
    let mut rows = Vec::new();
    let mut qualifying = 0;
    let mut work = 0usize;
    while let Some(row) = candidates.next()? {
        if work >= MAX_POSITION_WORK {
            return Err(QueryError::WorkLimit);
        }
        if work.is_multiple_of(256) && control.expired() {
            return Err(QueryError::Deadline);
        }
        work += 1;
        let key = PageKey {
            ordinal: row.get(0)?,
            value: row.get(1)?,
        };
        let mut posting = None;
        if let Some(position) = request.position {
            posting = matching_posting(postings, position, key.ordinal, control)?;
            if posting.is_none() {
                continue;
            }
        }
        qualifying += 1;
        if rows.len() <= request.page_size
            && request
                .after
                .is_none_or(|after| request.sort.compare(key, after).is_gt())
        {
            let mut summary = map_summary(row, 2)?;
            if let Some(posting) = posting {
                apply_posting(&mut summary, posting);
            }
            rows.push((key, summary));
        }
        if (!request.exact_count || request.position.is_none()) && rows.len() > request.page_size {
            break;
        }
    }
    if request.exact_count && request.position.is_some() {
        total = Some(qualifying);
    }
    Ok(make_page(rows, request.page_size, total))
}
