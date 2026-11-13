use codb_core::ParsedGame;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyVerdict {
    Accepted,
    Rejected(PolicyRejectionReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyRejectionReason {
    BelowRatingThreshold {
        white_elo: Option<u16>,
        black_elo: Option<u16>,
        reason: String,
    },
    ProvisionalRating,
    CasualUnratedGame,
    BulletOrUltraBullet {
        event: Option<String>,
        time_control: Option<String>,
    },
    TooFewPlies {
        plies: usize,
        minimum: usize,
    },
    NonStandardVariant(String),
    NonStandardStartingPosition(String),
    AbortedOrInfraction(String),
    /// Masters OTB only: the event, site or broadcast slug identifies an online event.
    OnlineEventInMastersSource(String),
    /// Masters OTB only: a pre-1970 game without a named event and venue.
    HistoricalWithoutProvenance,
}

impl PolicyRejectionReason {
    /// Stable machine-readable category used in build and quality reports.
    pub fn category(&self) -> &'static str {
        match self {
            PolicyRejectionReason::BelowRatingThreshold {
                white_elo,
                black_elo,
                ..
            } => {
                if white_elo.is_none() || black_elo.is_none() {
                    "missing_rating"
                } else {
                    "below_rating_threshold"
                }
            }
            PolicyRejectionReason::ProvisionalRating => "provisional_rating",
            PolicyRejectionReason::CasualUnratedGame => "casual_or_unrated",
            PolicyRejectionReason::BulletOrUltraBullet { .. } => "bullet_or_ultrabullet",
            PolicyRejectionReason::TooFewPlies { .. } => "too_few_plies",
            PolicyRejectionReason::NonStandardVariant(_) => "non_standard_variant",
            PolicyRejectionReason::NonStandardStartingPosition(_) => "non_standard_start",
            PolicyRejectionReason::AbortedOrInfraction(_) => "aborted_or_infraction",
            PolicyRejectionReason::OnlineEventInMastersSource(_) => {
                "online_event_in_masters_source"
            }
            PolicyRejectionReason::HistoricalWithoutProvenance => "historical_without_provenance",
        }
    }
}

/// Maps the free-text reason produced by the reader's early policy filter onto
/// the stable categories of [`PolicyRejectionReason::category`].
pub fn early_filter_category(reason: &str) -> &'static str {
    match reason {
        "Bullet" | "Bullet TC" => "bullet_or_ultrabullet",
        "Below rating threshold" => "below_rating_threshold",
        r if r.starts_with("Missing rating") => "missing_rating",
        _ => "other_policy",
    }
}

impl std::fmt::Display for PolicyRejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyRejectionReason::BelowRatingThreshold { reason, .. } => {
                write!(f, "Below rating threshold: {}", reason)
            }
            PolicyRejectionReason::ProvisionalRating => write!(f, "Provisional rating"),
            PolicyRejectionReason::CasualUnratedGame => write!(f, "Casual unrated game"),
            PolicyRejectionReason::BulletOrUltraBullet {
                event,
                time_control,
            } => {
                write!(
                    f,
                    "Bullet/UltraBullet excluded (event: {:?}, tc: {:?})",
                    event, time_control
                )
            }
            PolicyRejectionReason::TooFewPlies { plies, minimum } => {
                write!(f, "Too few plies ({} < {})", plies, minimum)
            }
            PolicyRejectionReason::NonStandardVariant(v) => {
                write!(f, "Non-standard variant: {}", v)
            }
            PolicyRejectionReason::NonStandardStartingPosition(fen) => {
                write!(f, "Non-standard starting position: {}", fen)
            }
            PolicyRejectionReason::AbortedOrInfraction(s) => {
                write!(f, "Aborted or rules infraction: {}", s)
            }
            PolicyRejectionReason::OnlineEventInMastersSource(m) => {
                write!(f, "Online event marker in Masters OTB source: {}", m)
            }
            PolicyRejectionReason::HistoricalWithoutProvenance => {
                write!(f, "Pre-1970 game without named event and venue")
            }
        }
    }
}

