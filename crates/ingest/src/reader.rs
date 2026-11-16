use opening_explorer_core::{
    compute_stable_game_id, Collection, CompactMove, GameMetadata, GameResult,
    GameSourceAttribution, IngestionCompletionStatus, IngestionStats, ParsedGame,
};
use opening_explorer_curate::policy::{
    is_aborted_or_infraction_termination, GigabaseHeaderMetadata, GigabaseOtbPolicy,
};
use pgn_reader::{RawTag, Reader, SanPlus, Visitor};
use shakmaty::fen::Fen;
use shakmaty::{CastlingMode, Chess, Move, Position, Role, Square};
use std::collections::HashMap;
use std::io::Read;
use std::ops::ControlFlow;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum IngestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Zstd decompression error: {0}")]
    Zstd(String),
    #[error("Move replay error at ply {ply}: {reason}")]
    IllegalMove { ply: usize, reason: String },
    #[error("Unsupported chess variant: {0}")]
    UnsupportedVariant(String),
    #[error("Non-standard starting position: {0}")]
    NonStandardStartingPosition(String),
    #[error("Malformed PGN: {0}")]
    MalformedPgn(String),
    #[error("Rejected by curation policy: {0}")]
    RejectedByPolicy(String),
}

#[derive(Default, Debug)]
pub struct GameTags {
    pub tag_count: usize,
    pub malformed_tag: Option<String>,
    pub header_values: HashMap<String, String>,
    pub conflicting_header_fields: Vec<String>,
    pub white: String,
    pub black: String,
    pub white_elo: Option<u16>,
    pub black_elo: Option<u16>,
    pub white_elo_raw: Option<String>,
    pub black_elo_raw: Option<String>,
    pub date: Option<String>,
    pub utc_date: Option<String>,
    pub event: Option<String>,
    pub site: Option<String>,
    pub round: Option<String>,
    pub result: GameResult,
    pub eco: Option<String>,
    pub time_control: Option<String>,
    pub aborted_or_infraction: bool,
    pub variant: Option<String>,
    pub fen_start: Option<String>,
    pub game_url: Option<String>,
    pub broadcast_url: Option<String>,
    pub game_id_tag: Option<String>,
    pub source_tag: Option<String>,
    pub source_quality: Option<String>,
    pub white_fide_id: Option<String>,
    pub black_fide_id: Option<String>,
    pub white_identity_evidence: Option<String>,
    pub black_identity_evidence: Option<String>,
    pub mode: Option<String>,
    pub white_title: Option<String>,
    pub black_title: Option<String>,
}

impl GameTags {
    fn source_attribution(&self) -> Option<GameSourceAttribution> {
        let attribution = GameSourceAttribution {
            source_tag: self.source_tag.clone(),
            source_quality: self.source_quality.clone(),
            white_fide_id: self.white_fide_id.clone(),
            black_fide_id: self.black_fide_id.clone(),
            white_identity_evidence: self.white_identity_evidence.clone(),
            black_identity_evidence: self.black_identity_evidence.clone(),
            mode: self.mode.clone(),
            white_title: self.white_title.clone(),
            black_title: self.black_title.clone(),
        };
        (attribution.source_tag.is_some()
            || attribution.source_quality.is_some()
            || attribution.white_fide_id.is_some()
            || attribution.black_fide_id.is_some()
            || attribution.white_identity_evidence.is_some()
            || attribution.black_identity_evidence.is_some()
            || attribution.mode.is_some()
            || attribution.white_title.is_some()
            || attribution.black_title.is_some())
        .then_some(attribution)
    }

    fn gigabase_header_metadata<'a>(
        &'a self,
        resolved_date: Option<&'a str>,
    ) -> GigabaseHeaderMetadata<'a> {
        GigabaseHeaderMetadata {
            source_tag: self.source_tag.as_deref(),
            white_name: Some(&self.white),
            black_name: Some(&self.black),
            white_elo: self.white_elo_raw.as_deref(),
            black_elo: self.black_elo_raw.as_deref(),
            white_title: self.white_title.as_deref(),
            black_title: self.black_title.as_deref(),
            white_identity_evidence: self.white_identity_evidence.as_deref(),
            black_identity_evidence: self.black_identity_evidence.as_deref(),
            white_fide_id: self.white_fide_id.as_deref(),
            black_fide_id: self.black_fide_id.as_deref(),
            mode: self.mode.as_deref(),
            date: resolved_date
                .or(self.date.as_deref())
                .or(self.utc_date.as_deref()),
            event: self.event.as_deref(),
            site: self.site.as_deref(),
            conflicting_metadata_fields: &self.conflicting_header_fields,
        }
    }
}

/// Computes a deterministic completeness score for PGN date strings:
/// - 3: Complete valid `YYYY.MM.DD` calendar date (no `?` placeholders)
/// - 2: Partial year+month known (`YYYY.MM.??`)
/// - 1: Partial year known (`YYYY.??.??`) or other non-placeholder date string
/// - 0: Unknown or placeholder-only (`""`, `"?"`, `"????.??.??"`)
pub fn date_completeness_rank(d: &str) -> u8 {
    let trimmed = d.trim();
    if trimmed.is_empty() || trimmed == "?" || !trimmed.chars().any(|c| c.is_ascii_alphanumeric()) {
        return 0;
    }

    let parts: Vec<&str> = trimmed.split(['.', '-']).collect();
    if parts.len() == 3 {
        let y_ok = parts[0].len() == 4
            && !parts[0].contains('?')
            && parts[0]
                .parse::<u16>()
                .is_ok_and(|y| (1000..=2999).contains(&y));
        let m_ok = parts[1].len() == 2
            && !parts[1].contains('?')
            && parts[1].parse::<u8>().is_ok_and(|m| (1..=12).contains(&m));
        let d_ok = parts[2].len() == 2
            && !parts[2].contains('?')
            && parts[2]
                .parse::<u8>()
                .is_ok_and(|day| (1..=31).contains(&day));

        if y_ok && m_ok && d_ok && !trimmed.contains('?') {
            return 3;
        }
        if y_ok && m_ok {
            return 2;
        }
        if y_ok {
            return 1;
        }
    }

    1
}

