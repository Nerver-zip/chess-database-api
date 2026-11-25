//! Integration, contract, concurrency, and equivalence tests for `opening-explorer-api`.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use opening_explorer_api::config::ServerConfig;
use opening_explorer_api::models::{
    AdminReloadResponse, CollectionsResponse, HealthResponse, PositionResponse, ReadyResponse,
};
use opening_explorer_api::routes::{create_router, create_router_with_request_timeout};
use opening_explorer_api::state::AppState;
use opening_explorer_api::{ApiError, ErrorEnvelope};
use opening_explorer_core::{
    Collection, CompactMove, GameMetadata, GameResult, IngestionStats, ParsedGame,
};
use opening_explorer_storage::{
    build_dataset_position_index, DatasetIndexConfig, DatasetWriter, DatasetWriterConfig,
    SourceArchiveMetadata,
};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
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
            source_attribution: None,
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
async fn test_game_retrieval_route_can_be_disabled_for_restricted_datasets() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 2);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        enable_game_retrieval: false,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    let game_request = Request::builder()
        .uri("/v1/collections/masters-otb/games/1")
        .body(Body::empty())
        .unwrap();
    let game_response = app.clone().oneshot(game_request).await.unwrap();
    assert_eq!(game_response.status(), StatusCode::NOT_FOUND);

    let position_request = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let position_response = app.oneshot(position_request).await.unwrap();
    assert_eq!(position_response.status(), StatusCode::OK);
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
        admin_token: Some("test-admin-secret".to_string()),
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

    // Now reload snapshots via admin endpoint with valid Bearer token
    let reload_req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer test-admin-secret")
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
    let reader = opening_explorer_storage::DatasetReader::open(&masters_dir).unwrap();
    let mut board = shakmaty::Chess::default();
    opening_explorer_api::chess_util::parse_and_apply_moves(&mut board, "e4, e5", 200).unwrap();
    let key = opening_explorer_api::chess_util::compute_key(&board);

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

#[tokio::test]
async fn test_etag_and_conditional_caching() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 5);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // 1. Position ETag
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp
        .headers()
        .get(header::ETAG)
        .expect("ETag header present")
        .to_str()
        .unwrap()
        .to_string();
    assert!(!etag.is_empty());

    // Conditional position request with matching If-None-Match
    let req_cached = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    let resp_cached = app.clone().oneshot(req_cached).await.unwrap();
    assert_eq!(resp_cached.status(), StatusCode::NOT_MODIFIED);

    // 2. Game ETag
    let game_id = 1u64;
    let game_uri = format!("/v1/collections/masters-otb/games/{}", game_id);
    let req_game = Request::builder()
        .uri(&game_uri)
        .body(Body::empty())
        .unwrap();
    let resp_game = app.clone().oneshot(req_game).await.unwrap();
    assert_eq!(resp_game.status(), StatusCode::OK);
    let game_etag = resp_game
        .headers()
        .get(header::ETAG)
        .expect("Game ETag present")
        .to_str()
        .unwrap()
        .to_string();
    assert!(!game_etag.is_empty());

    // Conditional game request with matching If-None-Match
    let req_game_cached = Request::builder()
        .uri(&game_uri)
        .header(header::IF_NONE_MATCH, &game_etag)
        .body(Body::empty())
        .unwrap();
    let resp_game_cached = app.oneshot(req_game_cached).await.unwrap();
    assert_eq!(resp_game_cached.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn test_admin_reload_security() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 5);

    // 1. Configured with admin_token: "secret-admin-key"
    let config = ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        elite_dataset: None,
        admin_token: Some("secret-admin-key".to_string()),
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Anonymous request -> 401 Unauthorized
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "unauthorized");

    // Invalid Bearer token -> 401 Unauthorized
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer wrong-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Invalid X-Admin-Token -> 401 Unauthorized
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header("X-Admin-Token", "wrong-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Cross-origin request (Origin header present) -> 401 Unauthorized (rejection of cross-origin admin triggers)
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer secret-admin-key")
        .header(header::ORIGIN, "https://malicious-website.com")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Valid Bearer token without Origin -> 200 OK
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer secret-admin-key")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Valid X-Admin-Token without Origin -> 200 OK
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header("X-Admin-Token", "secret-admin-key")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Configured WITHOUT admin_token (None): endpoint disabled
    let config_disabled = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        admin_token: None,
        ..Default::default()
    };
    let state_disabled = AppState::new(config_disabled).unwrap();
    let app_disabled = create_router(state_disabled);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer secret-admin-key")
        .body(Body::empty())
        .unwrap();
    let resp = app_disabled.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "admin_disabled");
}

