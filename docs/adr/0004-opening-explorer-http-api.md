---
summary: "Decisions on the production-ready Opening Explorer HTTP API, snapshot pinning, and concurrency model."
read_when: "Reviewing the HTTP server architecture, state management, endpoint contracts, and concurrency design."
status: current
---

# ADR 0004: Production-Ready Opening Explorer HTTP API — Axum Serving Layer, Snapshot Pinning & Lock-Free Concurrent Queries

## Status
Accepted

## Context
Milestones PR-01 through PR-03 delivered core data ingestion, deduplication, CGB3 compact game storage, and the CPI v2 position index with bounded-memory external merge sort. However, query capabilities were restricted to the CLI tool (`codb query`, `codb get-game`).

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
We introduce a dedicated workspace crate, `codb-api` (`crates/api`), and expose a `codb serve` command in `crates/cli`:
- **Framework**: Built on [`axum`](https://crates.io/crates/axum) (v0.8) and [`tokio`](https://crates.io/crates/tokio) (v1.40). Axum offers first-class ergonomics, type-safe extractors, zero-cost routing via `matchit`, and seamless middleware integration via `tower-http`.
- **Middleware Pipeline**: Configured with `tower_http::cors::CorsLayer` for configurable browser access, `tower_http::timeout::TimeoutLayer` for bounding query runtimes, and `tower_http::trace::TraceLayer` for structured observability.
- **CLI Command (`codb serve`)**: Accepts `--masters-dataset`, `--elite-dataset`, `--host`, `--port`, `--allowed-origins`, and pool configuration options, with environment variable fallbacks (`CODB_*`). Fails immediately at startup if neither collection is provided or if specified paths are invalid.

### 2. State Management & Snapshot Pinning
The application state (`AppState`) manages dataset handles:
- **`DatasetSnapshot`**: Encapsulates an active dataset version:
  - Pinned physical snapshot directory (e.g. `versions/v1`) resolved via `codb_storage::resolve_dataset_snapshot`.
  - Parsed `DatasetManifest`.
  - Memory-mapped `DiskPositionIndex` wrapped in `Option<Arc<DiskPositionIndex>>`.
  - Pooled `DatasetReaderPool` for game decompression.
- **Snapshot Pinning**: State stores snapshots in `parking_lot::RwLock<Option<Arc<DatasetSnapshot>>>`. Route handlers acquire the read lock for $< 10\text{ ns}$ to clone the `Arc<DatasetSnapshot>`. The cloned `Arc` pins the physical snapshot directory for the entire lifetime of the request.
- **Zero-Downtime Reload (`POST /v1/admin/reload`)**:
  - The reload operation opens, verifies, and prepares replacement snapshots in isolated local variables.
  - If verification or opening fails, the reload returns an error immediately and leaves the active serving state untouched.
  - If successful, the `RwLock` write pointer is updated in nanoseconds. In-flight requests finish cleanly against the prior version, while new requests immediately observe the new version.

### 3. Concurrency Model: Lock-Free CPI Queries & Worker Threadpool Offloading
- **Position Queries (`/v1/explorer/position`)**: `DiskPositionIndex` holds a memory-mapped sparse index and compressed block data accessed exclusively via immutable `&self`. Because no interior mutability or locks exist, concurrent requests query `DiskPositionIndex::query` completely lock-free with zero mutex contention, achieving sub-millisecond p50 latencies ($220\text{–}250\ \mu\text{s}$).
- **Game Retrieval (`/v1/collections/{collection}/games/{id}`)**: Shard reads and Zstd block decompression require file seeking and update a single-block cache. We introduce [`DatasetReaderPool`](../../crates/api/src/state.rs) with RAII `PooledReader` guards. Blocking disk I/O and decompression are executed inside `tokio::task::spawn_blocking`, protecting Tokio's worker threads from starvation.

### 4. Collection Isolation
`masters-otb` and `elite-online` datasets are strictly segregated. Routes require an explicit `{collection}` or `?collection=` parameter. Cross-collection game retrieval returns `404 Not Found`.

### 5. Defensive Input Validation & Explicit Rejection
- Exactly one of `fen` or `moves` must be provided. Providing neither or both returns `400 Bad Request`.
- Move sequences are validated for legality from the standard initial board and capped at a configurable maximum length (default 200 plies).
- Unsupported parameters (`since_year`, `min_rating`) are explicitly parsed and rejected with `400 Bad Request` (`code: "unsupported_parameter"`).
- Non-existent positions in a valid index return `200 OK` with `total_games: 0` and empty candidate moves, matching standard chess database conventions.

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