/// Resolves `Date` and `UTCDate` deterministically:
/// - Prefers the tag with strictly higher completeness rank (never overwrites a valid date with `????.??.??` or `YYYY.??.??`).
/// - When both tags have equal completeness rank (e.g., both are complete `YYYY.MM.DD`), prefers `Date` (canonical PGN Seven Tag Roster date) over `UTCDate`.
/// - Returns `None` if neither tag carries non-placeholder date information.
pub fn resolve_canonical_date(date: Option<&str>, utc_date: Option<&str>) -> Option<String> {
    let r_date = date.map(date_completeness_rank).unwrap_or(0);
    let r_utc = utc_date.map(date_completeness_rank).unwrap_or(0);

    if r_date == 0 && r_utc == 0 {
        None
    } else if r_utc > r_date {
        utc_date.map(|s| s.trim().to_string())
    } else {
        date.map(|s| s.trim().to_string())
    }
}

/// Classifies an HTTP(S) URL found in `Site` as either game-specific or tournament/broadcast-wide.
/// Returns `Some(true)` if game-specific, `Some(false)` if tournament/broadcast-wide, or `None` if not an HTTP(S) URL.
pub fn classify_http_url(url: &str) -> Option<bool> {
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))?;

    let mut segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Some(false);
    }
    // Remove host
    segments.remove(0);
    if segments.is_empty() {
        return Some(false);
    }

    match segments[0] {
        "broadcast" => {
            // Lichess BroadcastURL: /broadcast/<slug>/<round-slug>/<round-id> (4 segments)
            // Lichess GameURL:      /broadcast/<slug>/<round-slug>/<round-id>/<game-id> (5 segments)
            Some(segments.len() >= 5)
        }
        "tournament" | "tournaments" | "swiss" | "event" | "events" => Some(false),
        _ => Some(true),
    }
}

/// Resolves `(source_id, broadcast_url)` from `Site`, `GameURL`, `BroadcastURL`, and `GameId` tags.
/// Ensures a tournament or broadcast URL is never conflated with a unique game-specific `source_id`.
pub fn resolve_provenance(
    site: Option<&str>,
    game_url: Option<&str>,
    broadcast_url: Option<&str>,
    game_id_tag: Option<&str>,
) -> (Option<String>, Option<String>) {
    let resolved_broadcast = if let Some(b) = broadcast_url {
        Some(b.trim().to_string())
    } else if let Some(s) = site {
        if classify_http_url(s) == Some(false) {
            Some(s.trim().to_string())
        } else {
            None
        }
    } else {
        None
    };

    let resolved_source_id = if let Some(g) = game_url {
        Some(g.trim().to_string())
    } else if let Some(gid) = game_id_tag {
        Some(gid.trim().to_string())
    } else if let Some(s) = site {
        if classify_http_url(s) == Some(true) && resolved_broadcast.as_deref() != Some(s.trim()) {
            Some(s.trim().to_string())
        } else {
            None
        }
    } else {
        None
    };

    (resolved_source_id, resolved_broadcast)
}

pub struct GameMovetext {
    pub tags: GameTags,
    pub board: Chess,
    pub moves: Vec<CompactMove>,
    pub legal_replay_failed: bool,
    pub failure_ply: usize,
    pub failure_reason: String,
    pub ply_count: usize,
}

pub struct ShakmatyGameVisitor {
    pub collection: Collection,
    pub next_id: u64,
    pub early_policy_filter: bool,
    gigabase_policy: Option<GigabaseOtbPolicy>,
}

impl ShakmatyGameVisitor {
    pub fn new(collection: Collection, starting_id: u64) -> Self {
        ShakmatyGameVisitor {
            collection,
            next_id: starting_id,
            early_policy_filter: false,
            gigabase_policy: None,
        }
    }

    pub fn with_early_policy_filter(mut self, enabled: bool) -> Self {
        self.early_policy_filter = enabled;
        self
    }

    pub fn with_gigabase_policy(mut self, policy: GigabaseOtbPolicy) -> Self {
        self.gigabase_policy = Some(policy);
        self
    }
}

impl Visitor for ShakmatyGameVisitor {
    type Tags = GameTags;
    type Movetext = GameMovetext;
    type Output = Result<ParsedGame, IngestError>;

    fn begin_tags(&mut self) -> ControlFlow<Self::Output, Self::Tags> {
        ControlFlow::Continue(GameTags {
            result: GameResult::Star,
            ..Default::default()
        })
    }

    fn tag(
        &mut self,
        tags: &mut Self::Tags,
        name: &[u8],
        value: RawTag<'_>,
    ) -> ControlFlow<Self::Output> {
        tags.tag_count += 1;
        if name.is_empty() || !name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_') {
            tags.malformed_tag = Some(format!(
                "Invalid PGN tag name: '{}'",
                String::from_utf8_lossy(name)
            ));
            return ControlFlow::Continue(());
        }

        let val_cow = value.decode();
        let val_str = String::from_utf8_lossy(&val_cow);
        let trimmed = val_str.trim();
        let tag_name = String::from_utf8_lossy(name).into_owned();
        match tags.header_values.get(&tag_name) {
            Some(previous) if previous != trimmed => {
                if !tags.conflicting_header_fields.contains(&tag_name) {
                    tags.conflicting_header_fields.push(tag_name);
                }
            }
            Some(_) => {}
            None => {
                tags.header_values.insert(tag_name, trimmed.to_owned());
            }
        }