#[tokio::test]
async fn test_bounded_reader_pool_capacity() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 10);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        reader_pool_size: 2,
        reader_pool_timeout_secs: 5,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state.clone());

    // Launch 20 concurrent game retrievals
    let mut handles = Vec::new();
    for _ in 0..20 {
        let app_clone = app.clone();
        handles.push(tokio::spawn(async move {
            let req = Request::builder()
                .uri("/v1/collections/masters-otb/games/1")
                .body(Body::empty())
                .unwrap();
            let resp = app_clone.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = response_text(resp).await;
            assert!(body.contains("Grandmaster White 1"));
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify bounded capacity invariants
    let snapshot = state.get_snapshot(Collection::MastersOtb).unwrap();
    let pool = &snapshot.reader_pool;
    assert!(
        pool.peak_active_readers() <= 2,
        "Peak active readers {} exceeded pool capacity 2",
        pool.peak_active_readers()
    );
    assert_eq!(
        pool.active_readers(),
        0,
        "All reader permits must be released"
    );
}

#[tokio::test]
async fn test_conditional_caching_missing_resource_returns_404() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 5);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: None,
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // 1. Non-existent game with wildcard If-None-Match: * -> MUST return 404 Not Found (RFC 7232)
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/999999999")
        .header(header::IF_NONE_MATCH, "*")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 2. Non-existent game with specific If-None-Match -> MUST return 404 Not Found
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/999999999")
        .header(header::IF_NONE_MATCH, "\"some-etag\"")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 3. Existing game 1: get its ETag first
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/1")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let real_etag = resp
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // Existing game with wildcard If-None-Match: * -> returns 304 Not Modified
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/1")
        .header(header::IF_NONE_MATCH, "*")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);

    // Existing game with weak ETag (W/"...") -> returns 304 Not Modified
    let weak_etag = format!("W/{}", real_etag);
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/1")
        .header(header::IF_NONE_MATCH, &weak_etag)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);

    // Existing game with comma list containing the matching ETag -> returns 304 Not Modified
    let list_etag = format!("\"unrelated-tag\", {}", real_etag);
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/1")
        .header(header::IF_NONE_MATCH, &list_etag)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);

    // Existing game with non-matching ETag -> returns 200 OK
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/1")
        .header(header::IF_NONE_MATCH, "\"mismatched-etag\"")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_actual_version_replacement_a_to_b() {
    let tmp = tempdir().unwrap();
    let v1_dir = tmp.path().join("masters_v1");
    let v2_dir = tmp.path().join("masters_v2");
    setup_test_dataset(&v1_dir, Collection::MastersOtb, 5);
    setup_test_dataset(&v2_dir, Collection::MastersOtb, 10);

    let active_link = tmp.path().join("masters_active");
    std::os::unix::fs::symlink(&v1_dir, &active_link).unwrap();

    let config = ServerConfig {
        masters_dataset: Some(active_link.clone()),
        elite_dataset: None,
        admin_token: Some("reload-key".to_string()),
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Query v1: 5 games
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let pos_v1: PositionResponse = response_json(resp).await;
    assert_eq!(pos_v1.total_games, 5);

    // Atomic symlink switch to v2
    let tmp_link = tmp.path().join("masters_active_tmp");
    std::os::unix::fs::symlink(&v2_dir, &tmp_link).unwrap();
    std::fs::rename(&tmp_link, &active_link).unwrap();

    // Authenticated reload
    let reload_req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer reload-key")
        .body(Body::empty())
        .unwrap();
    let reload_resp = app.clone().oneshot(reload_req).await.unwrap();
    assert_eq!(reload_resp.status(), StatusCode::OK);

    // Query after reload: now reflects v2 (10 games)
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let pos_v2: PositionResponse = response_json(resp).await;
    assert_eq!(pos_v2.total_games, 10);
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        } else if ft.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            std::os::unix::fs::symlink(target, dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn corrupt_cpi_in_dir(dir: &Path) -> bool {
    let mut found = false;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if corrupt_cpi_in_dir(&p) {
                    found = true;
                }
            } else if p.file_name().is_some_and(|n| n == "positions.cpi") {
                std::fs::write(&p, b"INVALID_GARBAGE_CPI_DATA_FAILING_MAGIC_AND_INTEGRITY")
                    .unwrap();
                found = true;
            }
        }
    }
    found
}

