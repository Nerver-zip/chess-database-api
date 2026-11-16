use codb_core::{GameSourceAttribution, ParsedGame};
use std::collections::HashSet;

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
    /// Masters OTB only: event-level evidence did not verify a physical tournament.
    UnverifiedMastersProvenance(String),
    /// Masters OTB only: a pre-1970 game without a named event and venue.
    HistoricalWithoutProvenance,
    /// Gigabase-specific: source tag is absent or is outside the reviewed source allowlist.
    UnqualifiedGigabaseSource(Option<String>),
    /// Gigabase-specific: both player identities are not publisher-labelled Strong/Confirmed.
    UntrustedGigabasePlayerIdentity {
        white: Option<String>,
        black: Option<String>,
    },
    /// Gigabase-specific: the PGN explicitly says the game was not over the board.
    NonOtbGigabaseMode(String),
    /// Gigabase-specific: the date is pre-1970 and its Elo values are not independently verified.
    UnverifiedGigabaseHistoricalRating(u16),
    /// Gigabase-specific: the date is absent, partial, or not a valid calendar date.
    UnverifiedGigabaseDate,
    /// Gigabase r3: the supplied rating text is malformed or outside the accepted plausibility range.
    InvalidGigabaseRating {
        player: &'static str,
    },
    /// Gigabase r3: no approved strength rule is met.
    BelowGigabaseStrength,
    /// Gigabase r3: player name is empty, a placeholder, or otherwise unusable.
    InvalidGigabasePlayerIdentity {
        player: &'static str,
    },
    /// Gigabase r3: the publisher explicitly marked a player identity as conflicting.
    ConflictingGigabasePlayerIdentity {
        player: &'static str,
    },
    /// Gigabase r3: event or site metadata explicitly contradicts the OTB archive assertion.
    ContradictoryGigabaseProvenance {
        field: &'static str,
    },
    /// Gigabase r3: a supplied date has inconsistent known components.
    InvalidGigabaseDate,
    /// Gigabase r3: duplicate PGN headers contained conflicting metadata values.
    ConflictingGigabaseHeader(String),
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
            PolicyRejectionReason::UnverifiedMastersProvenance(_) => {
                "unresolved_masters_provenance"
            }
            PolicyRejectionReason::HistoricalWithoutProvenance => "historical_without_provenance",
            PolicyRejectionReason::UnqualifiedGigabaseSource(_) => "unqualified_source",
            PolicyRejectionReason::UntrustedGigabasePlayerIdentity { .. } => {
                "untrusted_player_identity"
            }
            PolicyRejectionReason::NonOtbGigabaseMode(_) => "non_otb_play_mode",
            PolicyRejectionReason::UnverifiedGigabaseHistoricalRating(_) => {
                "unverified_historical_rating"
            }
            PolicyRejectionReason::UnverifiedGigabaseDate => "unverified_game_date",
            PolicyRejectionReason::InvalidGigabaseRating { .. } => "invalid_rating",
            PolicyRejectionReason::BelowGigabaseStrength => "below_rating_threshold",
            PolicyRejectionReason::InvalidGigabasePlayerIdentity { .. } => {
                "invalid_player_identity"
            }
            PolicyRejectionReason::ConflictingGigabasePlayerIdentity { .. } => {
                "conflicting_player_identity"
            }
            PolicyRejectionReason::ContradictoryGigabaseProvenance { .. } => {
                "contradictory_otb_evidence"
            }
            PolicyRejectionReason::InvalidGigabaseDate => "invalid_game_date",
            PolicyRejectionReason::ConflictingGigabaseHeader(_) => "conflicting_source_metadata",
        }
    }
}

/// Maps the free-text reason produced by the reader's early policy filter onto
/// the stable categories of [`PolicyRejectionReason::category`].
pub fn early_filter_category(reason: &str) -> &'static str {
    match reason {
        "gigabase:unqualified_source" => "unqualified_source",
        "gigabase:untrusted_player_identity" => "untrusted_player_identity",
        "gigabase:non_otb_play_mode" => "non_otb_play_mode",
        "gigabase:unverified_historical_rating" => "unverified_historical_rating",
        "gigabase:unverified_game_date" => "unverified_game_date",
        "gigabase:invalid_rating" => "invalid_rating",
        "gigabase:below_rating_threshold" => "below_rating_threshold",
        "gigabase:invalid_player_identity" => "invalid_player_identity",
        "gigabase:conflicting_player_identity" => "conflicting_player_identity",
        "gigabase:contradictory_otb_evidence" => "contradictory_otb_evidence",
        "gigabase:invalid_game_date" => "invalid_game_date",
        "gigabase:conflicting_source_metadata" => "conflicting_source_metadata",
        "Bullet" | "Bullet TC" => "bullet_or_ultrabullet",
        "Below rating threshold" => "below_rating_threshold",
        r if r.starts_with("Missing rating") => "missing_rating",
        "Aborted or rules infraction" => "aborted_or_infraction",
        _ => "other_policy",
    }
}