        match name {
            b"White" => tags.white = trimmed.to_string(),
            b"Black" => tags.black = trimmed.to_string(),
            b"WhiteElo" => {
                tags.white_elo_raw = Some(trimmed.to_owned());
                tags.white_elo = trimmed.parse::<u16>().ok();
            }
            b"BlackElo" => {
                tags.black_elo_raw = Some(trimmed.to_owned());
                tags.black_elo = trimmed.parse::<u16>().ok();
            }
            b"Date" => {
                if !trimmed.is_empty() {
                    tags.date = Some(trimmed.to_string());
                }
            }
            b"UTCDate" => {
                if !trimmed.is_empty() {
                    tags.utc_date = Some(trimmed.to_string());
                }
            }
            b"Event" => {
                if !trimmed.is_empty() && trimmed != "?" {
                    tags.event = Some(trimmed.to_string());
                }
            }
            b"Site" => {
                if !trimmed.is_empty() && trimmed != "?" {
                    tags.site = Some(trimmed.to_string());
                }
            }
            b"GameURL" => {
                if !trimmed.is_empty() && trimmed != "?" {
                    tags.game_url = Some(trimmed.to_string());
                }
            }
            b"BroadcastURL" => {
                if !trimmed.is_empty() && trimmed != "?" {
                    tags.broadcast_url = Some(trimmed.to_string());
                }
            }
            b"GameId" => {
                if !trimmed.is_empty() && trimmed != "?" {
                    tags.game_id_tag = Some(trimmed.to_string());
                }
            }
            b"Source" => tags.source_tag = Some(trimmed.to_string()),
            b"SourceQuality" => tags.source_quality = Some(trimmed.to_string()),
            b"WhiteFideId" => tags.white_fide_id = Some(trimmed.to_string()),
            b"BlackFideId" => tags.black_fide_id = Some(trimmed.to_string()),
            b"IdentityEvidenceWhite" => tags.white_identity_evidence = Some(trimmed.to_string()),
            b"IdentityEvidenceBlack" => tags.black_identity_evidence = Some(trimmed.to_string()),
            b"Mode" => tags.mode = Some(trimmed.to_string()),
            b"WhiteTitle" => tags.white_title = Some(trimmed.to_string()),
            b"BlackTitle" => tags.black_title = Some(trimmed.to_string()),
            b"Round" => {
                if !trimmed.is_empty() && trimmed != "?" {
                    tags.round = Some(trimmed.to_string());
                }
            }
            b"Result" => tags.result = GameResult::from_str_lenient(trimmed),
            b"ECO" => {
                if !trimmed.is_empty() && trimmed != "?" {
                    tags.eco = Some(trimmed.to_string());
                }
            }
            b"TimeControl" => {
                if !trimmed.is_empty() && trimmed != "?" && trimmed != "-" {
                    tags.time_control = Some(trimmed.to_string());
                }
            }
            b"Termination" => {
                tags.aborted_or_infraction |= is_aborted_or_infraction_termination(trimmed);
            }
            b"Variant" => tags.variant = Some(trimmed.to_string()),
            b"FEN" => tags.fen_start = Some(trimmed.to_string()),
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: Self::Tags) -> ControlFlow<Self::Output, Self::Movetext> {
        if let Some(err_msg) = tags.malformed_tag {
            return ControlFlow::Break(Err(IngestError::MalformedPgn(err_msg)));
        }
        if tags.tag_count == 0 {
            return ControlFlow::Break(Err(IngestError::MalformedPgn(
                "Missing PGN header tags".into(),
            )));
        }

        if let Some(policy) = &self.gigabase_policy {
            let qualification = if policy.is_r3() {
                let resolved_date =
                    resolve_canonical_date(tags.date.as_deref(), tags.utc_date.as_deref());
                policy
                    .evaluate_r3_headers(&tags.gigabase_header_metadata(resolved_date.as_deref()))
                    .map(|_| ())
            } else {
                policy.evaluate_source_metadata(
                    tags.source_attribution().as_ref(),
                    tags.date.as_deref(),
                )
            };
            if let Err(reason) = qualification {
                return ControlFlow::Break(Err(IngestError::RejectedByPolicy(format!(
                    "gigabase:{}",
                    reason.category()
                ))));
            }
        }

        if self.early_policy_filter && tags.aborted_or_infraction {
            return ControlFlow::Break(Err(IngestError::RejectedByPolicy(
                "Aborted or rules infraction".into(),
            )));
        }

        if let Some(v) = &tags.variant {
            let v_lower = v.to_ascii_lowercase();
            if v_lower != "standard" && v_lower != "chess" {
                return ControlFlow::Break(Err(IngestError::UnsupportedVariant(v.clone())));
            }
        }

        let mut board = Chess::default();
        if let Some(fen) = &tags.fen_start {
            match fen.parse::<Fen>() {
                Ok(parsed_fen) => {
                    let pos_res: Result<Chess, _> =
                        parsed_fen.into_position(CastlingMode::Standard);
                    match pos_res {
                        Ok(custom_pos) => {
                            if custom_pos != Chess::default() {
                                return ControlFlow::Break(Err(
                                    IngestError::NonStandardStartingPosition(fen.clone()),
                                ));
                            }
                            board = custom_pos;
                        }
                        Err(_) => {
                            return ControlFlow::Break(Err(
                                IngestError::NonStandardStartingPosition(fen.clone()),
                            ));
                        }
                    }
                }
                Err(_) => {
                    return ControlFlow::Break(Err(IngestError::NonStandardStartingPosition(
                        fen.clone(),
                    )));
                }
            }
        }

        if self.early_policy_filter {
            match self.collection {
                Collection::EliteOnline => {
                    if let Some(ev) = &tags.event {
                        let ev_lower = ev.to_ascii_lowercase();
                        if ev_lower.contains("bullet") || ev_lower.contains("ultrabullet") {
                            return ControlFlow::Break(Err(IngestError::RejectedByPolicy(
                                "Bullet".into(),
                            )));
                        }
                    }
                    if let Some(tc) = &tags.time_control {
                        if opening_explorer_curate::policy::is_bullet_time_control(tc) {
                            return ControlFlow::Break(Err(IngestError::RejectedByPolicy(
                                "Bullet TC".into(),
                            )));
                        }
                    }
                    match (tags.white_elo, tags.black_elo) {
                        (Some(w), Some(b)) => {
                            if !((w >= 2200 && b >= 2200)
                                || (w >= 2400 && b >= 2000)
                                || (b >= 2400 && w >= 2000))
                            {
                                return ControlFlow::Break(Err(IngestError::RejectedByPolicy(
                                    "Below rating threshold".into(),
                                )));
                            }
                        }
                        _ => {
                            return ControlFlow::Break(Err(IngestError::RejectedByPolicy(
                                "Missing rating".into(),
                            )));
                        }
                    }
                }
                Collection::MastersOtb
                    if !self.gigabase_policy.as_ref().is_some_and(|p| p.is_r3()) =>
                {
                    let has_historical = tags
                        .date
                        .as_deref()
                        .and_then(|d| {
                            d.split(['.', '-'])
                                .next()
                                .and_then(|y| y.parse::<u16>().ok())
                        })
                        .is_some_and(|y| y < 1970);

                    if !has_historical {
                        match (tags.white_elo, tags.black_elo) {
                            (Some(w), Some(b)) => {
                                if !((w >= 2200 && b >= 2200)
                                    || (w >= 2400 && b >= 2000)
                                    || (b >= 2400 && w >= 2000))
                                {
                                    return ControlFlow::Break(Err(IngestError::RejectedByPolicy(
                                        "Below rating threshold".into(),
                                    )));
                                }
                            }
                            _ => {
                                return ControlFlow::Break(Err(IngestError::RejectedByPolicy(
                                    "Missing rating post-1970".into(),
                                )));
                            }
                        }
                    }
                }
                Collection::MastersOtb => {}
            }
        }

        ControlFlow::Continue(GameMovetext {
            tags,
            board,
            moves: Vec::with_capacity(64),
            legal_replay_failed: false,
            failure_ply: 0,
            failure_reason: String::new(),
            ply_count: 0,
        })
    }