#[tokio::test]
async fn test_corrupted_replacement_rejected() {
    let tmp = tempdir().unwrap();
    let v1_dir = tmp.path().join("masters_good");
    setup_test_dataset(&v1_dir, Collection::MastersOtb, 5);

    let active_link = tmp.path().join("masters_symlink");
    std::os::unix::fs::symlink(&v1_dir, &active_link).unwrap();

    let config = ServerConfig {
        masters_dataset: Some(active_link.clone()),
        elite_dataset: None,
        admin_token: Some("secret".to_string()),
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Baseline query succeeds
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Create a corrupted dataset candidate: copy good dataset and destroy positions.cpi
    let corrupted_dir = tmp.path().join("masters_corrupted");
    copy_dir_recursive(&v1_dir, &corrupted_dir).unwrap();

    let found_cpi = corrupt_cpi_in_dir(&corrupted_dir);
    assert!(found_cpi, "Must have found positions.cpi to corrupt");

    // Atomic symlink switch to corrupted dataset candidate
    let tmp_link = tmp.path().join("masters_symlink_tmp");
    std::os::unix::fs::symlink(&corrupted_dir, &tmp_link).unwrap();
    std::fs::rename(&tmp_link, &active_link).unwrap();

    // Authenticated reload must FAIL CLOSED (HTTP 500 ReloadFailed)
    let reload_req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .unwrap();
    let reload_resp = app.clone().oneshot(reload_req).await.unwrap();
    assert_eq!(reload_resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let err: ErrorEnvelope = response_json(reload_resp).await;
    assert_eq!(err.error.code, "reload_failed");

    // CRITICAL: The previous active snapshot was preserved and remains 100% operational!
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let pos: PositionResponse = response_json(resp).await;
    assert_eq!(pos.total_games, 5);

    // Game retrieval also continues working on the preserved snapshot
    let req = Request::builder()
        .uri("/v1/collections/masters-otb/games/1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_concurrent_queries_during_version_replacement() {
    let tmp = tempdir().unwrap();
    let v1_dir = tmp.path().join("masters_v1");
    let v2_dir = tmp.path().join("masters_v2");
    setup_test_dataset(&v1_dir, Collection::MastersOtb, 5);
    setup_test_dataset(&v2_dir, Collection::MastersOtb, 10);

    let active_link = tmp.path().join("masters_active");
    std::os::unix::fs::symlink(&v1_dir, &active_link).unwrap();

    let config = ServerConfig {
        masters_dataset: Some(active_link.clone()),
        elite_dataset: None,
        admin_token: Some("token".to_string()),
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // Launch continuous background queries
    let app_bg = app.clone();
    let stop_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop_signal);

    let query_task = tokio::spawn(async move {
        let mut count = 0;
        while !stop_clone.load(Ordering::Relaxed) {
            let req = Request::builder()
                .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
                .body(Body::empty())
                .unwrap();
            let resp = app_bg.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let req_game = Request::builder()
                .uri("/v1/collections/masters-otb/games/1")
                .body(Body::empty())
                .unwrap();
            let resp_game = app_bg.clone().oneshot(req_game).await.unwrap();
            assert_eq!(resp_game.status(), StatusCode::OK);

            count += 1;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        count
    });

    // While queries are in flight, perform version replacement
    tokio::time::sleep(Duration::from_millis(20)).await;
    let tmp_link = tmp.path().join("masters_active_tmp");
    std::os::unix::fs::symlink(&v2_dir, &tmp_link).unwrap();
    std::fs::rename(&tmp_link, &active_link).unwrap();

    let reload_req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer token")
        .body(Body::empty())
        .unwrap();
    let reload_resp = app.clone().oneshot(reload_req).await.unwrap();
    assert_eq!(reload_resp.status(), StatusCode::OK);

    // Let more queries run against the new snapshot
    tokio::time::sleep(Duration::from_millis(20)).await;
    stop_signal.store(true, Ordering::Relaxed);

    let total_queries = query_task.await.unwrap();
    assert!(
        total_queries > 0,
        "Executed {} concurrent queries during reload",
        total_queries
    );

    // Post-reload query sees v2
    let req = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=e4")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let pos: PositionResponse = response_json(resp).await;
    assert_eq!(pos.total_games, 10);
}

#[tokio::test]
async fn test_admin_token_validation_and_empty_token_rejection() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 2);

    // 1. Missing token (None) -> validate() succeeds; reload disabled (403 admin_disabled)
    let config_none = ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        elite_dataset: None,
        admin_token: None,
        ..Default::default()
    };
    assert!(config_none.validate().is_ok());
    let state_none = AppState::new(config_none).unwrap();
    let app_none = create_router(state_none);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .body(Body::empty())
        .unwrap();
    let resp = app_none.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "admin_disabled");

    // 2. Empty string token ("") -> validate() FAILS at startup
    let config_empty = ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        elite_dataset: None,
        admin_token: Some("".to_string()),
        ..Default::default()
    };
    let err = config_empty.validate().unwrap_err();
    assert!(err.contains("cannot be empty or whitespace-only"));
    assert!(AppState::new(config_empty).is_err());

    // 3. Whitespace-only token ("   \t\n  ") -> validate() FAILS at startup
    let config_ws = ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        elite_dataset: None,
        admin_token: Some("   \t\n  ".to_string()),
        ..Default::default()
    };
    let err = config_ws.validate().unwrap_err();
    assert!(err.contains("cannot be empty or whitespace-only"));
    assert!(AppState::new(config_ws).is_err());

    // 4. Valid token ("valid-secret") -> validate() succeeds, server starts
    let config_valid = ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        elite_dataset: None,
        admin_token: Some("valid-secret".to_string()),
        ..Default::default()
    };
    assert!(config_valid.validate().is_ok());
    let state_valid = AppState::new(config_valid).unwrap();
    let app_valid = create_router(state_valid);

    // 4a. Empty bearer token -> 401 Unauthorized
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer ")
        .body(Body::empty())
        .unwrap();
    let resp = app_valid.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let err: ErrorEnvelope = response_json(resp).await;
    assert_eq!(err.error.code, "unauthorized");

    // 4b. Invalid token -> 401 Unauthorized
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer wrong-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app_valid.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 4c. Valid token -> 200 OK
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/reload")
        .header(header::AUTHORIZATION, "Bearer valid-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app_valid.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_internal_error_sanitization() {
    use axum::response::IntoResponse;

    // 1. Storage error with simulated filesystem path and engine internals
    let storage_err = ApiError::Storage(
        "/var/run/opening-explorer/storage/shard_0001.cgb3: checksum mismatch at block offset 0xdeadbeef"
            .to_string(),
    );
    assert_eq!(storage_err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(storage_err.error_code(), "storage_error");
    let resp = storage_err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: ErrorEnvelope = response_json(resp).await;
    assert_eq!(body.error.code, "storage_error");
    assert_eq!(body.error.message, "A storage engine error occurred.");
    assert!(!body.error.message.contains("/var"));
    assert!(!body.error.message.contains("shard_0001"));
    assert!(!body.error.message.contains("0xdeadbeef"));

    // 2. Internal error with internal details
    let internal_err = ApiError::Internal(
        "worker thread panicked at /usr/src/repo/crates/api/src/state.rs:42: pool exhausted"
            .to_string(),
    );
    assert_eq!(
        internal_err.status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(internal_err.error_code(), "internal_error");
    let resp = internal_err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: ErrorEnvelope = response_json(resp).await;
    assert_eq!(body.error.code, "internal_error");
    assert_eq!(body.error.message, "An internal server error occurred.");
    assert!(!body.error.message.contains("/usr"));
    assert!(!body.error.message.contains("state.rs"));
    assert!(!body.error.message.contains("panicked"));

    // 3. ReloadFailed error with candidate path details
    let reload_err = ApiError::ReloadFailed(
        "corrupt candidate snapshot at /opt/data/builds/v2/positions.cpi: magic header invalid"
            .to_string(),
    );
    assert_eq!(reload_err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reload_err.error_code(), "reload_failed");
    let resp = reload_err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: ErrorEnvelope = response_json(resp).await;
    assert_eq!(body.error.code, "reload_failed");
    assert_eq!(
        body.error.message,
        "Administrative reload failed: replacement snapshot integrity check was rejected."
    );
    assert!(!body.error.message.contains("/opt"));
    assert!(!body.error.message.contains("positions.cpi"));
}

#[tokio::test]
async fn test_position_etag_semantics_and_fen_variation() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    let elite_dir = tmp.path().join("elite");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 5);
    setup_test_dataset(&elite_dir, Collection::EliteOnline, 5);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        elite_dataset: Some(elite_dir),
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    let app = create_router(state);

    // 1. Initial position via standard starting FEN
    let req_start_fen = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&fen=rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR%20w%20KQkq%20-%200%201")
        .body(Body::empty())
        .unwrap();
    let resp_start_fen = app.clone().oneshot(req_start_fen).await.unwrap();
    assert_eq!(resp_start_fen.status(), StatusCode::OK);
    let etag_start = resp_start_fen
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // 2. Initial position reached via move repetition (Nf3 Nf6 Ng1 Ng8)
    // Board placement and Zobrist key are identical, but FEN halfmove clock is 4 and fullmove is 3
    let req_repetition = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=Nf3,Nf6,Ng1,Ng8")
        .body(Body::empty())
        .unwrap();
    let resp_repetition = app.clone().oneshot(req_repetition).await.unwrap();
    assert_eq!(resp_repetition.status(), StatusCode::OK);
    let etag_repetition = resp_repetition
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // ETags MUST be distinct because response bodies differ in "fen" representation!
    assert_ne!(
        etag_start, etag_repetition,
        "ETags must differ when FEN clocks differ despite identical position key"
    );

    // 3. Conditional caching for starting position:
    // With matching ETag -> 304 Not Modified
    let req_cond_match = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&fen=rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR%20w%20KQkq%20-%200%201")
        .header(header::IF_NONE_MATCH, &etag_start)
        .body(Body::empty())
        .unwrap();
    let resp_cond_match = app.clone().oneshot(req_cond_match).await.unwrap();
    assert_eq!(resp_cond_match.status(), StatusCode::NOT_MODIFIED);

    // With repetition ETag -> 200 OK (different representation)
    let req_cond_mismatch = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&fen=rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR%20w%20KQkq%20-%200%201")
        .header(header::IF_NONE_MATCH, &etag_repetition)
        .body(Body::empty())
        .unwrap();
    let resp_cond_mismatch = app.clone().oneshot(req_cond_mismatch).await.unwrap();
    assert_eq!(resp_cond_mismatch.status(), StatusCode::OK);

    // 4. Transpositions that yield the EXACT same FEN representation share the same ETag
    // Example: 1. Nf3 Nf6 2. Nc3 Nc6 vs 1. Nc3 Nc6 2. Nf3 Nf6
    // Both sequences contain 4 knight moves, yielding identical halfmove clock (4) and fullmove counter (3).
    let req_trans_1 = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=Nf3,Nf6,Nc3,Nc6")
        .body(Body::empty())
        .unwrap();
    let resp_trans_1 = app.clone().oneshot(req_trans_1).await.unwrap();
    assert_eq!(resp_trans_1.status(), StatusCode::OK);
    let etag_trans_1 = resp_trans_1
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let req_trans_2 = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=Nc3,Nc6,Nf3,Nf6")
        .body(Body::empty())
        .unwrap();
    let resp_trans_2 = app.clone().oneshot(req_trans_2).await.unwrap();
    assert_eq!(resp_trans_2.status(), StatusCode::OK);
    let etag_trans_2 = resp_trans_2
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    assert_eq!(
        etag_trans_1, etag_trans_2,
        "Equivalent transpositions with identical FEN must share the exact same ETag"
    );

    // Conditional caching succeeds across identical transposition
    let req_trans_cached = Request::builder()
        .uri("/v1/explorer/position?collection=masters-otb&moves=Nc3,Nc6,Nf3,Nf6")
        .header(header::IF_NONE_MATCH, &etag_trans_1)
        .body(Body::empty())
        .unwrap();
    let resp_trans_cached = app.clone().oneshot(req_trans_cached).await.unwrap();
    assert_eq!(resp_trans_cached.status(), StatusCode::NOT_MODIFIED);

    // 5. Cross-collection isolation: querying elite-online with start FEN
    let req_elite = Request::builder()
        .uri("/v1/explorer/position?collection=elite-online&fen=rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR%20w%20KQkq%20-%200%201")
        .body(Body::empty())
        .unwrap();
    let resp_elite = app.clone().oneshot(req_elite).await.unwrap();
    assert_eq!(resp_elite.status(), StatusCode::OK);
    let etag_elite = resp_elite
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    assert_ne!(
        etag_start, etag_elite,
        "ETags across different collections must never collide"
    );
    assert!(etag_start.contains("masters-otb"));
    assert!(etag_elite.contains("elite-online"));
}