/// Masters OTB Inclusion Policy
#[derive(Debug, Clone)]
pub struct MastersOtbPolicy {
    pub min_both_elo: u16,
    pub min_one_elo: u16,
    pub min_other_elo: u16,
    pub allow_historical_pre_1970: bool,
    pub min_plies: usize,
}

impl Default for MastersOtbPolicy {
    fn default() -> Self {
        MastersOtbPolicy {
            min_both_elo: 2200,
            min_one_elo: 2400,
            min_other_elo: 2000,
            allow_historical_pre_1970: true,
            min_plies: 4,
        }
    }
}

impl MastersOtbPolicy {
    pub fn evaluate(&self, game: &ParsedGame) -> PolicyVerdict {
        // 1. Standard starting position check
        if let Some(fen) = &game.fen_start {
            if !is_standard_start_fen(fen) {
                return PolicyVerdict::Rejected(
                    PolicyRejectionReason::NonStandardStartingPosition(fen.clone()),
                );
            }
        }

        // 2. Minimum plies check
        if game.moves.len() < self.min_plies {
            return PolicyVerdict::Rejected(PolicyRejectionReason::TooFewPlies {
                plies: game.moves.len(),
                minimum: self.min_plies,
            });
        }

        // 3. OTB provenance: broadcast archives also contain online events.
        if let Some(marker) = online_event_marker(game) {
            return PolicyVerdict::Rejected(PolicyRejectionReason::OnlineEventInMastersSource(
                marker,
            ));
        }

        // 4. Historical exception: pre-1970 classical events before FIDE Elo.
        // A pre-1970 date alone is not evidence of authentic master play: the
        // record must also name both the event and the venue.
        if self.allow_historical_pre_1970 {
            if let Some(year) = extract_year(game) {
                if year < 1970 {
                    return if game.metadata.event.is_some() && game.metadata.site.is_some() {
                        PolicyVerdict::Accepted
                    } else {
                        PolicyVerdict::Rejected(PolicyRejectionReason::HistoricalWithoutProvenance)
                    };
                }
            }
        }

        // 5. Elo rating thresholds
        let w = game.metadata.white_elo;
        let b = game.metadata.black_elo;

        match (w, b) {
            (Some(we), Some(be)) => {
                if (we >= self.min_both_elo && be >= self.min_both_elo)
                    || (we >= self.min_one_elo && be >= self.min_other_elo)
                    || (be >= self.min_one_elo && we >= self.min_other_elo)
                {
                    PolicyVerdict::Accepted
                } else {
                    PolicyVerdict::Rejected(PolicyRejectionReason::BelowRatingThreshold {
                        white_elo: w,
                        black_elo: b,
                        reason: format!(
                            "Ratings ({}, {}) do not satisfy both >= {} or one >= {} & other >= {}",
                            we, be, self.min_both_elo, self.min_one_elo, self.min_other_elo
                        ),
                    })
                }
            }
            // If ratings missing post-1970, reject unless explicit title or historical
            _ => PolicyVerdict::Rejected(PolicyRejectionReason::BelowRatingThreshold {
                white_elo: w,
                black_elo: b,
                reason: "Missing official FIDE rating data for post-1970 game".into(),
            }),
        }
    }
}

/// Elite Online Inclusion Policy
#[derive(Debug, Clone)]
pub struct EliteOnlinePolicy {
    pub min_both_rating: u16,
    pub min_one_rating: u16,
    pub min_other_rating: u16,
    pub min_plies: usize,
    pub exclude_bullet: bool,
}

impl Default for EliteOnlinePolicy {
    fn default() -> Self {
        EliteOnlinePolicy {
            min_both_rating: 2200,
            min_one_rating: 2400,
            min_other_rating: 2000,
            min_plies: 6,
            exclude_bullet: true,
        }
    }
}