    fn san(
        &mut self,
        movetext: &mut Self::Movetext,
        san_plus: SanPlus,
    ) -> ControlFlow<Self::Output> {
        if movetext.legal_replay_failed {
            return ControlFlow::Continue(());
        }
        movetext.ply_count += 1;

        match san_plus.san.to_move(&movetext.board) {
            Ok(m) => {
                let compact = match &m {
                    Move::Normal {
                        from,
                        to,
                        promotion,
                        ..
                    } => {
                        let promo_val = match promotion {
                            Some(Role::Knight) => Some(1),
                            Some(Role::Bishop) => Some(2),
                            Some(Role::Rook) => Some(3),
                            Some(Role::Queen) => Some(4),
                            _ => None,
                        };
                        CompactMove::new(u8::from(*from), u8::from(*to), promo_val)
                    }
                    Move::EnPassant { from, to } => {
                        CompactMove::new(u8::from(*from), u8::from(*to), None)
                    }
                    Move::Castle { king, rook } => {
                        let to_sq = if rook.file() > king.file() {
                            if king.rank() == shakmaty::Rank::First {
                                Square::G1
                            } else {
                                Square::G8
                            }
                        } else {
                            if king.rank() == shakmaty::Rank::First {
                                Square::C1
                            } else {
                                Square::C8
                            }
                        };
                        CompactMove::new(u8::from(*king), u8::from(to_sq), None)
                    }
                    Move::Put { .. } => {
                        movetext.legal_replay_failed = true;
                        movetext.failure_ply = movetext.ply_count;
                        movetext.failure_reason = "Drop move in standard chess".into();
                        return ControlFlow::Continue(());
                    }
                };

                movetext.moves.push(compact);
                movetext.board.play_unchecked(m);
                ControlFlow::Continue(())
            }
            Err(e) => {
                movetext.legal_replay_failed = true;
                movetext.failure_ply = movetext.ply_count;
                movetext.failure_reason = format!("Illegal SAN move '{}': {}", san_plus, e);
                ControlFlow::Continue(())
            }
        }
    }

    fn end_game(&mut self, movetext: Self::Movetext) -> Self::Output {
        if movetext.legal_replay_failed {
            return Err(IngestError::IllegalMove {
                ply: movetext.failure_ply,
                reason: movetext.failure_reason,
            });
        }

        if movetext.tags.white.is_empty()
            && movetext.tags.black.is_empty()
            && movetext.moves.is_empty()
        {
            return Err(IngestError::MalformedPgn(
                "Malformed PGN record missing players and moves".into(),
            ));
        }

        let resolved_date = resolve_canonical_date(
            movetext.tags.date.as_deref(),
            movetext.tags.utc_date.as_deref(),
        )
        .or_else(|| {
            self.gigabase_policy
                .as_ref()
                .is_some_and(GigabaseOtbPolicy::is_r3)
                .then(|| {
                    movetext
                        .tags
                        .date
                        .clone()
                        .or_else(|| movetext.tags.utc_date.clone())
                })
                .flatten()
        });
        let (source_id, broadcast_url) = resolve_provenance(
            movetext.tags.site.as_deref(),
            movetext.tags.game_url.as_deref(),
            movetext.tags.broadcast_url.as_deref(),
            movetext.tags.game_id_tag.as_deref(),
        );

        let id = compute_stable_game_id(
            self.collection,
            source_id.as_deref(),
            &movetext.tags.white,
            &movetext.tags.black,
            resolved_date.as_deref(),
            movetext.tags.round.as_deref(),
            &movetext.moves,
        );
        let source_attribution = movetext.tags.source_attribution();

        let meta = GameMetadata {
            id,
            collection: self.collection,
            white: movetext.tags.white,
            black: movetext.tags.black,
            white_elo: movetext.tags.white_elo,
            black_elo: movetext.tags.black_elo,
            date: resolved_date,
            event: movetext.tags.event,
            site: movetext.tags.site,
            round: movetext.tags.round,
            result: movetext.tags.result,
            eco: movetext.tags.eco,
            time_control: movetext.tags.time_control,
            source_id,
            broadcast_url,
            source_attribution,
        };

        Ok(ParsedGame {
            metadata: meta,
            moves: movetext.moves,
            fen_start: movetext.tags.fen_start,
        })
    }
}