#[test]
fn test_reader_pool_timeout_validation() {
    let config_zero = ServerConfig {
        masters_dataset: Some(std::path::PathBuf::from("/tmp/masters")),
        reader_pool_timeout_secs: 0,
        ..Default::default()
    };
    let err = config_zero.validate().unwrap_err();
    assert!(err.contains("Reader pool timeout must be between 1 and 60 seconds"));

    let config_ok = ServerConfig {
        masters_dataset: Some(std::path::PathBuf::from("/tmp/masters")),
        reader_pool_timeout_secs: 10,
        ..Default::default()
    };
    assert!(config_ok.validate().is_ok());

    let config_pool_over_limit = ServerConfig {
        masters_dataset: Some(std::path::PathBuf::from("/tmp/masters")),
        reader_pool_size: 65,
        ..Default::default()
    };
    assert!(config_pool_over_limit
        .validate()
        .unwrap_err()
        .contains("Reader pool size must be between 1 and 64"));

    let config_request_over_limit = ServerConfig {
        masters_dataset: Some(std::path::PathBuf::from("/tmp/masters")),
        request_timeout_secs: 301,
        ..Default::default()
    };
    assert!(config_request_over_limit
        .validate()
        .unwrap_err()
        .contains("Request timeout must be between 1 and 300 seconds"));
}