impl EliteOnlinePolicy {
    pub fn evaluate(&self, game: &ParsedGame) -> PolicyVerdict {
        // 1. Standard starting position check
        if let Some(fen) = &game.fen_start {
            if !is_standard_start_fen(fen) {
                return PolicyVerdict::Rejected(
                    PolicyRejectionReason::NonStandardStartingPosition(fen.clone()),
                );
            }
        }

        // 2. Minimum plies check
        if game.moves.len() < self.min_plies {
            return PolicyVerdict::Rejected(PolicyRejectionReason::TooFewPlies {
                plies: game.moves.len(),
                minimum: self.min_plies,
            });
        }

        // 3a. Casual / unrated exclusion (Lichess labels these in the event name)
        if let Some(ev) = &game.metadata.event {
            if ev.to_ascii_lowercase().contains("casual") {
                return PolicyVerdict::Rejected(PolicyRejectionReason::CasualUnratedGame);
            }
        }

        // 3. Bullet & UltraBullet exclusion
        if self.exclude_bullet {
            if let Some(ev) = &game.metadata.event {
                let ev_lower = ev.to_ascii_lowercase();
                if ev_lower.contains("bullet") || ev_lower.contains("ultrabullet") {
                    return PolicyVerdict::Rejected(PolicyRejectionReason::BulletOrUltraBullet {
                        event: Some(ev.clone()),
                        time_control: game.metadata.time_control.clone(),
                    });
                }
            }

            if let Some(tc) = &game.metadata.time_control {
                if is_bullet_time_control(tc) {
                    return PolicyVerdict::Rejected(PolicyRejectionReason::BulletOrUltraBullet {
                        event: game.metadata.event.clone(),
                        time_control: Some(tc.clone()),
                    });
                }
            }
        }

        // 4. Rating thresholds
        let w = game.metadata.white_elo;
        let b = game.metadata.black_elo;

        match (w, b) {
            (Some(we), Some(be)) => {
                if (we >= self.min_both_rating && be >= self.min_both_rating)
                    || (we >= self.min_one_rating && be >= self.min_other_rating)
                    || (be >= self.min_one_rating && we >= self.min_other_rating)
                {
                    PolicyVerdict::Accepted
                } else {
                    PolicyVerdict::Rejected(PolicyRejectionReason::BelowRatingThreshold {
                        white_elo: w,
                        black_elo: b,
                        reason: format!(
                            "Ratings ({}, {}) below online threshold (both >= {}, or one >= {} & other >= {})",
                            we, be, self.min_both_rating, self.min_one_rating, self.min_other_rating
                        ),
                    })
                }
            }
            _ => PolicyVerdict::Rejected(PolicyRejectionReason::BelowRatingThreshold {
                white_elo: w,
                black_elo: b,
                reason: "Unrated or missing rating data in online game".into(),
            }),
        }
    }
}

/// Tokens that identify an online event when found in the event, site, or
/// broadcast URL slug of a Masters OTB candidate. Matching is token-based so
/// that unrelated words are not caught by substring accidents.
const ONLINE_TOKENS: &[&str] = &[
    "online",
    "internet",
    "arena",
    "bullet",
    "ultrabullet",
    "playchess",
    "chesscom",
    "tornelo",
];

/// Returns a description of the first online-event marker found in the game's
/// event, site, or broadcast URL, or `None` when no marker is present.
///
/// Per-game Lichess broadcast URLs (`https://lichess.org/broadcast/...`) are the
/// default `Site` value for broadcasts and are not markers by themselves, but
/// their slug is inspected. A `Site` that is a plain online platform name or an
/// online game URL is a marker.
pub fn online_event_marker(game: &ParsedGame) -> Option<String> {
    let m = &game.metadata;
    let fields: [(&str, Option<&str>); 3] = [
        ("event", m.event.as_deref()),
        ("site", m.site.as_deref()),
        ("broadcast_url", m.broadcast_url.as_deref()),
    ];
    for (name, value) in fields {
        let Some(v) = value else { continue };
        let lower = v.to_ascii_lowercase();
        if lower.contains("chess.com") {
            return Some(format!("{name} mentions chess.com"));
        }
        if name == "site" && lower.contains("lichess.org") && !lower.contains("/broadcast/") {
            return Some("site is an online Lichess game URL".to_string());
        }
        for token in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
            if ONLINE_TOKENS.contains(&token) {
                return Some(format!("{name} contains '{token}'"));
            }
        }
    }
    None
}