/// Returns true only if `io_err` is a single-record PGN syntax error generated by `pgn-reader`
/// (`"unterminated tag"` or `"unterminated comment"`), rather than a fatal stream/decompression I/O failure.
pub fn is_recoverable_pgn_syntax_error(io_err: &std::io::Error) -> bool {
    if io_err.kind() != std::io::ErrorKind::InvalidData {
        return false;
    }
    let msg = io_err.to_string();
    msg.contains("unterminated tag") || msg.contains("unterminated comment")
}

/// Resynchronizes the `pgn-reader` stream after a single-record syntax error (`unterminated tag` or `unterminated comment`).
/// Returns `Ok(true)` if more games remain, `Ok(false)` on clean EOF, or `Err(IngestError)` on fatal stream/resync failure.
pub fn resync_after_syntax_error<R: Read>(
    pgn_reader: &mut Reader<R>,
    consecutive_syntax_errors: &mut usize,
) -> Result<bool, IngestError> {
    const MAX_CONSECUTIVE_SYNTAX_ERRORS: usize = 64;
    *consecutive_syntax_errors += 1;
    if *consecutive_syntax_errors > MAX_CONSECUTIVE_SYNTAX_ERRORS {
        return Err(IngestError::MalformedPgn(format!(
            "Failed to resynchronize PGN stream after {} consecutive syntax errors",
            MAX_CONSECUTIVE_SYNTAX_ERRORS
        )));
    }

    match pgn_reader.skip_game() {
        Ok(has_more) => Ok(has_more),
        Err(io_err) => {
            if is_recoverable_pgn_syntax_error(&io_err) {
                Ok(true)
            } else {
                Err(IngestError::Io(io_err))
            }
        }
    }
}

/// Builds a configured `pgn_reader::Reader` supporting long tag lines (4 KB) and comments (16 KB).
pub fn build_pgn_reader<R: Read>(reader: R) -> Reader<R> {
    Reader::build(reader)
        .set_supported_tag_line_length(4096)
        .set_supported_comment_length(16384)
        .finish()
}

