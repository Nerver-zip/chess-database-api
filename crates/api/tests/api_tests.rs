//! Integration, contract, concurrency, and equivalence tests for `codb-api`.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use codb_api::config::ServerConfig;
use codb_api::models::{CollectionsResponse, HealthResponse, PositionResponse, ReadyResponse};
use codb_api::routes::create_router;
use codb_api::state::AppState;
use codb_api::ErrorEnvelope;
use codb_core::{Collection, CompactMove, GameMetadata, GameResult, IngestionStats, ParsedGame};
use codb_storage::{
    build_dataset_position_index, DatasetIndexConfig, DatasetWriter, DatasetWriterConfig,
    SourceArchiveMetadata,
};
use http_body_util::BodyExt;
use std::path::Path;
use tempfile::tempdir;
use tower::ServiceExt;

/// Helper to create mock test games.
fn make_test_game(id: u64, collection: Collection, moves: Vec<CompactMove>) -> ParsedGame {
    ParsedGame {
        metadata: GameMetadata {
            id,
            collection,
            white: format!("Grandmaster White {}", id),
            black: format!("Grandmaster Black {}", id),
            white_elo: Some(2700),
            black_elo: Some(2680),
            date: Some("2025.01.15".into()),
            event: Some("World Championship Candidate Match".into()),
            site: Some("Reykjavik ISL".into()),
            round: Some("1".into()),
            result: if id % 2 == 0 {
                GameResult::WhiteWins
            } else {
                GameResult::Draw
            },
            eco: Some("C50".into()),
            time_control: Some("5400+30".into()),
            source_id: Some(format!("https://example.com/games/{}", id)),
            broadcast_url: None,
        },
        moves,
        fen_start: None,
    }
}

/// Helper to build and index a persistent test dataset with CPI.
fn setup_test_dataset(dir: &Path, collection: Collection, num_games: usize) {
    let config = DatasetWriterConfig {
        collection,
        block_capacity: 4,
        max_games_per_shard: 10,
        zstd_level: 3,
        ..Default::default()
    };

    let mut writer = DatasetWriter::new(dir.to_path_buf(), config).unwrap();
    let e2e4 = CompactMove::new(12, 28, None); // e2 -> e4
    let e7e5 = CompactMove::new(52, 36, None); // e7 -> e5
    let g1f3 = CompactMove::new(6, 21, None); // g1 -> f3
    let b8c6 = CompactMove::new(57, 42, None); // b8 -> c6
    let f1c4 = CompactMove::new(5, 26, None); // f1 -> c4
    let d2d4 = CompactMove::new(11, 27, None); // d2 -> d4

    for i in 1..=num_games {
        let moves = if i % 3 == 0 {
            vec![e2e4, e7e5, d2d4]
        } else {
            vec![e2e4, e7e5, g1f3, b8c6, f1c4]
        };
        let game = make_test_game(i as u64, collection, moves);
        writer.push_game(game).unwrap();
    }

    let stats = IngestionStats {
        total_encountered: num_games as u64,
        accepted_unique: num_games as u64,
        ..Default::default()
    };
    let sources = vec![SourceArchiveMetadata {
        filename: "test_archive.pgn.zst".into(),
        url: None,
        sha256: Some("00".repeat(32)),
        size_bytes: 2048,
        license: "CC0-1.0".into(),
        checksum_verified: Some(true),
    }];

    writer.finalize(stats, sources).unwrap();

    // Build CPI v2 position index
    let index_cfg = DatasetIndexConfig {
        depth_plies: Some(40),
        expected_collection: Some(collection),
        memory_limit_bytes: 16 * 1024 * 1024,
        max_merge_fan_in: 64,
        positions_per_block: 64,
        zstd_level: 3,
        build_id_override: None,
    };
    build_dataset_position_index(dir, index_cfg).unwrap();
}

async fn response_json<T: serde::de::DeserializeOwned>(resp: axum::response::Response) -> T {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON ({}): {}",
            e,
            String::from_utf8_lossy(&body)
        )
    })
}