/// Sends one request through the router and returns the status, headers, and body text.
async fn send(
    app: &axum::Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let response_headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        response_headers,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

/// Parses a JSON error envelope and checks the code and status it carries.
fn assert_envelope(
    status: StatusCode,
    body: &str,
    expected_status: StatusCode,
    expected_code: &str,
) {
    assert_eq!(status, expected_status, "body: {}", body);
    let envelope: ErrorEnvelope = serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("expected an error envelope ({}): {}", e, body));
    assert_eq!(envelope.error.code, expected_code);
    assert!(!envelope.error.message.is_empty());
}

#[tokio::test]
async fn test_admin_reload_is_exempt_from_request_timeout() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 4);

    let config = ServerConfig {
        masters_dataset: Some(masters_dir),
        admin_token: Some("reload-token".to_string()),
        ..Default::default()
    };
    let state = AppState::new(config).unwrap();
    // A zero request timeout would cut off any read that waits on blocking work. The reload
    // must still complete, because it is not subject to the request timeout.
    let app = create_router_with_request_timeout(state, Duration::ZERO);

    let (status, _, body) = send(
        &app,
        Method::POST,
        "/v1/admin/reload",
        &[("authorization", "Bearer reload-token")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {}", body);
    let reload: AdminReloadResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(reload.status, "ok");
    assert_eq!(reload.reloaded_collections.len(), 1);
    assert!(reload.reloaded_collections[0].starts_with("masters-otb (version: "));
}

#[tokio::test]
async fn test_position_response_reports_index_depth() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 6);
    let state = AppState::new(ServerConfig {
        masters_dataset: Some(masters_dir),
        ..Default::default()
    })
    .unwrap();
    let app = create_router(state);

    let (status, _, body) = send(
        &app,
        Method::GET,
        "/v1/explorer/position?collection=masters-otb&moves=e4",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {}", body);
    let position: PositionResponse = serde_json::from_str(&body).unwrap();
    // setup_test_dataset builds a depth-40 index; the response must say so.
    assert_eq!(position.cpi_depth_plies, 40);
    let raw: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(raw["cpi_depth_plies"], 40);
}