#[cfg(test)]
mod gigabase_policy_tests {
    use super::*;

    fn attribution(mode: Option<&str>, white: &str, black: &str) -> GameSourceAttribution {
        GameSourceAttribution {
            source_tag: Some("Britbase".into()),
            source_quality: Some("1".into()),
            white_fide_id: Some("123456".into()),
            black_fide_id: Some("654321".into()),
            white_identity_evidence: Some(white.into()),
            black_identity_evidence: Some(black.into()),
            mode: mode.map(str::to_owned),
            white_title: None,
            black_title: None,
        }
    }

    fn policy() -> GigabaseOtbPolicy {
        GigabaseOtbPolicy::new(HashSet::from(["Britbase".to_owned()]))
    }

    #[test]
    fn accepts_documented_source_with_trusted_identities_and_missing_mode() {
        assert!(policy()
            .evaluate_source_metadata(
                Some(&attribution(None, "Confirmed", "Strong")),
                Some("2020.01.01")
            )
            .is_ok());
    }

    #[test]
    fn rejects_sources_outside_allowlist() {
        let mut source = attribution(None, "Confirmed", "Confirmed");
        source.source_tag = Some("Historical Archive".into());
        let err = policy()
            .evaluate_source_metadata(Some(&source), Some("2020.01.01"))
            .unwrap_err();
        assert_eq!(err.category(), "unqualified_source");
    }

    #[test]
    fn rejects_untrusted_identity_and_explicit_non_otb_mode() {
        let err = policy()
            .evaluate_source_metadata(
                Some(&attribution(None, "NoMatch", "Confirmed")),
                Some("2020.01.01"),
            )
            .unwrap_err();
        assert_eq!(err.category(), "untrusted_player_identity");

        let err = policy()
            .evaluate_source_metadata(
                Some(&attribution(
                    Some("Correspondence"),
                    "Confirmed",
                    "Confirmed",
                )),
                Some("2020.01.01"),
            )
            .unwrap_err();
        assert_eq!(err.category(), "non_otb_play_mode");
    }

    #[test]
    fn rejects_pre_1970_publisher_elo_without_historical_validation() {
        let err = policy()
            .evaluate_source_metadata(
                Some(&attribution(None, "Confirmed", "Confirmed")),
                Some("1969.12.31"),
            )
            .unwrap_err();
        assert_eq!(err.category(), "unverified_historical_rating");
    }

    #[test]
    fn rejects_missing_partial_and_invalid_gigabase_dates() {
        for date in [
            None,
            Some("????.??.??"),
            Some("2020.??.??"),
            Some("2020.13.01"),
            Some("2021.02.29"),
        ] {
            let err = policy()
                .evaluate_source_metadata(Some(&attribution(None, "Confirmed", "Confirmed")), date)
                .unwrap_err();
            assert_eq!(err.category(), "unverified_game_date", "date: {date:?}");
        }
    }

    #[test]
    fn accepts_complete_leap_day_gigabase_date() {
        assert!(policy()
            .evaluate_source_metadata(
                Some(&attribution(None, "Confirmed", "Confirmed")),
                Some("2024.02.29")
            )
            .is_ok());
    }