async fn response_text(resp: axum::response::Response) -> String {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
}

#[tokio::test]
async fn test_health_and_readiness_probes() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 12);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // 1. GET /health
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let health: HealthResponse = response_json(resp).await;
    assert_eq!(health.status, "ok");

    // 2. GET /ready (ready with masters-otb)
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ready: ReadyResponse = response_json(resp).await;
    assert_eq!(ready.status, "ready");
    assert_eq!(ready.collections_loaded, vec!["masters-otb"]);
}

#[tokio::test]
async fn test_collections_metadata_endpoint() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    let elite_dir = tmp.path().join("elite");

    setup_test_dataset(&masters_dir, Collection::MastersOtb, 8);
    setup_test_dataset(&elite_dir, Collection::EliteOnline, 6);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: Some(elite_dir),
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    let req = Request::builder()
        .uri("/v1/collections")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let meta: CollectionsResponse = response_json(resp).await;
    assert_eq!(meta.collections.len(), 2);

    let masters_meta = meta
        .collections
        .iter()
        .find(|c| c.collection == "masters-otb")
        .unwrap();
    assert_eq!(masters_meta.total_games, 8);
    assert!(masters_meta.cpi_available);
    assert_eq!(masters_meta.cpi_depth_plies, Some(40));

    let elite_meta = meta
        .collections
        .iter()
        .find(|c| c.collection == "elite-online")
        .unwrap();
    assert_eq!(elite_meta.total_games, 6);
    assert!(elite_meta.cpi_available);
}

#[tokio::test]
async fn test_explorer_position_by_moves_and_fen() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 15);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Query e4, e5, Nf3
    let uri = "/v1/explorer/position?collection=masters-otb&moves=e4,e5,Nf3";
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let pos: PositionResponse = response_json(resp).await;
    assert_eq!(pos.collection, "masters-otb");
    assert!(pos.total_games > 0);
    assert_eq!(pos.total_games, pos.white_wins + pos.draws + pos.black_wins);
    assert!(!pos.moves.is_empty());

    // Check candidate move formatting
    let top_move = &pos.moves[0];
    assert_eq!(top_move.uci, "b8c6");
    assert_eq!(top_move.san, "Nc6");
    assert!(top_move.total_games > 0);

    // Query same position using direct FEN
    let fen_encoded = pos.fen.replace(' ', "%20");
    let fen_uri = format!(
        "/v1/explorer/position?collection=masters-otb&fen={}",
        fen_encoded
    );
    let req_fen = Request::builder()
        .uri(&fen_uri)
        .body(Body::empty())
        .unwrap();
    let resp_fen = app.clone().oneshot(req_fen).await.unwrap();
    assert_eq!(resp_fen.status(), StatusCode::OK);
    let pos_fen: PositionResponse = response_json(resp_fen).await;
    assert_eq!(pos, pos_fen);

    // Query legal position absent from corpus
    let absent_uri = "/v1/explorer/position?collection=masters-otb&moves=h4,a5,h5,h6,Rh3";
    let req_absent = Request::builder()
        .uri(absent_uri)
        .body(Body::empty())
        .unwrap();
    let resp_absent = app.oneshot(req_absent).await.unwrap();
    assert_eq!(resp_absent.status(), StatusCode::OK);
    let pos_absent: PositionResponse = response_json(resp_absent).await;
    assert_eq!(pos_absent.total_games, 0);
    assert_eq!(pos_absent.moves.len(), 0);
}