#[tokio::test]
async fn test_framework_rejections_use_error_envelope() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 4);
    let state = AppState::new(ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        ..Default::default()
    })
    .unwrap();
    let app = create_router(state);

    // Repeated query parameter: the query decoder rejects it (400).
    let (status, headers, body) = send(
        &app,
        Method::GET,
        "/v1/explorer/position?collection=masters-otb&moves=e4&moves=d4",
        &[],
    )
    .await;
    assert_envelope(status, &body, StatusCode::BAD_REQUEST, "invalid_query");
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert!(body.contains("duplicate field"));

    // Unknown path (404).
    let (status, _, body) = send(&app, Method::GET, "/v1/no-such-route", &[]).await;
    assert_envelope(status, &body, StatusCode::NOT_FOUND, "not_found");

    // Wrong method (405), with the Allow header kept.
    let (status, headers, body) = send(
        &app,
        Method::POST,
        "/v1/explorer/position?collection=masters-otb&moves=e4",
        &[],
    )
    .await;
    assert_envelope(
        status,
        &body,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    );
    assert!(headers.get(header::ALLOW).is_some());

    // Game route while retrieval is disabled: an unknown path, so the same envelope.
    let disabled = create_router(
        AppState::new(ServerConfig {
            masters_dataset: Some(masters_dir),
            enable_game_retrieval: false,
            ..Default::default()
        })
        .unwrap(),
    );
    let (status, _, body) = send(
        &disabled,
        Method::GET,
        "/v1/collections/masters-otb/games/1",
        &[],
    )
    .await;
    assert_envelope(status, &body, StatusCode::NOT_FOUND, "not_found");

    // Application errors keep their own codes.
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/v1/explorer/position?collection=masters-otb",
        &[],
    )
    .await;
    assert_envelope(status, &body, StatusCode::BAD_REQUEST, "missing_position");
}

#[tokio::test]
async fn test_unavailable_collection_message_is_not_replaced() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 4);
    let app = create_router(
        AppState::new(ServerConfig {
            masters_dataset: Some(masters_dir),
            ..Default::default()
        })
        .unwrap(),
    );

    // A 503 describes the client-visible condition and is returned as written.
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/v1/explorer/position?collection=elite-online&moves=e4",
        &[],
    )
    .await;
    assert_envelope(
        status,
        &body,
        StatusCode::SERVICE_UNAVAILABLE,
        "collection_unavailable",
    );
    let envelope: ErrorEnvelope = serde_json::from_str(&body).unwrap();
    assert_eq!(
        envelope.error.message,
        "Collection 'elite-online' is not configured or loaded on this server."
    );
}

#[tokio::test]
async fn test_illegal_and_invalid_move_messages_name_token_and_ply() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 4);
    let app = create_router(
        AppState::new(ServerConfig {
            masters_dataset: Some(masters_dir),
            ..Default::default()
        })
        .unwrap(),
    );

    let cases = [
        // SAN that is legal notation but not legal here (black to move).
        ("e4,e4", "Illegal move 'e4' at ply 2."),
        // A white knight cannot reach f3 once it is already there.
        ("e4,e5,Nf3,Nf3", "Illegal move 'Nf3' at ply 4."),
        // UCI that is legal notation but not legal here.
        ("e2e4,e2e4", "Illegal move 'e2e4' at ply 2."),
        // Text that is neither SAN nor UCI.
        (
            "e4,e9",
            "Invalid move token 'e9' at ply 2: expected SAN or UCI notation.",
        ),
    ];
    for (moves, expected) in cases {
        let uri = format!(
            "/v1/explorer/position?collection=masters-otb&moves={}",
            urlencode(moves)
        );
        let (status, _, body) = send(&app, Method::GET, &uri, &[]).await;
        assert_envelope(status, &body, StatusCode::BAD_REQUEST, "invalid_moves");
        let envelope: ErrorEnvelope = serde_json::from_str(&body).unwrap();
        assert_eq!(
            envelope.error.message,
            format!("Invalid or illegal move sequence: {}", expected),
            "moves={}",
            moves
        );
    }

    // Legal notation in both SAN and UCI is still accepted.
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/v1/explorer/position?collection=masters-otb&moves=e4,e2e4",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {}", body);
}

