---
summary: "Decisions on the production-ready Opening Explorer HTTP API, snapshot pinning, and concurrency model."
read_when: "Reviewing the HTTP server architecture, state management, endpoint contracts, and concurrency design."
status: current
---

# ADR 0004: Production-Ready Opening Explorer HTTP API — Axum Serving Layer, Snapshot Pinning & Lock-Free Concurrent Queries

## Status
Accepted

## Context
Milestones PR-01 through PR-03 delivered core data ingestion, deduplication, CGB3 compact game storage, and the CPI v2 position index with bounded-memory external merge sort. However, query capabilities were restricted to the CLI tool (`opening-explorer query`, `opening-explorer get-game`).

To serve external consumer applications (including desktop review tools, web clients, and analysis engines), the project requires a production-oriented, read-only HTTP API that exposes opening statistics and game retrieval without sacrificing the engineering guarantees established in earlier milestones:

1. **Direct In-Process Integration**: Never spawn CLI child processes per HTTP request. The HTTP layer must invoke reusable library APIs directly with zero process startup overhead.
2. **Pinned Snapshot Consistency**: A client request must observe an internally consistent dataset snapshot. CPI position statistics, game lookup indices, and shard data must never be mixed across differing dataset versions.
3. **High-Throughput Concurrency**: Queries must not be serialized behind a global mutex. Position queries against the memory-mapped CPI v2 index must execute lock-free across concurrent worker threads.
4. **Asynchronous Runtime Safety**: Blocking disk decompression and file seeking must not block the asynchronous Tokio reactor threads.
5. **Zero-Downtime Atomic Reloading**: When a newly published dataset snapshot replaces `current`, the server must update its serving state atomically. In-flight requests must complete uninterrupted against their pinned snapshot, and corrupt replacement attempts must be rejected without degrading active service.
6. **Defensive Input Validation & Security**: All user-supplied parameters (FEN, move sequences, collection identifiers, game IDs) must be strictly validated with standard, machine-readable JSON error contracts. Unsupported query filters (`since_year`, `min_rating`) must fail with clear `400 Bad Request` responses rather than silently returning unfiltered results.

---

## Decisions