    fn r3_headers() -> GigabaseHeaderMetadata<'static> {
        GigabaseHeaderMetadata {
            source_tag: Some("Historical Archive"),
            white_name: Some("Smith, John"),
            black_name: Some("Jones, Alex"),
            white_elo: Some("2250"),
            black_elo: Some("2210"),
            white_title: None,
            black_title: None,
            white_identity_evidence: Some("NoMatch"),
            black_identity_evidence: Some("Probable"),
            white_fide_id: None,
            black_fide_id: None,
            mode: None,
            date: Some("1969.??.??"),
            event: Some("Open Championship"),
            site: Some("London"),
            conflicting_metadata_fields: &[],
        }
    }

    #[test]
    fn r3_accepts_unlisted_source_no_match_identity_missing_mode_and_historical_partial_date() {
        let headers = r3_headers();
        let policy = GigabaseOtbPolicy::publisher_trust_r3();
        assert_eq!(
            policy.evaluate_r3_headers(&headers).unwrap(),
            GigabaseStrengthRule::Original2200Or2400_2000
        );
        assert_eq!(
            parse_gigabase_date(headers.date),
            Some(GigabaseDateInfo {
                year: Some(1969),
                precision: GigabaseDatePrecision::Partial,
            })
        );
    }

    #[test]
    fn r3_applies_incremental_symmetric_rating_and_gm_rules() {
        let policy = GigabaseOtbPolicy::publisher_trust_r3();
        let mut headers = r3_headers();
        headers.white_elo = Some("2500");
        headers.black_elo = Some("1850");
        assert_eq!(
            policy.evaluate_r3_headers(&headers).unwrap(),
            GigabaseStrengthRule::Added2400Vs1800
        );
        headers.white_elo = Some("1850");
        headers.black_elo = Some("2500");
        assert_eq!(
            policy.evaluate_r3_headers(&headers).unwrap(),
            GigabaseStrengthRule::Added2400Vs1800
        );
        headers.white_elo = Some("2400");
        headers.black_elo = Some("1799");
        assert_eq!(
            policy.evaluate_r3_headers(&headers),
            Err(PolicyRejectionReason::BelowGigabaseStrength)
        );

        headers.white_elo = None;
        headers.black_elo = Some("1900");
        headers.white_title = Some("GM");
        assert_eq!(
            policy.evaluate_r3_headers(&headers).unwrap(),
            GigabaseStrengthRule::AddedGmVsRatedOpponent
        );
        headers.white_title = Some("WGM");
        assert_eq!(
            policy.evaluate_r3_headers(&headers),
            Err(PolicyRejectionReason::BelowGigabaseStrength)
        );
        headers.white_title = Some("GM");
        headers.black_elo = None;
        headers.black_title = Some("GM");
        assert_eq!(
            policy.evaluate_r3_headers(&headers).unwrap(),
            GigabaseStrengthRule::AddedGmVsGm
        );
    }

    #[test]
    fn r3_rejects_bad_identity_and_malformed_or_implausible_ratings() {
        let policy = GigabaseOtbPolicy::publisher_trust_r3();
        let mut headers = r3_headers();
        headers.white_identity_evidence = Some("Conflict");
        assert_eq!(
            policy.evaluate_r3_headers(&headers),
            Err(PolicyRejectionReason::ConflictingGigabasePlayerIdentity { player: "White" })
        );
        headers.white_identity_evidence = Some("NoMatch");
        headers.white_name = Some("?");
        assert_eq!(
            policy.evaluate_r3_headers(&headers),
            Err(PolicyRejectionReason::InvalidGigabasePlayerIdentity { player: "White" })
        );
        headers.white_name = Some("Smith, John");
        headers.white_elo = Some("0");
        assert_eq!(
            classify_gigabase_strength(&headers),
            Err(PolicyRejectionReason::InvalidGigabaseRating { player: "White" })
        );
        headers.white_elo = Some("not-a-rating");
        assert_eq!(
            classify_gigabase_strength(&headers),
            Err(PolicyRejectionReason::InvalidGigabaseRating { player: "White" })
        );
    }

    #[test]
    fn r3_rejects_explicit_non_otb_and_inconsistent_dates_but_ignores_broadcast_hosting() {
        let policy = GigabaseOtbPolicy::publisher_trust_r3();
        let mut headers = r3_headers();
        headers.mode = Some("Correspondence");
        assert!(matches!(
            policy.evaluate_r3_headers(&headers),
            Err(PolicyRejectionReason::NonOtbGigabaseMode(_))
        ));
        headers.mode = None;
        headers.event = Some("Online Chess Championship");
        assert_eq!(
            policy.evaluate_r3_headers(&headers),
            Err(PolicyRejectionReason::ContradictoryGigabaseProvenance { field: "Event" })
        );
        headers.event = Some("Championship Broadcast");
        headers.site = Some("https://lichess.org/broadcast/event/round/game");
        assert!(policy.evaluate_r3_headers(&headers).is_ok());
        headers.site = Some("https://lichess.org/game/abcdefgh");
        assert_eq!(
            policy.evaluate_r3_headers(&headers),
            Err(PolicyRejectionReason::ContradictoryGigabaseProvenance { field: "Site" })
        );
        headers.site = Some("London");
        headers.date = Some("2021.02.29");
        assert_eq!(
            policy.evaluate_r3_headers(&headers),
            Err(PolicyRejectionReason::InvalidGigabaseDate)
        );
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
            PolicyRejectionReason::UnverifiedMastersProvenance(reason) => {
                write!(f, "Masters event lacks verified OTB provenance: {}", reason)
            }
            PolicyRejectionReason::HistoricalWithoutProvenance => {
                write!(f, "Pre-1970 game without named event and venue")
            }
            PolicyRejectionReason::UnqualifiedGigabaseSource(source) => {
                write!(f, "Gigabase source is not approved: {:?}", source)
            }
            PolicyRejectionReason::UntrustedGigabasePlayerIdentity { white, black } => write!(
                f,
                "Gigabase player identity is not Strong or Confirmed (White: {:?}, Black: {:?})",
                white, black
            ),
            PolicyRejectionReason::NonOtbGigabaseMode(mode) => {
                write!(
                    f,
                    "Gigabase Mode does not establish over-the-board play: {mode}"
                )
            }
            PolicyRejectionReason::UnverifiedGigabaseHistoricalRating(year) => write!(
                f,
                "Gigabase historical Elo before 1970 is not independently verified ({year})"
            ),
            PolicyRejectionReason::UnverifiedGigabaseDate => {
                write!(f, "Gigabase game date is absent, partial, or invalid")
            }
            PolicyRejectionReason::InvalidGigabaseRating { player } => {
                write!(f, "Gigabase {player} Elo is malformed or implausible")
            }
            PolicyRejectionReason::BelowGigabaseStrength => {
                write!(
                    f,
                    "Gigabase game does not meet an approved Masters strength rule"
                )
            }
            PolicyRejectionReason::InvalidGigabasePlayerIdentity { player } => {
                write!(f, "Gigabase {player} player name is empty or a placeholder")
            }
            PolicyRejectionReason::ConflictingGigabasePlayerIdentity { player } => {
                write!(
                    f,
                    "Gigabase {player} identity evidence is explicitly conflicting"
                )
            }
            PolicyRejectionReason::ContradictoryGigabaseProvenance { field } => {
                write!(f, "Gigabase {field} metadata contradicts OTB play")
            }
            PolicyRejectionReason::InvalidGigabaseDate => {
                write!(f, "Gigabase date has inconsistent known components")
            }
            PolicyRejectionReason::ConflictingGigabaseHeader(field) => {
                write!(f, "Gigabase PGN contains conflicting {field} header values")
            }
        }
    }
}