#[tokio::test]
async fn test_input_validation_and_unsupported_parameters() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 6);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        max_move_plies: 3,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // 1. Missing collection parameter
    let req = Request::builder()
        .uri("/v1/explorer/position?moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "invalid_collection");

    // 2. Collection not loaded on server
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=elite-online&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "collection_unavailable");

    // 3. Missing both FEN and moves
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "missing_position");

    // 4. Ambiguous: both FEN and moves provided
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&fen=rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR%20w%20KQkq%20-%200%201&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "ambiguous_position");

    // 5. Unsupported parameters: since_year
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4&since_year=2020")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "unsupported_parameter");

    // 6. Unsupported parameters: min_rating
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4&min_rating=2500")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "unsupported_parameter");

    // 7. Invalid FEN syntax
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&fen=not_a_chess_fen")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "invalid_fen");

    // 8. Illegal move sequence
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4,e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "invalid_moves");

    // 9. Move plies limit exceeded (configured max 3)
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4,e5,Nf3,Nc6")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "move_sequence_too_long");
}

#[tokio::test]
async fn test_game_retrieval_endpoint() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 10);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // 1. Valid game retrieval
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/1")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/x-chess-pgn; charset=utf-8"
    );

    let pgn = response_text(resp).await;
    assert!(pgn.contains("[White \"Grandmaster White 1\"]"));
    assert!(pgn.contains("[Black \"Grandmaster Black 1\"]"));
    assert!(pgn.contains("1. e4 e5"));

    // 2. Non-existent game ID -> 404
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/999999")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "game_not_found");

    // 3. Invalid non-numeric ID -> 400
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/abc")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "invalid_game_id");

    // 4. Invalid collection in path -> 400
    let req = Request::builder()
        .uri("/v1/collections/unknown-col/games/1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "invalid_collection");
}

#[tokio::test]
async fn test_concurrent_position_and_game_queries() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 20);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    let mut handles = Vec::new();

    // Spawn 25 concurrent position queries and 25 concurrent game retrievals
    for i in 0..25 {
        let app_clone = app.clone();
        handles.push(tokio::spawn(async move {
            let moves = if i % 2 == 0 { "e4,e5" } else { "e4,e5,Nf3" };
            let uri = format!(
                "/v1/explorer/position?collection=masters-otb&moves={}",
                moves
            );
            let req = Request::builder().uri(&uri).body(Body::empty()).unwrap();
            let resp = app_clone.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let pos: PositionResponse = response_json(resp).await;
            assert!(pos.total_games > 0);
        }));

        let app_clone_game = app.clone();
        handles.push(tokio::spawn(async move {
            let game_id = (i % 20) + 1;
            let uri = format!("/v1/collections/masters-otb/games/{}", game_id);
            let req = Request::builder().uri(&uri).body(Body::empty()).unwrap();
            let resp = app_clone_game.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let pgn = response_text(resp).await;
            assert!(
                pgn.contains(&format!("White_{}", game_id))
                    || pgn.contains(&format!("Grandmaster White {}", game_id))
            );
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn test_snapshot_reload_and_resilience() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 10);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Initial query
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let pos_initial: PositionResponse = response_json(resp).await;
    assert_eq!(pos_initial.total_games, 10);

    // Now reload snapshots via admin endpoint
    let reload_req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .body(Body::empty())
        .unwrap();
    let reload_resp = app.clone().oneshot(reload_req).await.unwrap();
    assert_eq!(reload_resp.status(), StatusCode::OK);

    // Verify service still serves queries correctly after reload
    let req_after = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp_after = app.oneshot(req_after).await.unwrap();
    assert_eq!(resp_after.status(), StatusCode::OK);
    let pos_after: PositionResponse = response_json(resp_after).await;
    assert_eq!(pos_after.total_games, 10);
}

#[tokio::test]
async fn test_transposition_equivalence() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 10);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Sequence 1: 1. e4 e5 2. Nf3 Nc6
    let req1 = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4,e5,Nf3,Nc6")
        .body(Body::empty())
        .unwrap();
    let resp1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let pos1: PositionResponse = response_json(resp1).await;

    // Sequence 2: 1. Nf3 Nc6 2. e4 e5 (Transposition)
    let req2 = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=Nf3,Nc6,e4,e5")
        .body(Body::empty())
        .unwrap();
    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let pos2: PositionResponse = response_json(resp2).await;

    // The canonical 128-bit Zobrist position keys must match identically for transpositions
    assert_eq!(pos1.position_key, pos2.position_key);
    assert_eq!(pos1.total_games, pos2.total_games);
    assert_eq!(pos1.moves, pos2.moves);

    // Board position (pieces, turn, castling, en-passant) is identical
    let pos1_board_parts: Vec<&str> = pos1.fen.split_whitespace().take(4).collect();
    let pos2_board_parts: Vec<&str> = pos2.fen.split_whitespace().take(4).collect();
    assert_eq!(pos1_board_parts, pos2_board_parts);
}