/// Helper function to parse all games from a PGN reader with tracking statistics.
/// Fails closed (`Err`) on fatal I/O or decompression failures.
pub fn parse_pgn_stream<R: Read>(
    reader: R,
    collection: Collection,
    stats: &mut IngestionStats,
) -> Result<Vec<ParsedGame>, IngestError> {
    let mut pgn_reader = build_pgn_reader(reader);
    let mut visitor = ShakmatyGameVisitor::new(collection, 1);
    let mut games = Vec::new();
    let mut consecutive_syntax_errors = 0usize;

    loop {
        match pgn_reader.read_game(&mut visitor) {
            Ok(Some(result)) => {
                consecutive_syntax_errors = 0;
                stats.total_encountered += 1;
                match result {
                    Ok(game) => {
                        stats.accepted_unique += 1;
                        games.push(game);
                    }
                    Err(IngestError::UnsupportedVariant(_))
                    | Err(IngestError::NonStandardStartingPosition(_)) => {
                        stats.rejected_variant += 1;
                    }
                    Err(IngestError::IllegalMove { ply, reason }) => {
                        tracing::warn!("Rejected illegal move at ply {}: {}", ply, reason);
                        stats.rejected_illegal += 1;
                    }
                    Err(IngestError::MalformedPgn(_)) => {
                        stats.rejected_malformed += 1;
                    }
                    Err(_) => {
                        stats.quarantined += 1;
                    }
                }
            }
            Ok(None) => {
                stats.completion_status = IngestionCompletionStatus::CompleteEof;
                break;
            }
            Err(io_err) => {
                if is_recoverable_pgn_syntax_error(&io_err) {
                    stats.total_encountered += 1;
                    stats.rejected_malformed += 1;
                    tracing::warn!("Malformed PGN syntax error: {}", io_err);
                    if !resync_after_syntax_error(&mut pgn_reader, &mut consecutive_syntax_errors)?
                    {
                        stats.completion_status = IngestionCompletionStatus::CompleteEof;
                        break;
                    }
                } else {
                    tracing::error!("Fatal I/O error while reading PGN: {}", io_err);
                    return Err(IngestError::Io(io_err));
                }
            }
        }
    }

    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PGN: &str = r#"
[Event "FIDE World Championship 2021"]
[Site "Dubai UAE"]
[Date "2021.12.03"]
[Round "6"]
[White "Carlsen, Magnus"]
[Black "Nepomniachtchi, Ian"]
[Result "1-0"]
[WhiteElo "2855"]
[BlackElo "2782"]
[ECO "D02"]

1. d4 Nf6 2. Nf3 d5 3. g3 e6 4. Bg2 Be7 5. O-O O-O 6. b3 c5 7. dxc5 Bxc5 1-0

[Event "Illegal Move Test"]
[Site "Test"]
[Date "2024.01.01"]
[White "Player1"]
[Black "Player2"]
[Result "*"]

1. e4 e5 2. Qh5 Ke7 3. Qxe5# *

[Event "Invalid Game"]
[Site "Test"]
[Date "2024.01.01"]
[White "Cheater"]
[Black "Victim"]
[Result "*"]

1. e4 e5 2. Nf3 Ke7 3. Nxe5 Ke6 4. d4 Ba3 5. Nxa3 Nh6 6. Bxh6 g6 7. Bc4+ d5 8. Bxd5+ Ke7 9. Qf3 Kd6 10. Nxf7+ Ke7 11. Bg5+ Kd7 12. Qg4+ Ke8 13. Nd6+ Qxd6 14. Qxc8+ Qd8 15. Qxd8# 1-0
"#;

    #[test]
    fn test_parse_pgn_stream() {
        let mut stats = IngestionStats::default();
        let games =
            parse_pgn_stream(SAMPLE_PGN.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.total_encountered, 3);
        assert_eq!(stats.accepted_unique, 3);
        assert!(stats.is_fully_consistent(games.len()));
        assert_eq!(
            stats.completion_status,
            IngestionCompletionStatus::CompleteEof
        );
        assert_eq!(games.len(), 3);

        // Verify Carlsen vs Nepo game
        let g1 = &games[0];
        assert_eq!(g1.metadata.white, "Carlsen, Magnus");
        assert_eq!(g1.metadata.black, "Nepomniachtchi, Ian");
        assert_eq!(g1.metadata.white_elo, Some(2855));
        assert_eq!(g1.metadata.result, GameResult::WhiteWins);
        assert_eq!(g1.moves.len(), 14); // 7 full moves = 14 plies
    }

    fn parse_curated_game_with_termination(termination: &str) -> Result<ParsedGame, IngestError> {
        let pgn = format!(
            "[Event \"Rated Standard\"]\n[White \"A\"]\n[Black \"B\"]\n[WhiteElo \"2400\"]\n[BlackElo \"2200\"]\n[TimeControl \"600+0\"]\n[Result \"1-0\"]\n[Termination \"{}\"]\n\n1. e4 e5 2. Nf3 Nc6 1-0\n",
            termination
        );
        let mut reader = build_pgn_reader(pgn.as_bytes());
        let mut visitor =
            ShakmatyGameVisitor::new(Collection::EliteOnline, 1).with_early_policy_filter(true);
        reader
            .read_game(&mut visitor)
            .expect("valid PGN")
            .expect("one game")
    }

    #[test]
    fn test_curated_reader_rejects_aborted_and_infraction_terminations() {
        for termination in ["Abandoned", "Rules infraction", "Aborted"] {
            match parse_curated_game_with_termination(termination) {
                Err(IngestError::RejectedByPolicy(reason)) => {
                    assert_eq!(reason, "Aborted or rules infraction");
                }
                other => panic!("Expected termination rejection for {termination}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_curated_reader_keeps_normal_and_time_forfeit_terminations() {
        for termination in ["Normal", "Time forfeit"] {
            assert!(
                parse_curated_game_with_termination(termination).is_ok(),
                "{termination}"
            );
        }
    }

    #[test]
    fn test_illegal_move_rejection() {
        let bad_pgn = r#"
[Event "Illegal Knight Teleport"]
[White "Bad"]
[Black "Good"]
[Result "*"]

1. e4 e5 2. Na6 0-1
"#;
        let mut stats = IngestionStats::default();
        let games =
            parse_pgn_stream(bad_pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.total_encountered, 1);
        assert_eq!(stats.rejected_illegal, 1);
        assert!(stats.is_fully_consistent(games.len()));
        assert_eq!(games.len(), 0);
    }

    #[test]
    fn test_unsupported_variant_rejection() {
        let variant_pgn = r#"
[Event "Crazyhouse"]
[Variant "Crazyhouse"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 *
"#;
        let mut stats = IngestionStats::default();
        let games =
            parse_pgn_stream(variant_pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.total_encountered, 1);
        assert_eq!(stats.rejected_variant, 1);
        assert!(stats.is_fully_consistent(games.len()));
        assert_eq!(games.len(), 0);
    }

    #[test]
    fn test_non_standard_fen_rejection() {
        // Non-standard starting position (e.g. Chess960 or custom handicap)
        let chess960_pgn = r#"
[Event "Fischer Random"]
[FEN "bbqnnrkr/pppppppp/8/8/8/8/PPPPPPPP/BBQNNRKR w HFhf - 0 1"]
[White "Player1"]
[Black "Player2"]
[Result "*"]

1. c4 c5 *
"#;
        let mut stats = IngestionStats::default();
        let games =
            parse_pgn_stream(chess960_pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.total_encountered, 1);
        assert_eq!(stats.rejected_variant, 1);
        assert!(stats.is_fully_consistent(games.len()));
        assert_eq!(games.len(), 0);

        // Standard FEN explicitly specified should be accepted
        let standard_fen_pgn = r#"
[Event "Standard with explicit FEN"]
[FEN "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"]
[White "Player1"]
[Black "Player2"]
[Result "1-0"]

1. e4 e5 1-0
"#;
        let mut stats2 = IngestionStats::default();
        let games2 = parse_pgn_stream(
            standard_fen_pgn.as_bytes(),
            Collection::MastersOtb,
            &mut stats2,
        )
        .unwrap();
        assert_eq!(stats2.accepted_unique, 1);
        assert!(stats2.is_fully_consistent(games2.len()));
        assert_eq!(games2.len(), 1);
    }

    #[test]
    fn test_castling_both_sides() {
        let pgn = r#"
[Event "Castling Kingside Test"]
[White "White"]
[Black "Black"]
[Result "1/2-1/2"]

1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. O-O Nf6 5. d3 O-O 1/2-1/2

[Event "Castling Queenside Test"]
[White "White"]
[Black "Black"]
[Result "1/2-1/2"]

1. d4 d5 2. Nc3 Nc6 3. Bf4 Bf5 4. Qd2 Qd7 5. O-O-O O-O-O 1/2-1/2
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.accepted_unique, 2);
        assert!(stats.is_fully_consistent(games.len()));
        // Game 1: O-O
        let g1 = &games[0];
        assert_eq!(g1.moves[6].to_uci(), "e1g1");
        assert_eq!(g1.moves[9].to_uci(), "e8g8");

        // Game 2: O-O-O
        let g2 = &games[1];
        assert_eq!(g2.moves[8].to_uci(), "e1c1");
        assert_eq!(g2.moves[9].to_uci(), "e8c8");
    }

    #[test]
    fn test_en_passant() {
        let pgn = r#"
[Event "En Passant Test"]
[White "White"]
[Black "Black"]
[Result "*"]

1. e4 c5 2. e5 d5 3. exd6 *
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.accepted_unique, 1);
        assert!(stats.is_fully_consistent(games.len()));
        let g = &games[0];
        assert_eq!(g.moves.len(), 5);
        // Move 5: exd6 -> from e5 (sq 36) to d6 (sq 43)
        assert_eq!(g.moves[4].to_uci(), "e5d6");
    }

    #[test]
    fn test_underpromotion() {
        // Construct standard promotion sequence:
        // 1. e4 d5 2. exd5 c6 3. dxc6 Nf6 4. cxb7 e5 5. bxa8=N
        let pgn = r#"
[Event "Promotion Test"]
[White "White"]
[Black "Black"]
[Result "*"]

1. e4 d5 2. exd5 c6 3. dxc6 Nf6 4. cxb7 e5 5. bxa8=N *
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.accepted_unique, 1);
        assert!(stats.is_fully_consistent(games.len()));
        let g = &games[0];
        let last_move = g.moves.last().unwrap();
        assert_eq!(last_move.to_uci(), "b7a8n");
        assert_eq!(last_move.promotion_piece(), Some(1)); // 1 = Knight
    }

    #[test]
    fn test_unicode_and_escaped_headers() {
        let pgn = r#"
[Event "Torneo Internacional \"Memorial Capablanca\""]
[Site "La Habana CUB"]
[Date "1962.05.01"]
[Round "1"]
[White "Najdorf, Miguel"]
[Black "Spassky, Boris V"]
[Result "1/2-1/2"]
[WhiteElo "2580"]
[BlackElo "2640"]

1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 1/2-1/2
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.accepted_unique, 1);
        assert!(stats.is_fully_consistent(games.len()));
        let g = &games[0];
        assert_eq!(g.metadata.white, "Najdorf, Miguel");
        assert_eq!(g.metadata.black, "Spassky, Boris V");
        assert_eq!(
            g.metadata.event.as_deref(),
            Some("Torneo Internacional \"Memorial Capablanca\"")
        );
    }

    #[test]
    fn test_date_and_utcdate_precedence() {
        let pgn = r#"
[Event "Only UTCDate (Lichess Rated)"]
[Site "https://lichess.org/a9tcp02g"]
[UTCDate "2012.12.31"]
[White "PlayerA"]
[Black "PlayerB"]
[Result "1-0"]

1. e4 e5 1-0

[Event "Partial Date with Complete UTCDate"]
[Site "Canberra, Australia"]
[Date "2026.??.??"]
[UTCDate "2026.07.31"]
[White "PlayerC"]
[Black "PlayerD"]
[Result "0-1"]

1. d4 d5 0-1

[Event "UTCDate Before Unknown Date (Never Overwrite Valid Date)"]
[Site "Canberra, Australia"]
[UTCDate "2026.08.15"]
[Date "????.??.??"]
[White "PlayerE"]
[Black "PlayerF"]
[Result "1/2-1/2"]

1. c4 c5 1/2-1/2

[Event "Both Complete (Date Takes Precedence Over UTCDate)"]
[Site "Canberra, Australia"]
[Date "2026.07.30"]
[UTCDate "2026.07.31"]
[White "PlayerG"]
[Black "PlayerH"]
[Result "1-0"]

1. Nf3 Nf6 1-0
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(games.len(), 4);
        assert_eq!(games[0].metadata.date.as_deref(), Some("2012.12.31"));
        assert_eq!(games[1].metadata.date.as_deref(), Some("2026.07.31"));
        assert_eq!(games[2].metadata.date.as_deref(), Some("2026.08.15"));
        assert_eq!(games[3].metadata.date.as_deref(), Some("2026.07.30"));
    }

    #[test]
    fn test_provenance_extraction_game_vs_broadcast_vs_site() {
        let pgn = r#"
[Event "2026 ANU Open"]
[Site "Canberra, Australia"]
[Date "2026.07.31"]
[Round "2.1"]
[White "Ikeda, Junta"]
[Black "Hegde, Sanat"]
[Result "1-0"]
[BroadcastURL "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots"]
[GameURL "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots/Qmy8doCq"]

1. e4 c6 1-0

[Event "Broadcast Without GameURL"]
[Site "Canberra, Australia"]
[Date "2026.07.31"]
[White "A"]
[Black "B"]
[Result "1-0"]
[BroadcastURL "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots"]

1. e4 e5 1-0

[Event "Broadcast URL in Site Without GameURL"]
[Site "https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots"]
[Date "2026.07.31"]
[White "C"]
[Black "D"]
[Result "1-0"]

1. d4 d5 1-0

[Event "Online Game With Canonical Site URL"]
[Site "https://lichess.org/a9tcp02g"]
[UTCDate "2024.05.01"]
[White "E"]
[Black "F"]
[Result "0-1"]

1. c4 e5 0-1
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();
        assert_eq!(games.len(), 4);

        // Game 0: Full Lichess Broadcast tags
        assert_eq!(
            games[0].metadata.site.as_deref(),
            Some("Canberra, Australia")
        );
        assert_eq!(
            games[0].metadata.source_id.as_deref(),
            Some("https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots/Qmy8doCq")
        );
        assert_eq!(
            games[0].metadata.broadcast_url.as_deref(),
            Some("https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots")
        );

        // Game 1: BroadcastURL only -> must NOT pollute source_id!
        assert_eq!(
            games[1].metadata.site.as_deref(),
            Some("Canberra, Australia")
        );
        assert_eq!(games[1].metadata.source_id, None);
        assert_eq!(
            games[1].metadata.broadcast_url.as_deref(),
            Some("https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots")
        );

        // Game 2: Tournament/Broadcast URL placed in Site -> classified as broadcast_url, NOT source_id!
        assert_eq!(games[2].metadata.source_id, None);
        assert_eq!(
            games[2].metadata.broadcast_url.as_deref(),
            Some("https://lichess.org/broadcast/2026-anu-open/round-2/iDPcCots")
        );

        // Game 3: Game-specific URL in Site -> extracted as source_id
        assert_eq!(
            games[3].metadata.source_id.as_deref(),
            Some("https://lichess.org/a9tcp02g")
        );
        assert_eq!(games[3].metadata.broadcast_url, None);
    }

    #[test]
    fn captures_gigabase_source_fields_without_exposing_them_as_persisted_metadata() {
        let pgn = r#"
[Event "Federation Open"]
[Site "City"]
[Date "2024.01.01"]
[White "Player A"]
[Black "Player B"]
[WhiteElo "2450"]
[BlackElo "2300"]
[WhiteFideId "123456"]
[BlackFideId "654321"]
[Source "Britbase"]
[SourceQuality "1"]
[IdentityEvidenceWhite "Confirmed"]
[IdentityEvidenceBlack "Strong"]
[Mode "Correspondence"]
[Result "1-0"]

1. e4 e5 1-0
"#;
        let mut stats = IngestionStats::default();
        let games = parse_pgn_stream(pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();
        let attribution = games[0].metadata.source_attribution.as_ref().unwrap();

        assert_eq!(attribution.source_tag.as_deref(), Some("Britbase"));
        assert_eq!(attribution.source_quality.as_deref(), Some("1"));
        assert_eq!(attribution.white_fide_id.as_deref(), Some("123456"));
        assert_eq!(attribution.black_fide_id.as_deref(), Some("654321"));
        assert_eq!(
            attribution.white_identity_evidence.as_deref(),
            Some("Confirmed")
        );
        assert_eq!(
            attribution.black_identity_evidence.as_deref(),
            Some("Strong")
        );
        assert_eq!(attribution.mode.as_deref(), Some("Correspondence"));
        assert!(serde_json::to_value(&games[0].metadata)
            .unwrap()
            .get("source_attribution")
            .is_none());
    }

    #[test]
    fn test_fail_closed_on_synthetic_io_error() {
        struct FailingReader {
            prefix: Vec<u8>,
            pos: usize,
        }
        impl Read for FailingReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.pos < self.prefix.len() {
                    let n = buf.len().min(self.prefix.len() - self.pos);
                    buf[..n].copy_from_slice(&self.prefix[self.pos..self.pos + n]);
                    self.pos += n;
                    Ok(n)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "synthetic stream read failure",
                    ))
                }
            }
        }

        let reader = FailingReader {
            prefix: b"[Event \"Valid Start\"]\n[White \"A\"]\n".to_vec(),
            pos: 0,
        };
        let mut stats = IngestionStats::default();
        let res = parse_pgn_stream(reader, Collection::MastersOtb, &mut stats);
        assert!(res.is_err(), "Fatal I/O error must fail closed with Err");
    }

    #[test]
    fn test_fail_closed_on_truncated_zstd_stream() {
        let valid_pgn =
            b"[Event \"Test\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n";
        let compressed = zstd::encode_all(&valid_pgn[..], 3).unwrap();
        // Truncate the zstd frame midway so decompression fails
        let truncated = &compressed[..compressed.len() / 2];
        let decoder = zstd::stream::read::Decoder::new(truncated).unwrap();

        let mut stats = IngestionStats::default();
        let res = parse_pgn_stream(decoder, Collection::MastersOtb, &mut stats);
        assert!(
            res.is_err(),
            "Truncated zstd stream must fail closed with Err"
        );
    }

    #[test]
    fn test_malformed_pgn_followed_by_valid_game() {
        // Record 1: Malformed tag name (contains spaces/invalid chars)
        // Record 2: Valid game
        let mixed_pgn = r#"
[Invalid Tag Name "Broken"]
[White "Ghost"]
[Black "Phantom"]

1. e4 e5 *

[Event "Valid Recovery Game"]
[Site "Reykjavik ISL"]
[Date "1972.07.23"]
[Round "6"]
[White "Fischer, Robert James"]
[Black "Spassky, Boris V"]
[Result "1-0"]

1. c4 e6 2. Nf3 d5 3. d4 Nf6 4. Nc3 Be7 1-0
"#;
        let mut stats = IngestionStats::default();
        let games =
            parse_pgn_stream(mixed_pgn.as_bytes(), Collection::MastersOtb, &mut stats).unwrap();

        assert_eq!(stats.total_encountered, 2);
        assert_eq!(stats.rejected_malformed, 1);
        assert_eq!(stats.accepted_unique, 1);
        assert!(stats.is_fully_consistent(games.len()));
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].metadata.white, "Fischer, Robert James");
    }
}