/// Optional source gates for reproducing Gigabase r2 or applying the publisher-trust r3 policy.
#[derive(Debug, Clone)]
pub struct GigabaseOtbPolicy {
    version: GigabasePolicyVersion,
}

#[derive(Debug, Clone)]
enum GigabasePolicyVersion {
    R2 {
        allowed_source_tags: HashSet<String>,
    },
    R3PublisherTrust,
}

/// Header fields shared by the production r3 policy and its full-source impact analysis.
#[derive(Debug, Clone, Copy)]
pub struct GigabaseHeaderMetadata<'a> {
    pub source_tag: Option<&'a str>,
    pub white_name: Option<&'a str>,
    pub black_name: Option<&'a str>,
    pub white_elo: Option<&'a str>,
    pub black_elo: Option<&'a str>,
    pub white_title: Option<&'a str>,
    pub black_title: Option<&'a str>,
    pub white_identity_evidence: Option<&'a str>,
    pub black_identity_evidence: Option<&'a str>,
    pub white_fide_id: Option<&'a str>,
    pub black_fide_id: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub date: Option<&'a str>,
    pub event: Option<&'a str>,
    pub site: Option<&'a str>,
    pub conflicting_metadata_fields: &'a [String],
}

/// Strength rule assigned in a deterministic, non-overlapping order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GigabaseStrengthRule {
    Original2200Or2400_2000,
    Added2400Vs1800,
    AddedGmVsRatedOpponent,
    AddedGmVsGm,
}

impl GigabaseStrengthRule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original2200Or2400_2000 => "original_2200_2200_or_2400_2000",
            Self::Added2400Vs1800 => "added_2400_vs_1800",
            Self::AddedGmVsRatedOpponent => "added_gm_vs_rated_opponent",
            Self::AddedGmVsGm => "added_gm_vs_gm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GigabaseDatePrecision {
    Unknown,
    Partial,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GigabaseDateInfo {
    pub year: Option<u16>,
    pub precision: GigabaseDatePrecision,
}

pub const APPROVED_GIGABASE_SOURCE_TAGS: [&str; 11] = [
    "AnaSchaigorodsky",
    "Britbase",
    "CanadianChessFederation",
    "ChessScotland",
    "DanBase",
    "Federscacchi",
    "GreekBase",
    "LichessBroadcastAPI",
    "NiederrheinischerSchachverband",
    "NiedersächsischerSchachverband",
    "SchachverbandRuhrgebiet",
];

impl GigabaseOtbPolicy {
    /// Constructs the historical r2 allowlist policy for reproducing committed evidence.
    pub fn new(allowed_source_tags: HashSet<String>) -> Self {
        Self {
            version: GigabasePolicyVersion::R2 {
                allowed_source_tags,
            },
        }
    }

    /// Constructs the current r3 policy, which trusts the archive-level OTB assertion unless
    /// record metadata contains a concrete contradiction.
    pub fn publisher_trust_r3() -> Self {
        Self {
            version: GigabasePolicyVersion::R3PublisherTrust,
        }
    }

    pub fn is_r3(&self) -> bool {
        matches!(self.version, GigabasePolicyVersion::R3PublisherTrust)
    }