#[tokio::test]
async fn test_missing_collection_message_is_clear() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 4);
    let app = create_router(
        AppState::new(ServerConfig {
            masters_dataset: Some(masters_dir),
            ..Default::default()
        })
        .unwrap(),
    );

    let (status, _, body) = send(&app, Method::GET, "/v1/explorer/position?moves=e4", &[]).await;
    assert_envelope(status, &body, StatusCode::BAD_REQUEST, "invalid_collection");
    let envelope: ErrorEnvelope = serde_json::from_str(&body).unwrap();
    assert_eq!(
        envelope.error.message,
        "Missing required query parameter 'collection'. Expected 'masters-otb' or 'elite-online'."
    );
}

#[tokio::test]
async fn test_game_not_found_message_names_collection_not_snapshot_path() {
    let tmp = tempdir().unwrap();
    let masters_dir = tmp.path().join("masters");
    setup_test_dataset(&masters_dir, Collection::MastersOtb, 4);
    let state = AppState::new(ServerConfig {
        masters_dataset: Some(masters_dir.clone()),
        ..Default::default()
    })
    .unwrap();

    // The route checks the lookup index before reading, so this error path is reached
    // through the reader pool directly.
    let snapshot = state.get_snapshot(Collection::MastersOtb).unwrap();
    let err = snapshot
        .reader_pool
        .get_game_pgn(999_999, Duration::from_secs(5))
        .await
        .unwrap_err();
    match err {
        ApiError::GameNotFound(id, collection) => {
            assert_eq!(id, 999_999);
            assert_eq!(collection, "masters-otb");
        }
        other => panic!("expected GameNotFound, got {:?}", other),
    }
    let message = ApiError::GameNotFound(999_999, "masters-otb".to_string()).to_string();
    assert!(!message.contains(&masters_dir.display().to_string()));
}

