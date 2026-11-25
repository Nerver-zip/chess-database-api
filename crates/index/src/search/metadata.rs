//! Search projections for canonical game metadata.

use unicode_normalization::UnicodeNormalization;

/// Normalize a name for matching while keeping display text in the source metadata.
///
/// Compatibility characters are folded with NFKC, letters are lowercased, and
/// runs of Unicode whitespace become one ASCII space. Punctuation is preserved.
pub fn normalize_name_search(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;

    for character in value.nfkc().flat_map(char::to_lowercase) {
        if character.is_whitespace() {
            pending_space = !normalized.is_empty();
        } else {
            if pending_space {
                normalized.push(' ');
                pending_space = false;
            }
            normalized.push(character);
        }
    }

    normalized
}

/// Inclusive calendar bounds for a complete or partial PGN date.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DateBounds {
    pub lower_yyyymmdd: u32,
    pub upper_yyyymmdd: u32,
}

/// Parse a PGN `Date` value such as `2024.02.29` or `2024.02.??`.
///
/// Unknown dates and malformed or impossible dates return `None`. Partial
/// dates must have unknown components at the end so their inclusive range is
/// unambiguous.
pub fn parse_pgn_date_bounds(value: &str) -> Option<DateBounds> {
    let mut components = value.split('.');
    let year_text = components.next()?;
    let month_text = components.next()?;
    let day_text = components.next()?;
    if components.next().is_some() || year_text.len() != 4 {
        return None;
    }

    let year = parse_date_component(year_text)?;
    let month = parse_date_component(month_text)?;
    let day = parse_date_component(day_text)?;
    let year = year?;

    match (month, day) {
        (None, None) => Some(DateBounds {
            lower_yyyymmdd: year * 10_000 + 101,
            upper_yyyymmdd: year * 10_000 + 12_31,
        }),
        (Some(month), None) => {
            if !(1..=12).contains(&month) {
                return None;
            }
            Some(DateBounds {
                lower_yyyymmdd: year * 10_000 + month * 100 + 1,
                upper_yyyymmdd: year * 10_000 + month * 100 + days_in_month(year, month)?,
            })
        }
        (Some(month), Some(day)) => {
            if day == 0 || day > days_in_month(year, month)? {
                return None;
            }
            let date = year * 10_000 + month * 100 + day;
            Some(DateBounds {
                lower_yyyymmdd: date,
                upper_yyyymmdd: date,
            })
        }
        (None, Some(_)) => None,
    }
}

fn parse_date_component(value: &str) -> Option<Option<u32>> {
    if value == "??" {
        Some(None)
    } else if matches!(value.len(), 2 | 4) && value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse().ok().map(Some)
    } else {
        None
    }
}

fn days_in_month(year: u32, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if year.checked_rem(400) == Some(0)
            || (year.checked_rem(4) == Some(0) && year.checked_rem(100) != Some(0)) =>
        {
            Some(29)
        }
        2 => Some(28),
        _ => None,
    }
}

/// One side's source-attribution values used for conservative FIDE identity projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlayerIdentityEvidence<'a> {
    pub fide_id: Option<&'a str>,
    pub identity_evidence: Option<&'a str>,
}

/// Derive a FIDE ID only when high-confidence evidence agrees across sources.
///
/// Every syntactically valid decimal ID participates in conflict detection,
/// including IDs attached to weaker evidence. At least one `Confirmed` or
/// `Strong` record must support the sole ID. Source attribution values remain
/// unchanged in their original sidecar records.
pub fn derive_fide_id<'a>(
    evidence: impl IntoIterator<Item = PlayerIdentityEvidence<'a>>,
) -> Option<String> {
    let mut candidate: Option<String> = None;
    let mut supported = false;

    for record in evidence {
        let Some(id) = record.fide_id.and_then(normalize_decimal_id) else {
            continue;
        };

        if candidate.as_ref().is_some_and(|existing| existing != &id) {
            return None;
        }
        candidate.get_or_insert_with(|| id.clone());

        let trusted_evidence = record.identity_evidence.is_some_and(|label| {
            label.eq_ignore_ascii_case("confirmed") || label.eq_ignore_ascii_case("strong")
        });
        supported |= trusted_evidence;
    }

    candidate.filter(|_| supported)
}

fn normalize_decimal_id(value: &str) -> Option<String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let canonical = value.trim_start_matches('0');
    if canonical.is_empty() {
        None
    } else {
        Some(canonical.to_owned())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeControlSpeed {
    Unknown,
    OtherFormat,
    Bullet,
    Blitz,
    Rapid,
    Classical,
}

impl TimeControlSpeed {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::OtherFormat => "other_format",
            Self::Bullet => "bullet",
            Self::Blitz => "blitz",
            Self::Rapid => "rapid",
            Self::Classical => "classical",
        }
    }
}