#[tokio::test]
async fn test_collection_segregation_and_isolation() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    let elite_dir = tmp.path().join("elite");

    setup_test_dataset(&masters_dir, Collection::MastersOtb, 15);
    setup_test_dataset(&elite_dir, Collection::EliteOnline, 5);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: Some(elite_dir),
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Query masters
    let req_m = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp_m = app.clone().oneshot(req_m).await.unwrap();
    assert_eq!(resp_m.status(), StatusCode::OK);
    let pos_m: PositionResponse = response_json(resp_m).await;
    assert_eq!(pos_m.total_games, 15);

    // Query elite
    let req_e = Request::builder()
        .uri("/v1/explorer/position?collection=elite-online&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp_e = app.clone().oneshot(req_e).await.unwrap();
    assert_eq!(resp_e.status(), StatusCode::OK);
    let pos_e: PositionResponse = response_json(resp_e).await;
    assert_eq!(pos_e.total_games, 5);

    // Strict collection isolation for game retrieval: game 10 exists in masters (15 games), but not in elite (5 games)
    let req_g_m = Request::builder()
        .uri("/v1/collections/masters-otb/games/10")
        .body(Body::empty())
        .unwrap();
    let resp_g_m = app.clone().oneshot(req_g_m).await.unwrap();
    assert_eq!(resp_g_m.status(), StatusCode::OK);

    let req_g_e = Request::builder()
        .uri("/v1/collections/elite-online/games/10")
        .body(Body::empty())
        .unwrap();
    let resp_g_e = app.oneshot(req_g_e).await.unwrap();
    assert_eq!(resp_g_e.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_cors_preflight_and_headers() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 5);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        allowed_origins: vec!["http://localhost:3000".to_string()],
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/explorer/position")
        .header(header::ORIGIN, "http://localhost:3000")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .unwrap(),
        "http://localhost:3000"
    );
}

#[tokio::test]
async fn test_cli_and_http_query_equivalence() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 25);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Directly query the dataset using DatasetReader (same underlying call as CLI)
    let reader = codb_storage::DatasetReader::open(&masters_dir).unwrap();
    let mut board = shakmaty::Chess::default();
    codb_api::chess_util::parse_and_apply_moves(&mut board, "e4, e5", 200).unwrap();
    let key = codb_api::chess_util::compute_key(&board);

    let direct_stats = reader
        .query_position(key)
        .unwrap()
        .expect("Position stats in CPI");

    // Query via HTTP endpoint
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4,e5")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let http_pos: PositionResponse = response_json(resp).await;

    // Verify exact equivalence of totals
    assert_eq!(http_pos.total_games, direct_stats.total_games as u64);
    assert_eq!(http_pos.white_wins, direct_stats.white_wins as u64);
    assert_eq!(http_pos.draws, direct_stats.draws as u64);
    assert_eq!(http_pos.black_wins, direct_stats.black_wins as u64);

    // Verify candidate moves count and values
    assert_eq!(http_pos.moves.len(), direct_stats.moves.len());
    for http_m in &http_pos.moves {
        let matching = direct_stats
            .moves
            .iter()
            .find(|m| m.uci == http_m.uci)
            .unwrap();
        assert_eq!(http_m.white_wins, matching.white_wins as u64);
        assert_eq!(http_m.draws, matching.draws as u64);
        assert_eq!(http_m.black_wins, matching.black_wins as u64);
        assert_eq!(
            http_m.total_games,
            (matching.white_wins + matching.draws + matching.black_wins) as u64
        );
    }
}