    /// Applies the complete r3 header policy. Missing and partial dates remain unknown data;
    /// valid partial dates are accepted without filling in their unknown components.
    pub fn evaluate_r3_headers(
        &self,
        headers: &GigabaseHeaderMetadata<'_>,
    ) -> Result<GigabaseStrengthRule, PolicyRejectionReason> {
        if !self.is_r3() {
            return Err(PolicyRejectionReason::BelowGigabaseStrength);
        }
        if let Some(reason) = r3_header_rejections(headers).into_iter().next() {
            return Err(reason);
        }
        classify_gigabase_strength(headers)
    }

    /// Applies the retained minimum-length and starting-position checks after header
    /// qualification, without reapplying the old numeric-only strength policy.
    pub fn evaluate_r3_game(&self, game: &ParsedGame) -> Option<PolicyRejectionReason> {
        if let Some(fen) = &game.fen_start {
            if !is_standard_start_fen(fen) {
                return Some(PolicyRejectionReason::NonStandardStartingPosition(
                    fen.clone(),
                ));
            }
        }
        if game.moves.len() < 4 {
            return Some(PolicyRejectionReason::TooFewPlies {
                plies: game.moves.len(),
                minimum: 4,
            });
        }
        None
    }

    pub fn evaluate_source_metadata(
        &self,
        source: Option<&GameSourceAttribution>,
        date: Option<&str>,
    ) -> Result<(), PolicyRejectionReason> {
        let allowed_source_tags = match &self.version {
            GigabasePolicyVersion::R2 {
                allowed_source_tags,
            } => allowed_source_tags,
            GigabasePolicyVersion::R3PublisherTrust => return Ok(()),
        };
        let source_tag = source.and_then(|metadata| metadata.source_tag.as_deref());
        if source_tag.is_none_or(|tag| !allowed_source_tags.contains(tag)) {
            return Err(PolicyRejectionReason::UnqualifiedGigabaseSource(
                source_tag.map(str::to_owned),
            ));
        }

        let white_identity =
            source.and_then(|metadata| metadata.white_identity_evidence.as_deref());
        let black_identity =
            source.and_then(|metadata| metadata.black_identity_evidence.as_deref());
        if !is_trusted_gigabase_identity(white_identity)
            || !is_trusted_gigabase_identity(black_identity)
        {
            return Err(PolicyRejectionReason::UntrustedGigabasePlayerIdentity {
                white: white_identity.map(str::to_owned),
                black: black_identity.map(str::to_owned),
            });
        }

        if let Some(mode) = source.and_then(|metadata| metadata.mode.as_deref()) {
            if !is_explicit_otb_mode(mode) {
                return Err(PolicyRejectionReason::NonOtbGigabaseMode(mode.to_owned()));
            }
        }

        let Some((year, _, _)) = date.and_then(parse_complete_pgn_date) else {
            return Err(PolicyRejectionReason::UnverifiedGigabaseDate);
        };

        if year < 1970 {
            return Err(PolicyRejectionReason::UnverifiedGigabaseHistoricalRating(
                year,
            ));
        }

        Ok(())
    }
}

/// Returns every independent header-level r3 rejection reason for audit and policy-impact reports.
pub fn r3_header_rejections(headers: &GigabaseHeaderMetadata<'_>) -> Vec<PolicyRejectionReason> {
    let mut reasons = Vec::new();
    let source = headers
        .source_tag
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if source.is_none_or(is_placeholder_gigabase_value) {
        reasons.push(PolicyRejectionReason::UnqualifiedGigabaseSource(
            source.map(str::to_owned),
        ));
    }
    reasons.extend(
        headers
            .conflicting_metadata_fields
            .iter()
            .cloned()
            .map(PolicyRejectionReason::ConflictingGigabaseHeader),
    );

    let white = usable_gigabase_name(headers.white_name);
    if white.is_none() {
        reasons.push(PolicyRejectionReason::InvalidGigabasePlayerIdentity { player: "White" });
    }
    let black = usable_gigabase_name(headers.black_name);
    if black.is_none() {
        reasons.push(PolicyRejectionReason::InvalidGigabasePlayerIdentity { player: "Black" });
    }

    if is_identity_conflict(headers.white_identity_evidence) {
        reasons.push(PolicyRejectionReason::ConflictingGigabasePlayerIdentity { player: "White" });
    }
    if is_identity_conflict(headers.black_identity_evidence) {
        reasons.push(PolicyRejectionReason::ConflictingGigabasePlayerIdentity { player: "Black" });
    }

    if let (Some(white), Some(black)) = (white, black) {
        let white_fide_id = usable_identifier(headers.white_fide_id);
        let black_fide_id = usable_identifier(headers.black_fide_id);
        if normalize_identity_name(white) == normalize_identity_name(black)
            && (white_fide_id.is_none()
                || black_fide_id.is_none()
                || white_fide_id == black_fide_id)
        {
            reasons.push(PolicyRejectionReason::InvalidGigabasePlayerIdentity { player: "Both" });
        }
    }

    if parse_gigabase_date(headers.date).is_none() {
        reasons.push(PolicyRejectionReason::InvalidGigabaseDate);
    }
    if let Some(mode) = headers
        .mode
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "?")
    {
        if !is_explicit_otb_mode(mode) {
            reasons.push(PolicyRejectionReason::NonOtbGigabaseMode(mode.to_owned()));
        }
    }
    if let Some(field) = contradictory_otb_metadata(headers.event, headers.site) {
        reasons.push(PolicyRejectionReason::ContradictoryGigabaseProvenance { field });
    }
    if let Err(reason) = classify_gigabase_strength(headers) {
        reasons.push(reason);
    }
    reasons
}