/// Percent-encodes characters that are significant in a query string.
fn urlencode(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            ',' => "%2C".to_string(),
            ' ' => "%20".to_string(),
            other => other.to_string(),
        })
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn search_rolls_forward_and_back_with_snapshot_bound_cursors() {
    use opening_explorer_storage::search::{build_search_index, BuildSearchConfig};
    use serde_json::Value;
    use std::os::unix::fs::symlink;

    async fn get(app: &axum::Router, uri: &str, status: StatusCode) -> Value {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let actual = response.status();
        let body: Value = response_json(response).await;
        assert_eq!(actual, status, "{uri}: {body}");
        body
    }

    let tmp = tempdir().unwrap();
    let dataset = tmp.path().join("elite");
    setup_test_dataset(&dataset, Collection::EliteOnline, 12);
    let legacy = std::fs::read_link(dataset.join("current")).unwrap();
    let state = AppState::new(ServerConfig {
        elite_dataset: Some(dataset.clone()),
        ..Default::default()
    })
    .unwrap();
    let router = create_router(state.clone());
    let games = "/v1/collections/elite-online/games";
    let explorer = "/v1/explorer/position?collection=elite-online&moves=e4";
    let unavailable = get(&router, games, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(unavailable["error"]["code"], "search_unavailable");
    let legacy_answer = get(&router, explorer, StatusCode::OK).await;

    let indexed = build_search_index(
        &dataset,
        BuildSearchConfig {
            memory_limit_bytes: 48,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(indexed.index.game_count, 12);
    assert!(!indexed.index.canonical_sha256.is_empty());
    let current = std::fs::read_link(dataset.join("current")).unwrap();
    for file in ["positions.cpi", "games.idx", "shards/shard_0000.cgb3"] {
        assert_eq!(
            std::fs::read(dataset.join(&legacy).join(file)).unwrap(),
            std::fs::read(dataset.join(&current).join(file)).unwrap(),
        );
    }

    state.reload().await.unwrap();
    let caps = get(&router, "/v1/collections", StatusCode::OK).await;
    assert_eq!(caps["collections"][0]["capabilities"]["game_search"], true);
    assert_eq!(
        caps["collections"][0]["capabilities"]["position_search_coverage"]["max_ply"],
        40
    );
    let query = format!("{games}?moves=e4,e5&continuation=Nf3&min_both_ratings=2680&sort=date_desc&exact_count=true&limit=3");
    let first = get(&router, &query, StatusCode::OK).await;
    assert_eq!(first["total_games"], 8);
    assert_eq!(first["games"].as_array().unwrap().len(), 3);
    assert_eq!(first["games"][0]["position_match"]["ply"], 2);
    assert_eq!(first["games"][0]["position_match"]["continuation"], "g1f3");
    let token = first["next_cursor"].as_str().unwrap();
    let second_uri = format!("{query}&cursor={token}");
    let second = get(&router, &second_uri, StatusCode::OK).await;
    let last = get(
        &router,
        &format!("{query}&cursor={}", second["next_cursor"].as_str().unwrap()),
        StatusCode::OK,
    )
    .await;
    assert_eq!(last["has_more"], false);
    assert_eq!(last["next_cursor"], Value::Null);
    let ids: std::collections::HashSet<_> = [&first, &second, &last]
        .into_iter()
        .flat_map(|page| {
            page["games"]
                .as_array()
                .unwrap()
                .iter()
                .map(|game| game["id"].as_str().unwrap())
        })
        .collect();
    assert_eq!(ids.len(), 8);
    let detail = format!(
        "{games}/{}/metadata",
        first["games"][0]["id"].as_str().unwrap()
    );
    let detail = get(&router, &detail, StatusCode::OK).await;
    assert_eq!(detail["game"]["id"], first["games"][0]["id"]);
    let calendar = get(
        &router,
        &format!("{games}?from_year=2025&sort=date_desc&exact_count=true&limit=2"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(calendar["total_games"], 12);
    assert_eq!(calendar["games"].as_array().unwrap().len(), 2);
    let absent_year = get(
        &router,
        &format!("{games}?through_year=2024&sort=date_asc&exact_count=true"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(absent_year["total_games"], 0);
    let invalid = get(
        &router,
        &format!("{games}?cursor={token}"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(invalid["error"]["code"], "invalid_cursor");
    let invalid = get(
        &router,
        &format!("{games}?min_white_rating=2700&max_white_rating=2600"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(invalid["error"]["code"], "invalid_query");

    let enriched_answer = get(&router, explorer, StatusCode::OK).await;
    assert_eq!(legacy_answer["moves"], enriched_answer["moves"]);
    assert_eq!(legacy_answer["total_games"], enriched_answer["total_games"]);
    let all_stats = get(
        &router,
        "/v1/explorer/position/filtered?collection=elite-online&moves=e4&min_both_ratings=2600",
        StatusCode::OK,
    )
    .await;
    assert_eq!(enriched_answer, all_stats);
    let filtered = get(
        &router,
        "/v1/explorer/position/filtered?collection=elite-online&moves=e4&result=1-0",
        StatusCode::OK,
    )
    .await;
    assert_eq!(filtered["total_games"], 6);
    assert_eq!(filtered["white_wins"], 6);

    let players = get(
        &router,
        "/v1/collections/elite-online/players?prefix=grandmaster&limit=2",
        StatusCode::OK,
    )
    .await;
    assert_eq!(players["players"].as_array().unwrap().len(), 2);
    let more_players = get(
        &router,
        &format!(
            "/v1/collections/elite-online/players?prefix=grandmaster&limit=2&cursor={}",
            players["next_cursor"].as_str().unwrap()
        ),
        StatusCode::OK,
    )
    .await;
    for player in more_players["players"].as_array().unwrap() {
        assert!(!players["players"].as_array().unwrap().contains(player));
    }
    let exact_player = get(
        &router,
        "/v1/collections/elite-online/players?name=Grandmaster+White+1",
        StatusCode::OK,
    )
    .await;
    assert_eq!(exact_player["players"].as_array().unwrap().len(), 1);
    assert_eq!(
        exact_player["players"][0]["display_name"],
        "Grandmaster White 1"
    );
    let events = get(
        &router,
        "/v1/collections/elite-online/events?prefix=world&year=2025",
        StatusCode::OK,
    )
    .await;
    assert_eq!(events["events"][0]["game_count"], 12);

    build_search_index(&dataset, BuildSearchConfig::default()).unwrap();
    state.reload().await.unwrap();
    let stale = get(&router, &second_uri, StatusCode::CONFLICT).await;
    assert_eq!(stale["error"]["code"], "stale_cursor");

    let broken = dataset.join("current/search/positions.postings");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(broken)
        .unwrap();
    std::io::Write::write_all(&mut file, b"corrupt").unwrap();
    drop(file);
    assert!(state.reload().await.is_err());
    // Roll back the disk pointer without changing the legacy files the service had pinned.
    assert_eq!(
        get(&router, explorer, StatusCode::OK).await["total_games"],
        12
    );
    let staged_link = dataset.join("rollback");
    symlink(legacy, &staged_link).unwrap();
    std::fs::rename(staged_link, dataset.join("current")).unwrap();
    state.reload().await.unwrap();
    get(&router, games, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(get(&router, explorer, StatusCode::OK).await, legacy_answer);
}