/// Classify a simple `base+increment` control using `base + 40 * increment`.
///
/// Bounds follow the curated collection contract: below 180 seconds is bullet,
/// below 600 is blitz, below 1800 is rapid, and 1800 seconds or more is
/// classical. Missing values remain `Unknown`; unsupported or overflowing
/// formats remain `OtherFormat`.
pub fn classify_time_control(value: Option<&str>) -> TimeControlSpeed {
    let Some(value) = value else {
        return TimeControlSpeed::Unknown;
    };
    let value = value.trim();
    if value.is_empty() || value == "?" || value == "-" {
        return TimeControlSpeed::Unknown;
    }
    let Some((base, increment)) = value.split_once('+') else {
        return TimeControlSpeed::OtherFormat;
    };
    let (Ok(base), Ok(increment)) = (base.parse::<u32>(), increment.parse::<u32>()) else {
        return TimeControlSpeed::OtherFormat;
    };
    let Some(estimated_seconds) = increment
        .checked_mul(40)
        .and_then(|extra| base.checked_add(extra))
    else {
        return TimeControlSpeed::OtherFormat;
    };

    match estimated_seconds {
        0..180 => TimeControlSpeed::Bullet,
        180..600 => TimeControlSpeed::Blitz,
        600..1800 => TimeControlSpeed::Rapid,
        _ => TimeControlSpeed::Classical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_unicode_case_and_whitespace_without_erasing_punctuation() {
        assert_eq!(
            normalize_name_search("  Ａda   Lovelace—Šah  "),
            "ada lovelace—šah"
        );
        assert_ne!(
            normalize_name_search("O'Neil"),
            normalize_name_search("ONeil")
        );
        assert_eq!(normalize_name_search("   "), "");
    }

    #[test]
    fn parses_full_partial_and_leap_year_dates_to_inclusive_bounds() {
        assert_eq!(
            parse_pgn_date_bounds("2024.02.29"),
            Some(DateBounds {
                lower_yyyymmdd: 20240229,
                upper_yyyymmdd: 20240229,
            })
        );
        assert_eq!(
            parse_pgn_date_bounds("2023.02.??"),
            Some(DateBounds {
                lower_yyyymmdd: 20230201,
                upper_yyyymmdd: 20230228,
            })
        );
        assert_eq!(
            parse_pgn_date_bounds("2024.??.??"),
            Some(DateBounds {
                lower_yyyymmdd: 20240101,
                upper_yyyymmdd: 20241231,
            })
        );
        assert_eq!(parse_pgn_date_bounds("2023.02.29"), None);
        assert_eq!(parse_pgn_date_bounds("2024.??.01"), None);
        assert_eq!(parse_pgn_date_bounds("????.??.??"), None);
    }

    #[test]
    fn derives_only_consistent_high_confidence_decimal_player_ids() {
        let evidence = [
            PlayerIdentityEvidence {
                fide_id: Some("001234"),
                identity_evidence: Some("Strong"),
            },
            PlayerIdentityEvidence {
                fide_id: Some("1234"),
                identity_evidence: Some("Confirmed"),
            },
        ];
        assert_eq!(derive_fide_id(evidence), Some("1234".to_owned()));

        let conflict = [
            evidence[0],
            PlayerIdentityEvidence {
                fide_id: Some("9999"),
                identity_evidence: Some("Unmatched"),
            },
        ];
        assert_eq!(derive_fide_id(conflict), None);

        assert_eq!(
            derive_fide_id([PlayerIdentityEvidence {
                fide_id: Some("1234"),
                identity_evidence: Some("Possible"),
            }]),
            None
        );
    }

    #[test]
    fn classifies_time_controls_at_documented_boundaries_and_keeps_unknown_distinct() {
        assert_eq!(classify_time_control(None), TimeControlSpeed::Unknown);
        assert_eq!(classify_time_control(Some("?")), TimeControlSpeed::Unknown);
        assert_eq!(
            classify_time_control(Some("179+0")),
            TimeControlSpeed::Bullet
        );
        assert_eq!(
            classify_time_control(Some("180+0")),
            TimeControlSpeed::Blitz
        );
        assert_eq!(
            classify_time_control(Some("600+0")),
            TimeControlSpeed::Rapid
        );
        assert_eq!(
            classify_time_control(Some("1800+0")),
            TimeControlSpeed::Classical
        );
        assert_eq!(
            classify_time_control(Some("40/900:0")),
            TimeControlSpeed::OtherFormat
        );
        assert_eq!(
            classify_time_control(Some("4294967295+4294967295")),
            TimeControlSpeed::OtherFormat
        );
    }
}