/// Classifies an r3 strength rule using publisher-supplied Elo and exact `GM` title labels.
/// A malformed or implausible supplied rating is an error even when a title rule could otherwise
/// qualify the game.
pub fn classify_gigabase_strength(
    headers: &GigabaseHeaderMetadata<'_>,
) -> Result<GigabaseStrengthRule, PolicyRejectionReason> {
    let white = parse_gigabase_rating(headers.white_elo)
        .map_err(|_| PolicyRejectionReason::InvalidGigabaseRating { player: "White" })?;
    let black = parse_gigabase_rating(headers.black_elo)
        .map_err(|_| PolicyRejectionReason::InvalidGigabaseRating { player: "Black" })?;
    let white_gm = is_gm_title(headers.white_title);
    let black_gm = is_gm_title(headers.black_title);

    if white.zip(black).is_some_and(|(w, b)| {
        (w >= 2200 && b >= 2200) || (w >= 2400 && b >= 2000) || (b >= 2400 && w >= 2000)
    }) {
        return Ok(GigabaseStrengthRule::Original2200Or2400_2000);
    }
    if white
        .zip(black)
        .is_some_and(|(w, b)| (w >= 2400 && b >= 1800) || (b >= 2400 && w >= 1800))
    {
        return Ok(GigabaseStrengthRule::Added2400Vs1800);
    }
    if (white_gm && black.is_some_and(|rating| rating >= 1800))
        || (black_gm && white.is_some_and(|rating| rating >= 1800))
    {
        return Ok(GigabaseStrengthRule::AddedGmVsRatedOpponent);
    }
    if white_gm && black_gm {
        return Ok(GigabaseStrengthRule::AddedGmVsGm);
    }
    Err(PolicyRejectionReason::BelowGigabaseStrength)
}

fn parse_gigabase_rating(raw: Option<&str>) -> Result<Option<u16>, ()> {
    let Some(value) = raw.map(str::trim).filter(|value| {
        !value.is_empty()
            && !matches!(
                value.to_ascii_lowercase().as_str(),
                "?" | "-" | "n/a" | "na" | "none"
            )
    }) else {
        return Ok(None);
    };
    let rating = value.parse::<u16>().map_err(|_| ())?;
    (100..=3500)
        .contains(&rating)
        .then_some(Some(rating))
        .ok_or(())
}

fn is_gm_title(title: Option<&str>) -> bool {
    title.is_some_and(|value| value.trim().eq_ignore_ascii_case("GM"))
}

fn is_identity_conflict(evidence: Option<&str>) -> bool {
    evidence.is_some_and(|value| value.trim().eq_ignore_ascii_case("Conflict"))
}

fn usable_gigabase_name(name: Option<&str>) -> Option<&str> {
    let name = name?.trim();
    if name.is_empty() || is_placeholder_gigabase_value(name) {
        return None;
    }
    name.chars().any(char::is_alphanumeric).then_some(name)
}

fn is_placeholder_gigabase_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "?" | "-" | "." | "unknown" | "unnamed" | "anonymous" | "n/a" | "na" | "none" | "null"
    )
}

fn usable_identifier(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty() && !is_placeholder_gigabase_value(value)).then(|| value.to_ascii_lowercase())
}

fn normalize_identity_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Parses complete and partial PGN dates without manufacturing unknown components.
pub fn parse_gigabase_date(value: Option<&str>) -> Option<GigabaseDateInfo> {
    let Some(value) = value
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "?")
    else {
        return Some(GigabaseDateInfo {
            year: None,
            precision: GigabaseDatePrecision::Unknown,
        });
    };
    let bytes = value.as_bytes();
    if bytes.len() != 10 || !matches!(bytes[4], b'.' | b'-') || bytes[7] != bytes[4] {
        return None;
    }
    let parts = [value.get(0..4)?, value.get(5..7)?, value.get(8..10)?];
    let mut known = [None; 3];
    for (index, part) in parts.iter().enumerate() {
        if *part == "??" || (index == 0 && *part == "????") {
            continue;
        }
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        known[index] = Some(part.parse::<u16>().ok()?);
    }
    if parts[0] != "????" && known[0].is_none() {
        return None;
    }
    let year = known[0];
    if year.is_some_and(|year| !(1000..=2999).contains(&year)) {
        return None;
    }
    let month = known[1].map(u8::try_from).transpose().ok()?;
    if month.is_some_and(|month| !(1..=12).contains(&month)) {
        return None;
    }
    let day = known[2].map(u8::try_from).transpose().ok()?;
    if let Some(day) = day {
        if !(1..=31).contains(&day) {
            return None;
        }
        if let Some(month) = month {
            let days_in_month = match month {
                2 if year.is_none_or(is_leap_year) => 29,
                2 => 28,
                4 | 6 | 9 | 11 => 30,
                _ => 31,
            };
            if day > days_in_month {
                return None;
            }
        }
    }
    let precision = if year.is_some() && month.is_some() && day.is_some() {
        GigabaseDatePrecision::Complete
    } else if year.is_none() && month.is_none() && day.is_none() {
        GigabaseDatePrecision::Unknown
    } else {
        GigabaseDatePrecision::Partial
    };
    Some(GigabaseDateInfo { year, precision })
}

fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn contradictory_otb_metadata(event: Option<&str>, site: Option<&str>) -> Option<&'static str> {
    for (field, value) in [("Event", event), ("Site", site)] {
        let Some(value) = value
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "?")
        else {
            continue;
        };
        let lower = value.to_ascii_lowercase();
        let words = lower
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        if words
            .iter()
            .any(|word| matches!(*word, "correspondence" | "postal" | "online"))
            || words.windows(2).any(|pair| pair == ["internet", "chess"])
            || words.windows(2).any(|pair| pair == ["email", "chess"])
            || words.windows(2).any(|pair| pair == ["computer", "chess"])
            || words.windows(2).any(|pair| pair == ["engine", "only"])
            || words
                .windows(3)
                .any(|pair| pair == ["engine", "vs", "engine"])
            || words
                .windows(2)
                .any(|pair| pair == ["engine", "tournament"])
        {
            return Some(field);
        }
        if field == "Site" && is_known_online_game_url(value) {
            return Some(field);
        }
    }
    None
}

fn is_known_online_game_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if let Some((_, path)) = lower.split_once("lichess.org/") {
        let path = path.split(['?', '#']).next().unwrap_or_default();
        if path.starts_with("broadcast/") {
            return false;
        }
        let first = path.split('/').next().unwrap_or_default();
        if first == "game" || first == "analysis" {
            return true;
        }
        if first.len() == 8 && first.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return true;
        }
    }
    lower.contains("chess.com/live/game/")
        || lower.contains("chess.com/game/live/")
        || lower.contains("play.chess.com/")
}

fn parse_complete_pgn_date(value: &str) -> Option<(u16, u8, u8)> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || !matches!(bytes[4], b'.' | b'-') || bytes[7] != bytes[4] {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }

    let year = value.get(0..4)?.parse::<u16>().ok()?;
    let month = value.get(5..7)?.parse::<u8>().ok()?;
    let day = value.get(8..10)?.parse::<u8>().ok()?;
    if !(1000..=2999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }

    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days_in_month)
        .contains(&day)
        .then_some((year, month, day))
}

fn is_trusted_gigabase_identity(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value.eq_ignore_ascii_case("Strong") || value.eq_ignore_ascii_case("Confirmed")
    })
}

fn is_explicit_otb_mode(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(normalized.as_str(), "otb" | "overtheboard")
}

/// Masters OTB Inclusion Policy
#[derive(Debug, Clone)]
pub struct MastersOtbPolicy {
    pub min_both_elo: u16,
    pub min_one_elo: u16,
    pub min_other_elo: u16,
    pub allow_historical_pre_1970: bool,
    pub min_plies: usize,
    verified_online_tournament_slugs: HashSet<String>,
    verified_otb_tournament_slugs: HashSet<String>,
    require_verified_otb_event: bool,
}

impl Default for MastersOtbPolicy {
    fn default() -> Self {
        MastersOtbPolicy {
            min_both_elo: 2200,
            min_one_elo: 2400,
            min_other_elo: 2000,
            allow_historical_pre_1970: true,
            min_plies: 4,
            verified_online_tournament_slugs: HashSet::new(),
            verified_otb_tournament_slugs: HashSet::new(),
            require_verified_otb_event: false,
        }
    }
}

impl MastersOtbPolicy {
    /// Applies exact event-level classifications from the reproducible Masters audit.
    pub fn with_event_qualification(
        mut self,
        verified_online_tournament_slugs: HashSet<String>,
        verified_otb_tournament_slugs: HashSet<String>,
        require_verified_otb_event: bool,
    ) -> Self {
        self.verified_online_tournament_slugs = verified_online_tournament_slugs
            .into_iter()
            .map(|slug| slug.to_ascii_lowercase())
            .collect();
        self.verified_otb_tournament_slugs = verified_otb_tournament_slugs
            .into_iter()
            .map(|slug| slug.to_ascii_lowercase())
            .collect();
        self.require_verified_otb_event = require_verified_otb_event;
        self
    }

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