pub fn is_standard_start_fen(fen: &str) -> bool {
    let trimmed = fen.trim();
    trimmed.starts_with("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq -")
}

pub fn is_bullet_time_control(tc: &str) -> bool {
    // Format: "<initial_seconds>+<increment_seconds>"
    if let Some((init, inc)) = tc.trim().split_once('+') {
        if let (Ok(init_sec), Ok(inc_sec)) = (init.parse::<u32>(), inc.parse::<u32>()) {
            // Standard chess definition: estimated game duration = init + 40 * inc
            // Bullet is < 180 seconds total estimated duration
            let total_sec = init_sec + 40 * inc_sec;
            return total_sec < 180;
        }
    }
    false
}

fn extract_year(game: &ParsedGame) -> Option<u16> {
    game.metadata.date.as_ref().and_then(|d| {
        let parts: Vec<&str> = d.split('.').collect();
        parts.first().and_then(|y| y.parse::<u16>().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codb_core::{Collection, CompactMove, GameMetadata, GameResult};

    fn make_game(
        collection: Collection,
        w_elo: Option<u16>,
        b_elo: Option<u16>,
        event: &str,
        date: &str,
        tc: Option<&str>,
        plies: usize,
    ) -> ParsedGame {
        ParsedGame {
            metadata: GameMetadata {
                id: 1,
                collection,
                white: "Player A".into(),
                black: "Player B".into(),
                white_elo: w_elo,
                black_elo: b_elo,
                date: Some(date.into()),
                event: Some(event.into()),
                site: None,
                round: None,
                result: GameResult::WhiteWins,
                eco: None,
                time_control: tc.map(|s| s.into()),
                source_id: None,
                broadcast_url: None,
            },
            moves: vec![CompactMove::new(12, 28, None); plies],
            fen_start: None,
        }
    }

    #[test]
    fn test_elite_online_bullet_rejection() {
        let policy = EliteOnlinePolicy::default();

        // 1+0 bullet game (60s)
        let bullet_game = make_game(
            Collection::EliteOnline,
            Some(2450),
            Some(2500),
            "Rated Bullet game",
            "2024.01.01",
            Some("60+0"),
            20,
        );
        match policy.evaluate(&bullet_game) {
            PolicyVerdict::Rejected(PolicyRejectionReason::BulletOrUltraBullet { .. }) => {}
            other => panic!("Expected bullet rejection, got {:?}", other),
        }

        // 3+0 blitz game (180s)
        let blitz_game = make_game(
            Collection::EliteOnline,
            Some(2450),
            Some(2500),
            "Rated Blitz game",
            "2024.01.01",
            Some("180+0"),
            20,
        );
        assert_eq!(policy.evaluate(&blitz_game), PolicyVerdict::Accepted);
    }

    #[test]
    fn test_elite_online_rating_filter() {
        let policy = EliteOnlinePolicy::default(); // 2200 both or 2400+2000

        // 2300 vs 2250 -> Accepted
        let g1 = make_game(
            Collection::EliteOnline,
            Some(2300),
            Some(2250),
            "Blitz",
            "2024.01.01",
            Some("180+2"),
            20,
        );
        assert_eq!(policy.evaluate(&g1), PolicyVerdict::Accepted);

        // 1800 vs 1750 -> Rejected
        let g2 = make_game(
            Collection::EliteOnline,
            Some(1800),
            Some(1750),
            "Blitz",
            "2024.01.01",
            Some("180+2"),
            20,
        );
        match policy.evaluate(&g2) {
            PolicyVerdict::Rejected(PolicyRejectionReason::BelowRatingThreshold { .. }) => {}
            other => panic!("Expected BelowRatingThreshold, got {:?}", other),
        }
    }

    #[test]
    fn test_masters_otb_historical_exception() {
        let policy = MastersOtbPolicy::default();

        // 1938 AVRO tournament (ratings missing before 1970)
        let historical = make_game(
            Collection::MastersOtb,
            None,
            None,
            "AVRO 1938",
            "1938.11.15",
            None,
            30,
        );
        // A bare pre-1970 date with no venue is not accepted.
        assert_eq!(
            policy.evaluate(&historical),
            PolicyVerdict::Rejected(PolicyRejectionReason::HistoricalWithoutProvenance)
        );
        let mut documented = historical.clone();
        documented.metadata.site = Some("Netherlands".into());
        assert_eq!(policy.evaluate(&documented), PolicyVerdict::Accepted);

        // 2024 modern tournament with missing ratings -> Rejected
        let modern_missing = make_game(
            Collection::MastersOtb,
            None,
            None,
            "Club Open",
            "2024.05.01",
            None,
            30,
        );
        match policy.evaluate(&modern_missing) {
            PolicyVerdict::Rejected(PolicyRejectionReason::BelowRatingThreshold { .. }) => {}
            other => panic!(
                "Expected rejection for modern missing rating, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_elite_online_rejects_casual_and_categories() {
        let policy = EliteOnlinePolicy::default();
        let g = make_game(
            Collection::EliteOnline,
            Some(2450),
            Some(2500),
            "Casual Blitz game",
            "2024.01.01",
            Some("180+0"),
            20,
        );
        let verdict = policy.evaluate(&g);
        assert_eq!(
            verdict,
            PolicyVerdict::Rejected(PolicyRejectionReason::CasualUnratedGame)
        );
        if let PolicyVerdict::Rejected(r) = verdict {
            assert_eq!(r.category(), "casual_or_unrated");
        }
        assert_eq!(
            early_filter_category("Missing rating post-1970"),
            "missing_rating"
        );
        assert_eq!(early_filter_category("Bullet TC"), "bullet_or_ultrabullet");
    }

    #[test]
    fn test_masters_otb_rejects_online_events() {
        let policy = MastersOtbPolicy::default();
        for (event, site, url, expect_online) in [
            ("Titled Tuesday Online", None, None, true),
            ("Candidates 2026", Some("Chess.com"), None, true),
            ("Open", Some("https://lichess.org/a9tcp02g"), None, true),
            (
                "Round 3",
                None,
                Some("https://lichess.org/broadcast/fide-online-blitz/round-3/abc"),
                true,
            ),
            ("Bullet Arena", None, None, true),
            (
                "Gazi Open",
                Some("https://lichess.org/broadcast/gazi-open/round-3/IkQ2crNr/DB4I8oF3"),
                Some("https://lichess.org/broadcast/gazi-open/round-3/IkQ2crNr"),
                false,
            ),
            ("Tata Steel Masters", Some("Wijk aan Zee NED"), None, false),
            ("Online-free Online", None, None, true),
            ("Baltic Onlinesport Cup", None, None, false),
        ] {
            let mut g = make_game(
                Collection::MastersOtb,
                Some(2650),
                Some(2700),
                event,
                "2026.03.01",
                None,
                40,
            );
            g.metadata.site = site.map(Into::into);
            g.metadata.broadcast_url = url.map(Into::into);
            let verdict = policy.evaluate(&g);
            if expect_online {
                assert!(
                    matches!(
                        verdict,
                        PolicyVerdict::Rejected(PolicyRejectionReason::OnlineEventInMastersSource(
                            _
                        ))
                    ),
                    "expected online rejection for {event:?}, got {verdict:?}"
                );
            } else {
                assert_eq!(verdict, PolicyVerdict::Accepted, "{event:?}");
            }
        }
    }
}