### 1. HTTP Server Architecture & Framework Selection
We introduce a dedicated workspace crate, `opening-explorer-api` (`crates/api`), and expose a `opening-explorer serve` command in `crates/cli`:
- **Framework**: Built on [`axum`](https://crates.io/crates/axum) (v0.8) and [`tokio`](https://crates.io/crates/tokio) (v1.40). Axum offers first-class ergonomics, type-safe extractors, zero-cost routing via `matchit`, and seamless middleware integration via `tower-http`.
- **Middleware Pipeline**: Configured with `tower_http::cors::CorsLayer` for configurable browser access, `tower_http::timeout::TimeoutLayer` for bounding query runtimes, and `tower_http::trace::TraceLayer` for structured observability.
- **CLI Command (`opening-explorer serve`)**: Accepts `--masters-dataset`, `--elite-dataset`, `--host`, `--port`, `--allowed-origins`, `--reader-pool-size`, `--reader-pool-timeout-secs`, and `--admin-token`, with environment variable fallbacks (`CODB_*`). Fails immediately at startup if neither collection is provided, if specified paths are invalid, if `reader_pool_timeout_secs == 0`, or if `admin_token` is empty or whitespace-only.

### 2. State Management & Snapshot Pinning
The application state (`AppState`) manages dataset handles:
- **`DatasetSnapshot`**: Encapsulates an active dataset version:
  - Pinned physical snapshot directory (e.g. `versions/v1`) resolved via `resolve_snapshot_dir`.
  - Parsed `DatasetManifest`.
  - Memory-mapped `DiskPositionIndex` wrapped in `Option<Arc<DiskPositionIndex>>`.
  - Memory-mapped `GameLookupReader` for $O(\log N)$ existence checks and location lookups.
  - Semaphore-bounded `DatasetReaderPool` for game decompression.
- **Snapshot Pinning**: State stores snapshots in `parking_lot::RwLock<Option<Arc<DatasetSnapshot>>>`. Route handlers acquire the read lock for $< 10\text{ ns}$ to clone the `Arc<DatasetSnapshot>`. The cloned `Arc` pins the physical snapshot directory for the entire lifetime of the request.
- **Zero-Downtime Reload (`POST /v1/admin/reload` & `SIGHUP`)**:
  - The reload operation opens, verifies, and prepares candidate snapshots in isolated local variables offloaded to `spawn_blocking`.
  - Runs deep validation (`verify_dataset` block checksums + `DiskPositionIndex::verify_integrity`). If verification or opening fails, the reload fails closed (`500 reload_failed`) and leaves the active serving state untouched and fully operational.
  - If successful, the `RwLock` write pointer is updated in nanoseconds. In-flight requests finish cleanly against the prior version, while new requests immediately observe the new version.
  - Administrative security: `POST /v1/admin/reload` requires authorization via `Bearer <token>` or `X-Admin-Token` matching `--admin-token` / `OPENING_EXPLORER_ADMIN_TOKEN`. Startup configuration rejects empty or whitespace-only tokens (`ServerConfig::validate`). If not configured, the endpoint returns `403 Forbidden`. Cross-origin triggers (`Origin` header present) are rejected with `401 Unauthorized`.
  - Local operators can also trigger reload via standard Unix `SIGHUP` signal (`kill -HUP <pid>`).

### 3. Concurrency Model: Lock-Free CPI Queries & Semaphore-Bounded Reader Pool
- **Position Queries (`/v1/explorer/position`)**: `DiskPositionIndex` holds a memory-mapped sparse index and compressed block data accessed exclusively via immutable `&self`. Because no interior mutability or locks exist, concurrent requests query `DiskPositionIndex::query` completely lock-free with zero mutex contention, achieving sub-millisecond p50 latencies ($220\text{–}360\ \mu\text{s}$).
- **Game Retrieval (`/v1/collections/{collection}/games/{id}`)**: Shard reads and Zstd block decompression require file seeking and update a single-block cache. We introduce [`DatasetReaderPool`](../../crates/api/src/state.rs) with strict capacity bounds enforced by an asynchronous semaphore (`tokio::sync::Semaphore`).
  - Active readers are strictly limited to `reader_pool_size`.
  - Requests exceeding capacity wait asynchronously up to `reader_pool_timeout_secs` without blocking Tokio worker threads.
  - Blocking disk I/O and decompression are executed inside `tokio::task::spawn_blocking`, protecting Tokio's worker threads from starvation.
  - Permits are safely dropped and readers returned to the idle pool upon completion, error, or cancellation.

### 4. Collection Isolation
`masters-otb` and `elite-online` datasets are strictly segregated. Routes require an explicit `{collection}` or `?collection=` parameter. Cross-collection game retrieval returns `404 Not Found`.

### 5. Defensive Input Validation, Conditional Caching & Security
- Exactly one of `fen` or `moves` must be provided. Providing neither or both returns `400 Bad Request`.
- Move sequences are validated for legality from the standard initial board and capped at a configurable maximum length (default 200 plies).
- Unsupported parameters (`since_year`, `min_rating`) are explicitly parsed and rejected with `400 Bad Request` (`code: "unsupported_parameter"`).
- Non-existent positions in a valid index return `200 OK` with `total_games: 0` and empty candidate moves, matching standard chess database conventions.
- Conforming strictly to RFC 7232, game existence in `games.idx` is verified prior to evaluating `If-None-Match`. Non-existent games return `404 Not Found` rather than evaluating conditional caching or returning `304 Not Modified`.
- Default CORS configuration is restrictive (empty `allowed_origins`), permitting only same-origin requests unless explicit origins are provided.
- **Internal Error Sanitization**: HTTP 500 responses (`reload_failed`, `storage_error`, `internal_error`) log detailed diagnostics to server-side logs without leaking secrets, but return sanitized client-facing messages that never expose filesystem paths, storage internals, or diagnostic details.
- **Representation-Accurate Position ETags**: Position ETags identify the exact response representation per RFC 7232 by hashing collection identity, dataset build ID, canonical 128-bit position key, and the complete FEN string (including move clock counters). Equivalent board positions reached via move repetitions that alter FEN counters produce distinct ETags, while identical representations preserve conditional caching (`304 Not Modified`).

---

## Consequences

### Positive
- **High Performance**: Single-client position queries execute in $\sim 230\ \mu\text{s}$ p50; throughput reaches $> 3,000\text{ req/s}$ on a single connection.
- **Bounded Memory**: Process RSS remains $< 10\text{ MB}$, well within the $30\text{ MB}$ ingestion budget.
- **Snapshot Safety**: Version tearing across requests during dataset updates is mathematically impossible due to immutable `Arc` pinning.
- **Fail-Safe Operation**: Corrupt or half-published replacement versions cannot break active query serving.

### Deferred / Future Work
- **Year & Rating Filters**: Filtering by year or player rating requires multi-dimensional position indexing or secondary indices, deferred to future index revisions.
- **HTTP Caching**: ETags and `Cache-Control` headers can be enhanced with version-keyed cache tags when deployed behind CDN edge nodes.
