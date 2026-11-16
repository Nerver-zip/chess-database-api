use codb_core::{Collection, IngestionCompletionStatus};
use codb_curate::dedupe::Deduplicator;
use codb_curate::policy::GigabaseOtbPolicy;
use codb_ingest::pipeline::{CuratedPipeline, MemoryGameConsumer};
use std::collections::HashSet;
use std::io::Cursor;

const PGN_FILE_1: &str = r#"[Event "Broadcast 1"]
[Site "https://lichess.org/broadcast/tourney-1/round-1/12345"]
[Date "2024.01.10"]
[Round "1"]
[White "Carlsen, Magnus"]
[Black "Nakamura, Hikaru"]
[Result "1-0"]
[WhiteElo "2830"]
[BlackElo "2780"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Broadcast 1"]
[Site "https://lichess.org/broadcast/tourney-1/round-1/12346"]
[Date "2024.01.10"]
[Round "1"]
[White "Caruana, Fabiano"]
[Black "So, Wesley"]
[Result "1/2-1/2"]
[WhiteElo "2790"]
[BlackElo "2750"]

1. d4 Nf6 2. c4 e6 1/2-1/2
"#;

// Contains duplicate of Carlsen-Nakamura, and a superseding extended version of Caruana-So
const PGN_FILE_2: &str = r#"[Event "Broadcast 1"]
[Site "https://lichess.org/broadcast/tourney-1/round-1/12345"]
[Date "2024.01.10"]
[Round "1"]
[White "Carlsen, M."]
[Black "Nakamura, H."]
[Result "1-0"]
[WhiteElo "2830"]
[BlackElo "2780"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Broadcast 1"]
[Site "https://lichess.org/broadcast/tourney-1/round-1/12346"]
[Date "2024.01.10"]
[Round "1"]
[White "Caruana, Fabiano"]
[Black "So, Wesley"]
[Result "1/2-1/2"]
[WhiteElo "2790"]
[BlackElo "2750"]

1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 1/2-1/2
"#;

#[test]
fn test_curated_pipeline_cross_file_deduplication_and_supersession() {
    let deduplicator = Deduplicator::new();
    let consumer = MemoryGameConsumer::new();
    let mut pipeline = CuratedPipeline::new(Collection::MastersOtb, deduplicator, consumer);

    // Process File 1
    pipeline
        .process_stream(Cursor::new(PGN_FILE_1))
        .expect("Stream 1 must process successfully");
    assert_eq!(pipeline.stats.total_encountered, 2);
    assert_eq!(pipeline.stats.accepted_unique, 2);
    assert_eq!(pipeline.consumer.games.len(), 2);

    // Process File 2
    pipeline
        .process_stream(Cursor::new(PGN_FILE_2))
        .expect("Stream 2 must process successfully");
    assert_eq!(pipeline.stats.total_encountered, 4);
    assert_eq!(pipeline.stats.accepted_unique, 2);
    assert_eq!(pipeline.stats.confirmed_duplicates, 1);
    assert_eq!(pipeline.stats.superseded_prior, 1);

    // Exactly 2 retained canonical games
    assert_eq!(pipeline.consumer.games.len(), 2);
    assert!(pipeline.stats.is_reconciled());
    assert!(pipeline.stats.verify_retained_count(2));

    // Caruana-So must be the extended version (4 moves / 8 plies)
    let caruana_game = pipeline
        .consumer
        .games
        .iter()
        .find(|g| g.metadata.white.contains("Caruana"))
        .unwrap();
    assert_eq!(caruana_game.moves.len(), 6); // 1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 (6 plies)
}

#[test]
fn test_curated_pipeline_max_games_limit() {
    let deduplicator = Deduplicator::new();
    let consumer = MemoryGameConsumer::new();
    let mut pipeline = CuratedPipeline::new(Collection::MastersOtb, deduplicator, consumer)
        .with_max_games(Some(1));

    pipeline
        .process_stream(Cursor::new(PGN_FILE_1))
        .expect("Stream must stop cleanly at limit");

    assert_eq!(pipeline.stats.accepted_unique, 1);
    assert_eq!(pipeline.consumer.games.len(), 1);
    assert_eq!(
        pipeline.stats.completion_status,
        IngestionCompletionStatus::PartialLimitReached
    );
}

#[test]
fn test_gigabase_policy_reconciles_source_identity_mode_and_history_rejections() {
    let pgn = r#"[Event "Open A"]
[Site "City"]
[Date "2024.01.01"]
[White "Player A"]
[Black "Player B"]
[WhiteElo "2400"]
[BlackElo "2200"]
[Source "Britbase"]
[IdentityEvidenceWhite "Confirmed"]
[IdentityEvidenceBlack "Strong"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Open B"]
[Site "City"]
[Date "2024.01.01"]
[White "Player C"]
[Black "Player D"]
[WhiteElo "2400"]
[BlackElo "2200"]
[Source "Historical Archive"]
[IdentityEvidenceWhite "Confirmed"]
[IdentityEvidenceBlack "Strong"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Open C"]
[Site "City"]
[Date "2024.01.01"]
[White "Player E"]
[Black "Player F"]
[WhiteElo "2400"]
[BlackElo "2200"]
[Source "Britbase"]
[IdentityEvidenceWhite "NoMatch"]
[IdentityEvidenceBlack "Confirmed"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Open D"]
[Site "City"]
[Date "2024.01.01"]
[White "Player G"]
[Black "Player H"]
[WhiteElo "2400"]
[BlackElo "2200"]
[Source "Britbase"]
[IdentityEvidenceWhite "Confirmed"]
[IdentityEvidenceBlack "Confirmed"]
[Mode "Correspondence"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0

[Event "Open E"]
[Site "City"]
[Date "1968.01.01"]
[White "Player I"]
[Black "Player J"]
[WhiteElo "2400"]
[BlackElo "2200"]
[Source "Britbase"]
[IdentityEvidenceWhite "Confirmed"]
[IdentityEvidenceBlack "Confirmed"]
[Result "1-0"]

1. e4 e5 2. Nf3 Nc6 1-0
"#;

    let policy = GigabaseOtbPolicy::new(HashSet::from(["Britbase".to_owned()]));
    let mut pipeline = CuratedPipeline::new(
        Collection::MastersOtb,
        Deduplicator::new(),
        MemoryGameConsumer::new(),
    )
    .with_gigabase_source_qualification(policy);

    pipeline.process_stream(Cursor::new(pgn)).unwrap();

    assert_eq!(pipeline.stats.total_encountered, 5);
    assert_eq!(pipeline.stats.accepted_unique, 1);
    assert_eq!(pipeline.stats.rejected_by_policy, 4);
    assert_eq!(pipeline.policy_rejection_reasons["unqualified_source"], 1);
    assert_eq!(
        pipeline.policy_rejection_reasons["untrusted_player_identity"],
        1
    );
    assert_eq!(pipeline.policy_rejection_reasons["non_otb_play_mode"], 1);
    assert_eq!(
        pipeline.policy_rejection_reasons["unverified_historical_rating"],
        1
    );
    assert!(pipeline.stats.is_reconciled());
    assert!(pipeline
        .stats
        .verify_retained_count(pipeline.consumer.games.len()));
    assert_eq!(pipeline.consumer.games.len(), 1);
}

#[test]
fn test_gigabase_r3_policy_rules_identity_dates_modes_and_attribution() {
    #[allow(clippy::too_many_arguments)]
    fn record(
        event: &str,
        white: &str,
        black: &str,
        white_elo: Option<&str>,
        black_elo: Option<&str>,
        white_title: Option<&str>,
        black_title: Option<&str>,
        identity_white: &str,
        identity_black: &str,
        source: &str,
        mode: Option<&str>,
        date: &str,
    ) -> String {
        let optional_tag = |name: &str, value: Option<&str>| {
            value
                .map(|value| format!("[{name} \"{value}\"]\n"))
                .unwrap_or_default()
        };
        format!(
            "[Event \"{event}\"]\n[Site \"London\"]\n[Date \"{date}\"]\n\
             [White \"{white}\"]\n[Black \"{black}\"]\n\
             {}{}{}{}[IdentityEvidenceWhite \"{identity_white}\"]\n\
             [IdentityEvidenceBlack \"{identity_black}\"]\n[Source \"{source}\"]\n\
             {}[Result \"1-0\"]\n\n1. e4 e5 2. Nf3 Nc6 1-0\n\n",
            optional_tag("WhiteElo", white_elo),
            optional_tag("BlackElo", black_elo),
            optional_tag("WhiteTitle", white_title),
            optional_tag("BlackTitle", black_title),
            optional_tag("Mode", mode),
        )
    }

    let conflicting_source = record(
        "Conflicting Source",
        "Player U",
        "Player V",
        Some("2300"),
        Some("2300"),
        None,
        None,
        "Confirmed",
        "Confirmed",
        "Britbase",
        None,
        "2024.01.01",
    )
    .replace(
        "[Source \"Britbase\"]\n",
        "[Source \"Britbase\"]\n[Source \"Legacy Software Dump\"]\n",
    );
    let pgn = [
        record(
            "Historical Open",
            "Player A",
            "Player B",
            Some("2250"),
            Some("2210"),
            None,
            None,
            "NoMatch",
            "Probable",
            "Historical Archive",
            None,
            "1968.??.??",
        ),
        record(
            "Rating Boundary",
            "Player C",
            "Player D",
            Some("1800"),
            Some("2500"),
            None,
            None,
            "Ambiguous",
            "NoMatch",
            "Legacy Software Dump",
            None,
            "2024.01.01",
        ),
        record(
            "GM Rated",
            "Player E",
            "Player F",
            None,
            Some("1900"),
            Some("GM"),
            None,
            "NoMatch",
            "Strong",
            "Community Project",
            None,
            "2024.01.01",
        ),
        record(
            "GM Match",
            "Player G",
            "Player H",
            None,
            None,
            Some("GM"),
            Some("GM"),
            "Probable",
            "Ambiguous",
            "Independent Collection",
            None,
            "2024.??.??",
        ),
        record(
            "Known OTB",
            "Player I",
            "Player J",
            Some("2200"),
            Some("2200"),
            None,
            None,
            "Confirmed",
            "Strong",
            "Britbase",
            Some("OTB"),
            "?",
        ),
        record(
            "Below Boundary",
            "Player K",
            "Player L",
            Some("2400"),
            Some("1799"),
            Some("WGM"),
            Some("IM"),
            "Confirmed",
            "Confirmed",
            "Britbase",
            None,
            "2024.01.01",
        ),
        record(
            "Correspondence",
            "Player M",
            "Player N",
            Some("2300"),
            Some("2300"),
            None,
            None,
            "Confirmed",
            "Confirmed",
            "Britbase",
            Some("Correspondence"),
            "2024.01.01",
        ),
        record(
            "Identity Conflict",
            "Player O",
            "Player P",
            Some("2300"),
            Some("2300"),
            None,
            None,
            "Conflict",
            "Confirmed",
            "Britbase",
            None,
            "2024.01.01",
        ),
        record(
            "Online Chess Event",
            "Player Q",
            "Player R",
            Some("2300"),
            Some("2300"),
            None,
            None,
            "Confirmed",
            "Confirmed",
            "Britbase",
            None,
            "2024.01.01",
        ),
        record(
            "Invalid Elo",
            "Player S",
            "Player T",
            Some("9999"),
            Some("1900"),
            Some("GM"),
            None,
            "Confirmed",
            "Confirmed",
            "Britbase",
            None,
            "2024.01.01",
        ),
        conflicting_source,
    ]
    .join("");
    let policy = GigabaseOtbPolicy::publisher_trust_r3();
    let mut pipeline = CuratedPipeline::new(
        Collection::MastersOtb,
        Deduplicator::new(),
        MemoryGameConsumer::new(),
    )
    .with_gigabase_source_qualification(policy);

    pipeline.process_stream(Cursor::new(pgn)).unwrap();

    assert_eq!(pipeline.stats.total_encountered, 11);
    assert_eq!(pipeline.stats.accepted_unique, 5);
    assert_eq!(pipeline.stats.rejected_by_policy, 6);
    assert_eq!(
        pipeline.policy_rejection_reasons["below_rating_threshold"],
        1
    );
    assert_eq!(pipeline.policy_rejection_reasons["non_otb_play_mode"], 1);
    assert_eq!(
        pipeline.policy_rejection_reasons["conflicting_player_identity"],
        1
    );
    assert_eq!(
        pipeline.policy_rejection_reasons["contradictory_otb_evidence"],
        1
    );
    assert_eq!(pipeline.policy_rejection_reasons["invalid_rating"], 1);
    assert_eq!(
        pipeline.policy_rejection_reasons["conflicting_source_metadata"],
        1
    );
    assert!(pipeline.stats.is_reconciled());
    assert!(pipeline
        .stats
        .verify_retained_count(pipeline.consumer.games.len()));
    let gm_game = pipeline
        .consumer
        .games
        .iter()
        .find(|game| game.metadata.white == "Player E")
        .unwrap();
    let attribution = gm_game.metadata.source_attribution.as_ref().unwrap();
    assert_eq!(attribution.white_title.as_deref(), Some("GM"));
    assert_eq!(
        attribution.white_identity_evidence.as_deref(),
        Some("NoMatch")
    );
    assert!(pipeline
        .consumer
        .games
        .iter()
        .any(|game| game.metadata.date.as_deref() == Some("?")));
}

#[test]
fn test_gigabase_rating_boundaries_and_explicit_otb_mode() {
    struct Case {
        name: &'static str,
        white_elo: Option<&'static str>,
        black_elo: Option<&'static str>,
        mode: Option<&'static str>,
        expected_rejection: Option<&'static str>,
    }

    let cases = [
        Case {
            name: "both_at_2200",
            white_elo: Some("2200"),
            black_elo: Some("2200"),
            mode: None,
            expected_rejection: None,
        },
        Case {
            name: "white_one_below_both_threshold",
            white_elo: Some("2199"),
            black_elo: Some("2200"),
            mode: None,
            expected_rejection: Some("below_rating_threshold"),
        },
        Case {
            name: "black_one_below_both_threshold",
            white_elo: Some("2200"),
            black_elo: Some("2199"),
            mode: None,
            expected_rejection: Some("below_rating_threshold"),
        },
        Case {
            name: "white_at_2400_black_at_2000",
            white_elo: Some("2400"),
            black_elo: Some("2000"),
            mode: None,
            expected_rejection: None,
        },
        Case {
            name: "white_one_below_2400_exception",
            white_elo: Some("2399"),
            black_elo: Some("2000"),
            mode: None,
            expected_rejection: Some("below_rating_threshold"),
        },
        Case {
            name: "black_one_below_2000_exception",
            white_elo: Some("2400"),
            black_elo: Some("1999"),
            mode: None,
            expected_rejection: Some("below_rating_threshold"),
        },
        Case {
            name: "reverse_color_2400_exception",
            white_elo: Some("2000"),
            black_elo: Some("2400"),
            mode: None,
            expected_rejection: None,
        },
        Case {
            name: "missing_white_rating",
            white_elo: None,
            black_elo: Some("2200"),
            mode: None,
            expected_rejection: Some("missing_rating"),
        },
        Case {
            name: "missing_black_rating",
            white_elo: Some("2400"),
            black_elo: None,
            mode: None,
            expected_rejection: Some("missing_rating"),
        },
        Case {
            name: "unparseable_rating",
            white_elo: Some("unrated"),
            black_elo: Some("2400"),
            mode: None,
            expected_rejection: Some("missing_rating"),
        },
        Case {
            name: "explicit_otb_mode",
            white_elo: Some("2200"),
            black_elo: Some("2200"),
            mode: Some("OTB"),
            expected_rejection: None,
        },
    ];

    for case in cases {
        let white_elo = case
            .white_elo
            .map(|value| format!("[WhiteElo \"{value}\"]\n"))
            .unwrap_or_default();
        let black_elo = case
            .black_elo
            .map(|value| format!("[BlackElo \"{value}\"]\n"))
            .unwrap_or_default();
        let mode = case
            .mode
            .map(|value| format!("[Mode \"{value}\"]\n"))
            .unwrap_or_default();
        let pgn = format!(
            "[Event \"Boundary {}\"]\n\
             [Site \"London ENG\"]\n\
             [Date \"2024.06.12\"]\n\
             [Round \"1\"]\n\
             [White \"White {}\"]\n\
             [Black \"Black {}\"]\n\
             {white_elo}{black_elo}\
             [Source \"Britbase\"]\n\
             [IdentityEvidenceWhite \"Strong\"]\n\
             [IdentityEvidenceBlack \"Confirmed\"]\n\
             {mode}[Result \"1-0\"]\n\n\
             1. e4 e5 2. Nf3 Nc6 1-0\n\n",
            case.name, case.name, case.name
        );

        let policy = GigabaseOtbPolicy::new(HashSet::from(["Britbase".to_owned()]));
        let mut pipeline = CuratedPipeline::new(
            Collection::MastersOtb,
            Deduplicator::new(),
            MemoryGameConsumer::new(),
        )
        .with_gigabase_source_qualification(policy);

        pipeline
            .process_stream(Cursor::new(pgn))
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));

        assert_eq!(pipeline.stats.total_encountered, 1, "{}", case.name);
        assert!(pipeline.stats.is_reconciled(), "{}", case.name);
        assert!(pipeline
            .stats
            .verify_retained_count(pipeline.consumer.games.len()));
        if let Some(category) = case.expected_rejection {
            assert_eq!(pipeline.stats.accepted_unique, 0, "{}", case.name);
            assert_eq!(pipeline.stats.rejected_by_policy, 1, "{}", case.name);
            assert_eq!(
                pipeline.policy_rejection_reasons.get(category),
                Some(&1),
                "{}",
                case.name
            );
            assert!(pipeline.consumer.games.is_empty(), "{}", case.name);
        } else {
            assert_eq!(pipeline.stats.accepted_unique, 1, "{}", case.name);
            assert_eq!(pipeline.stats.rejected_by_policy, 0, "{}", case.name);
            assert_eq!(pipeline.consumer.games.len(), 1, "{}", case.name);
        }
    }
}