        // 3. Event-level provenance. When the audit is configured, exact tournament IDs
        // decide the event class; explicit per-game event, site, or broadcast markers still
        // reject a game from an otherwise verified tournament.
        let tournament_slug = broadcast_tournament_slug(game.metadata.broadcast_url.as_deref());
        if let Some(slug) = tournament_slug.as_deref() {
            if self.verified_online_tournament_slugs.contains(slug) {
                return PolicyVerdict::Rejected(PolicyRejectionReason::OnlineEventInMastersSource(
                    format!(
                        "official event audit classifies broadcast tournament '{slug}' as online"
                    ),
                ));
            }
        }
        if self.require_verified_otb_event {
            let Some(slug) = tournament_slug else {
                return PolicyVerdict::Rejected(
                    PolicyRejectionReason::UnverifiedMastersProvenance(
                        "game has no event-level Lichess broadcast tournament identifier".into(),
                    ),
                );
            };
            if !self.verified_otb_tournament_slugs.contains(&slug) {
                return PolicyVerdict::Rejected(
                    PolicyRejectionReason::UnverifiedMastersProvenance(format!(
                        "broadcast tournament '{slug}' is unresolved or absent from the verified OTB allowlist"
                    )),
                );
            }
            if let Some(marker) = online_event_marker(game) {
                return PolicyVerdict::Rejected(PolicyRejectionReason::OnlineEventInMastersSource(
                    marker,
                ));
            }
        } else if let Some(marker) = online_event_marker(game) {
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

/// Returns whether a PGN Termination value explicitly marks an aborted,
/// abandoned, unplayed, or rules-infraction game. Unknown values remain eligible.
pub fn is_aborted_or_infraction_termination(termination: &str) -> bool {
    const REJECTED: &[&str] = &[
        "aborted",
        "abandoned",
        "rules infraction",
        "no contest",
        "unplayed",
        "unterminated",
    ];
    REJECTED
        .iter()
        .any(|value| termination.trim().eq_ignore_ascii_case(value))
}

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

fn broadcast_tournament_slug(url: Option<&str>) -> Option<String> {
    let path = url?.split(['?', '#']).next()?;
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    while let Some(part) = parts.next() {
        if part.eq_ignore_ascii_case("broadcast") {
            return parts.next().map(str::to_ascii_lowercase);
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
                source_attribution: None,
            },
            moves: vec![CompactMove::new(12, 28, None); plies],
            fen_start: None,
        }
    }

    #[test]
    fn test_termination_classification() {
        for value in [
            "Aborted",
            "Abandoned",
            "Rules infraction",
            "No contest",
            "Unplayed",
            "Unterminated",
        ] {
            assert!(is_aborted_or_infraction_termination(value), "{value}");
        }
        for value in ["Normal", "Time forfeit", ""] {
            assert!(!is_aborted_or_infraction_termination(value), "{value}");
        }
        assert_eq!(
            early_filter_category("Aborted or rules infraction"),
            "aborted_or_infraction"
        );
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
    fn test_masters_event_qualification_uses_exact_tournament_ids() {
        let policy = MastersOtbPolicy::default().with_event_qualification(
            HashSet::from(["verified-online-7".to_string()]),
            HashSet::from(["verified-otb-12".to_string()]),
            true,
        );

        let mut otb = make_game(
            Collection::MastersOtb,
            Some(2650),
            Some(2700),
            "Open",
            "2026.03.01",
            None,
            40,
        );
        otb.metadata.broadcast_url =
            Some("https://lichess.org/broadcast/verified-otb-12/round-1/AbCd1234".into());
        assert_eq!(policy.evaluate(&otb), PolicyVerdict::Accepted);

        let mut explicit_online_site = otb.clone();
        explicit_online_site.metadata.site = Some("Internet Chess Club".into());
        assert!(matches!(
            policy.evaluate(&explicit_online_site),
            PolicyVerdict::Rejected(PolicyRejectionReason::OnlineEventInMastersSource(_))
        ));

        let mut online = otb.clone();
        online.metadata.broadcast_url =
            Some("https://lichess.org/broadcast/verified-online-7/round-3/XyZ12345".into());
        assert!(matches!(
            policy.evaluate(&online),
            PolicyVerdict::Rejected(PolicyRejectionReason::OnlineEventInMastersSource(_))
        ));

        let mut unresolved = otb;
        unresolved.metadata.broadcast_url =
            Some("https://lichess.org/broadcast/unreviewed-13/round-2/QwEr1234".into());
        assert!(matches!(
            policy.evaluate(&unresolved),
            PolicyVerdict::Rejected(PolicyRejectionReason::UnverifiedMastersProvenance(_))
        ));
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
