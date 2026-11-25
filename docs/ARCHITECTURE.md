---
summary: "End-to-end architecture, crate boundaries, data flow, concurrency, and memory bounds."
read_when: "Understanding system design, data flow, concurrency guarantees, and crate relationships."
status: current
---

# Chess Database API Architecture

## 1. Executive Overview

Chess Database API is a modular chess database and position indexing engine written in Rust. It ingests large, heterogeneous PGN corpora, enforces strict legal move validation, canonicalizes headers, eliminates duplicates with prefix-supersession, stores compressed games in persistent sharded CGB3 containers, and builds immutable Compact Position Indexes (CPI v2) using bounded-memory external sorting.

Search-enabled snapshots add an immutable SQLite metadata catalog and sorted positional occurrence postings. The HTTP service pins one snapshot per request and uses its CPI, search and game readers.

The architecture is designed around explicit resource bounds:
- **Streaming Ingestion Memory:** ingestion is streaming with O(1) memory scaling relative to input size. Record executed-process VmHWM for each build host; no fixed 30 MB release cutoff applies.
- **Position Indexing Memory:** peak accumulation is bounded by the --memory-limit-mib option (default 128 MiB, configurable down to 16 MiB).
- **HTTP Runtime:** Compose applies a 512 MiB memory cap, a 2 CPU quota, bounded request and reader-pool timeouts, and a reader-pool capacity of 1–64 per snapshot.
- **Current Release Footprint:** exact corpus and container measurements are recorded in [CORPUS_BUILD.md](CORPUS_BUILD.md). The historical 15-million-game projection is research only, not a current deployment target.

---

## 2. Workspace Crate Boundaries

The workspace comprises seven decoupled crates within `crates/`:

```
crates/
├── core/       # [chess-database-core] Fundamental domain types, move packing, and invariants
├── ingest/     # [chess-database-ingest] Streaming PGN parsing and full legal move replay
├── curate/     # [chess-database-curate] Policy filtering, header canonicalization, and deduplication
├── storage/    # [chess-database-storage] CGB3 container, sharding, atomic publishing, and locking
├── index/      # [chess-database-index] Position extraction, external merge sort, and CPI v2 indexer
├── api/        # [chess-database-api] Read-only HTTP serving, discovery and Opening Explorer
└── cli/        # [chess-database-cli] CLI binary (chess-database), command dispatch, and verification tools
```

```mermaid
flowchart TD
    CLI["crates/cli (chess-database binary)"]
    API["crates/api (Axum HTTP server)"]
    INGEST["crates/ingest (PGN streaming & legal validation)"]
    CURATE["crates/curate (policy & deduplication)"]
    STORAGE["crates/storage (CGB3 shards, games.idx, locks, publication)"]
    INDEX["crates/index (CPI v2, search catalog, postings and queries)"]
    CORE["crates/core (models, CompactMove, PositionKey)"]

    CLI --> API
    CLI --> INGEST
    CLI --> CURATE
    CLI --> STORAGE
    CLI --> INDEX
    CLI --> CORE

    API --> STORAGE
    API --> INDEX
    API --> CORE

    INGEST --> CORE
    CURATE --> CORE
    STORAGE --> CORE
    STORAGE --> INDEX
    INDEX --> CORE
```

### 2.1 `chess-database-core`
- **Location:** [`crates/core/`](../crates/core/)
- **Responsibilities:**
  - `Collection`: Strictly distinguishes `MastersOtb` from `EliteOnline`.
  - `CompactMove`: 16-bit bit-packed move representation (`from`: 6 bits, `to`: 6 bits, `promotion`: 4 bits).
  - `PositionKey`: 128-bit canonical Zobrist-like position hash capturing board squares, side to move, castling rights, and en passant availability.
  - `GameResult`: Lenient parsing for `1-0`, `0-1`, `1/2-1/2`, and `*`.
  - `StableGameId`: Deterministic 64-bit FNV-1a hash computed over canonical metadata key and normalized move sequence.

### 2.2 `chess-database-ingest`
- **Location:** [`crates/ingest/`](../crates/ingest/)
- **Responsibilities:**
  - Streaming PGN parser built on `pgn-reader` with transparent Zstandard decompression (`.pgn` or `.pgn.zst`).
  - Strict legal move validation using `shakmaty`: every move in every game is executed on an internal legal board state. Games with illegal moves fail closed and are rejected.
  - Time control evaluation: computes estimated game duration ($T_{\text{est}} = \text{base} + 40 \times \text{increment}$) to filter out bullet/ultrabullet games.

### 2.3 `chess-database-curate`
- **Location:** [`crates/curate/`](../crates/curate/)
- **Responsibilities:**
  - `policy.rs`: Concrete collection rules (`MastersOtbPolicy` and `EliteOnlinePolicy`), Elo/rating thresholds, minimum plies, and variant rejection.
  - `dedupe.rs`: Normalized metadata keys (`White|Black|Year|Event|Round`), move sequence prefix comparison, and superseding detection.
  - Dual-mode deduplication backends: `DeduplicatorBackend::Memory` (for tests/fixtures) and `DeduplicatorBackend::Disk` (SQLite-backed catalog in staging for multi-million game scaling).

### 2.4 `chess-database-storage`
- **Location:** [`crates/storage/`](../crates/storage/)
- **Responsibilities:**
  - `format.rs`: CGB3 block container (format version 3), block-level dictionary interning, and Zstandard compression.
  - `shard.rs`: Multi-shard file management (`shard_XXXX.cgb3`), 32-byte header `CGCS`, block descriptors, and automatic shard splitting.
  - `lookup.rs`: Global memory-mapped binary search index (`games.idx`) providing 16-byte fixed-record random lookup by 64-bit ID.
  - `lock.rs`: Process-level advisory file locking (`.dataset.lock`) using `flock(LOCK_EX | LOCK_NB)` to serialize mutations while permitting concurrent readers.
  - `dataset.rs`: Dataset publication coordinator, staging directory management, atomic symlink swaps, snapshot isolation, and crash recovery.

### 2.5 `chess-database-index`
- **Location:** [`crates/index/`](../crates/index/)
- **Responsibilities:**
  - `position.rs`: Canonical position extraction (`extract_game_positions_detailed`), intra-game repetition tracking, game result aggregation, and CPI v2 format writer (`positions.cpi`).
  - `external.rs`: Bounded-memory external merge sort pipeline (`ExternalPositionIndexer`), temporary run file emission (`.crun`), and deterministic hierarchical K-way merge.
  - Opening Explorer query execution: sub-millisecond memory-mapped sparse index lookup and block decompression for SAN, UCI, and legal FEN inputs.

### 2.6 `chess-database-api`
- **Location:** [`crates/api/`](../crates/api/)
- **Responsibilities:**
  - Production-ready, asynchronous, read-only HTTP Opening Explorer service built on Axum 0.8 and Tokio.
  - Implements `/v1/explorer/position`, `/v1/collections`, `/health`, `/ready`, and `/v1/admin/reload`; the stored-PGN route is optional through `CHESS_DATABASE_ENABLE_GAME_RETRIEVAL`.
  - Zero-copy, lock-free concurrent query execution directly against memory-mapped CPI v2 indexes.
  - Snapshot isolation pinning immutable version directories (`versions/vX/`) and atomic zero-downtime hot-reloading via `parking_lot::RwLock<Arc<DatasetSnapshot>>`.
  - Pooled, bounded threadpool execution for Zstandard CGB3 game decompression without blocking asynchronous request loops.

### 2.7 `chess-database-cli`
- **Location:** [`crates/cli/`](../crates/cli/)
- **Responsibilities:**
  - Unified CLI command parsing via `clap`: `build`, `build-index`, `query`, `verify`, `inspect`, `report`, `get-game`, `bench`, and `serve`.
  - `chess-database build --report-json` and `chess-database report` emit machine-readable accounting, rejection-reason, and distribution reports consumed by the corpus release workflow.
  - Service daemon lifecycle management for `chess-database serve`, handling host, port, timeout, move ply limits, and graceful OS shutdown signals.
  - Progress reporting and structured JSON diagnostic output.
  - Benchmark harnesses and verification gates.

---

## 3. End-to-End Data Flow

```mermaid
sequenceDiagram
    autonumber
    participant Source as PGN / PGN.zst Source
    participant Ingest as chess-database-ingest (Stream & Legal Replay)
    participant Curate as chess-database-curate (Policy & Deduplication)
    participant Storage as chess-database-storage (CGB3 Shards & games.idx)
    participant Index as chess-database-index (External Sort & CPI v2)
    participant Search as Derived Metadata and Position Postings
    participant API as Axum snapshot server
    participant Consumer as API client

    Source->>Ingest: Stream PGN tokens
    Ingest->>Ingest: Validate legal moves (shakmaty)
    Ingest->>Curate: ParsedGame (moves, metadata, time control)
    Curate->>Curate: Evaluate Policy (Rating, Ply count, Variant)
    Curate->>Curate: Deduplicate (Prefix supersession, exact match)
    Curate->>Storage: Accepted unique ParsedGame
    Storage->>Storage: Intern into CGB3 block buffer (N=512)
    Storage->>Storage: Compress with zstd & write shard_XXXX.cgb3
    Storage->>Storage: Finalize games.idx & atomic symlink publication
    Storage->>Index: Read immutable CGB3 shard stream
    Index->>Index: Extract position keys (HashSet intra-game dedupe)
    Index->>Index: External sort via temporary .crun files
    Index->>Index: Deterministic K-way merge into positions.cpi
    Storage->>Search: Stream canonical games and source attribution
    Search->>Search: Build metadata catalog and merge sorted position occurrences
    Search->>Storage: Stage and deeply verify derived search snapshot
    Storage->>Storage: Atomically activate verified version
    API->>Storage: Verify and pin an immutable version
    Consumer->>API: Unfiltered Opening Explorer request
    API->>Index: Read pinned CPI
    Consumer->>API: Search or filtered Explorer request
    API->>Search: Bounded worker reads pinned catalog and postings
    API->>Consumer: Page or exact filtered counts
```

---

## 4. Sharding and Persistent Layout

Datasets are packaged as self-contained directory trees under `versions/v_<timestamp>_<nonce>` and exposed through an atomic symlink (`current`):

```
dataset_dir/
├── .dataset.lock           # Advisory flock file serializing writers
├── current -> versions/v1  # Atomic symlink pointing to active snapshot
└── versions/
    ├── v1/
    │   ├── manifest.json   # Dataset manifest, SHA-256 digests, and collection info
    │   ├── games.idx       # Sorted 16-byte fixed-record memory-mapped global index
    │   ├── shards/        # shard_0000.cgb3 and additional CGB3 shards
    │   ├── source-attribution.jsonl # Optional source attribution
    │   ├── positions.cpi   # Production Compact Position Index (CPI v2)
    │   └── search/         # Optional metadata.sqlite and positions.postings
    └── .staging_XXXX/      # Isolated build directory for in-flight publications
```

### 4.1 Shard Architecture (`shard.rs`)
- **Magic Header:** `CGCS` (`0x43474353`, 4 bytes).
- **Format Version:** `1` (`u32`).
- **Header Size:** Fixed 32 bytes with zero-padded reserved space.
- **Block Layout:** Sequential dictionary-interned CGB3 blocks compressed with Zstandard.
- **Shard Splitting:** Shards split automatically at configurable thresholds (e.g. 500 MB or 500,000 games) to ensure manageable file sizes across cloud storage and filesystem boundaries.

### 4.2 Global Lookup Index (`lookup.rs`)
- **Record Size:** Exactly 16 bytes per game:
  - `game_id: u64` (LittleEndian FNV-1a hash)
  - `shard_id: u16`
  - `block_index: u32`
  - `game_index_in_block: u16`
- **Lookup Cost:** $O(\log N)$ binary search over memory-mapped memory, followed by reading a single block descriptor and decompressing the target CGB3 block.

---

## 5. Concurrency Model and Locking (`lock.rs`)

`chess-database-api` provides strong concurrency guarantees:
- **Lock-Free Read Snapshots:** Readers resolve `current` to its target canonical path (e.g., `versions/v1`) and pin open file descriptors. Readers require no locks, will never block on concurrent writes, and see a 100% consistent immutable snapshot even during active publication.
- **Serialized Mutations via Advisory Flock:** Mutations (`build`, `build-index`, `recover`) acquire an exclusive lock on `.dataset.lock` via `flock(fd, LOCK_EX | LOCK_NB)`. If another mutation process holds the lock, incoming operations fail closed immediately with `StorageError::Lock`.
- **Atomic Symlink Publication:** New versions are completely staged in `versions/.staging_<pid>_<timestamp>_<nonce>`, verified with deep integrity checks, and published via atomic symlink swap (`rename(temp_symlink, current)`). Readers observing `current` never observe a partial or half-written state.
- **Bounded HTTP Reader Pool Concurrency:** The HTTP serving layer enforces strict capacity bounds on active CGB3 decompressing readers via an asynchronous semaphore (`tokio::sync::Semaphore`). Requests exceeding pool capacity wait safely without blocking Tokio runtime worker threads. Permits and reader instances are safely recycled or dropped under timeouts and cancellations.
- **Fail-Closed Snapshot Hot-Reloading:** Dynamic dataset reloads (`POST /v1/admin/reload` or Unix `SIGHUP`) validate replacement candidate snapshots deeply before activation (`verify_dataset` checksum check and CPI index verification). If candidate verification fails, the reload fails closed and the existing active snapshot remains 100% functional.
- **Administrative Access Control:** Administrative reload endpoints require explicit token authentication (`--admin-token`), reject cross-origin requests (`Origin` header present), and default to restrictive same-origin CORS.

---

## 6. Bounded-Memory Architecture

Memory limits are strictly enforced across all components:

| Component | Ingestion / Processing Strategy | Peak Memory Guarantee |
| :--- | :--- | :--- |
| **PGN Ingestion** | Streaming tokenization via `pgn-reader`, immediate legal validation | O(1) memory scaling relative to input size; measure per-process RSS on the build host |
| **Deduplication** | Disk-backed SQLite index in staging directory | 256 KiB SQLite cache plus a 512 KiB staging-catalog cache; temp store is disk-backed |
| **CGB3 Storage** | Single active block buffer ($N = 512$ games) flushed to disk | $\approx 2\text{ MB}$ uncompressed block RAM |
| **Position Indexing** | External merge sort with partitioned `.crun` spill files | Strictly bounded by `--memory-limit-mib` (default 128 MiB) |
| **Opening Query** | Memory-mapped sparse index ($34\text{ B/entry}$) and one decoded block per call | The decoder context is worker-local. The existing CPI block size limit bounds the decoded payload. |
| **HTTP Serving** | Bounded reader pool plus memory-mapped CPI/games.idx | Reader count and request waits are bounded; process RSS includes touched file-backed pages and is measured against the real releases |

> **Measured releases:** Build-step and real-container RSS measurements are recorded in [CORPUS_BUILD.md](CORPUS_BUILD.md) and its Docker integration evidence.

---

## 6.1 Corpus Orchestration Layer (Outside the Rust Workspace)

Source acquisition and release assembly are deliberately **not** part of the Rust pipeline. They live in a thin Python orchestration layer that only invokes the existing `opening-explorer` CLI:

```mermaid
flowchart TD
    CAT["corpus/catalog.json<br/>pinned sources, licenses, publisher SHA-256"] --> FETCH["scripts/corpus.py fetch<br/>HTTPS, resume, retry, checksum, atomic finalize"]
    FETCH --> SRC["data/sources/&lt;collection&gt;/*.pgn.zst<br/>(git-ignored)"]
    SRC --> BUILD["chess-database build --manifest<br/>policy, dedupe, CGB3"]
    BUILD --> ATTR["Optional Gigabase source-attribution JSONL<br/>separate from CGB3"]
    ATTR --> INDEX["chess-database build-index<br/>CPI v2, bounded memory"]
    INDEX --> VERIFY["chess-database verify --deep<br/>chess-database report"]
    VERIFY --> PUBLISH["chess-database publish<br/>complete immutable version + atomic current symlink"]
    PUBLISH --> PKG["corpus.py package<br/>verifiable .tar + SHA-256"]
    PUBLISH --> SERVE["chess-database serve (both collections)"]
```

The orchestration adds no binary format and does not reimplement ingestion, curation, deduplication, storage, or index semantics. It builds in a private data/work/ workspace, deeply verifies a complete candidate, moves the immutable version under the stable data/releases/<collection>/versions/ root, and atomically replaces the relative current symlink. Previous versions remain in place for readers pinned to an older snapshot. The opt-in Gigabase qualification path registers source tags and identity evidence in a checksummed JSONL sidecar; CGB3, CPI v2, and `games.idx` layouts are unchanged. See [CORPUS_BUILD.md](CORPUS_BUILD.md) and [DATA_CONTRACTS.md](DATA_CONTRACTS.md#4-dataset-manifest-contract-manifestjson).

---

## 7. Docker Runtime Boundary

The multi-stage image ships `chess-database`, its compatibility executable path and runtime libraries. Compose mounts each stable dataset root read-only. Startup and reload verify all manifest-recorded checksums and required headers before activating a snapshot. Builds and installs perform deep structural verification before publication. Set `CHESS_DATABASE_DEEP_VALIDATE_ON_STARTUP=true` to repeat those deep checks during serving operations. The service binds port 8080 on the container network and exposes the `chess-database`, `opening-explorer` and `codb` aliases. See [Docker deployment](DOCKER_DEPLOYMENT.md).

## 8. Integration Boundaries (Client Applications)

```mermaid
graph LR
    subgraph Engine["chess-database-api (Core Storage & Indexing Engine)"]
        direction TB
        DB_STORE["Persistent CGB3 Archive"]
        DB_INDEX["Production CPI v2 Index"]
        DB_SEARCH["Optional metadata and position postings"]
        DB_API["Read-Only HTTP Service<br/>(Axum chess-database serve)"]
    end

    subgraph Client["Client Application"]
        direction TB
        CLIENT_WEB["Web/Desktop Frontend<br/>(Board, Moves, Reviews)"]
        CLIENT_LOCAL["Local Review Engine<br/>(Stockfish WASM)"]
        CLIENT_SYNC["Accounts & Sync Service<br/>(PostgreSQL)"]
    end

    CLIENT_WEB -->|HTTP GET /v1/explorer/position| DB_API
    CLIENT_WEB -.->|optional HTTP GET /v1/collections/.../games/...| DB_API
    CLIENT_WEB -.->|optional search| DB_API
    DB_API --> DB_SEARCH
    DB_API --> DB_INDEX
    DB_API --> DB_STORE
```

- **Domain Decoupling:** `chess-database-api` has zero dependency on UI frameworks, Stockfish engine binaries, or account synchronization.
- **Local-First Independence:** Client applications can use the read-only HTTP interface from a backend gateway. The service provides stored games, discovery and Opening Explorer statistics independently of how the client implements its board and analysis features.

## 9. Derived search

The [search index](SEARCH_INDEX.md) projects canonical metadata into a bounded-cache SQLite catalog and maps position keys to sorted game occurrences. Requests pin one immutable snapshot. The default admission limit is two blocking search workers with a short bounded queue. A single aggregation slot inside that limit keeps CPU-intensive filtered queries from occupying both workers.

The legacy Explorer route uses CPI v2 with worker-local Zstandard decoder reuse and per-query block validation. For simple rating, date and result filters, a bounded dense ordinal projection avoids walking SQLite candidate rows. Other predicates use the selectivity-aware SQL and posting planner. Both paths apply CPI first-visit semantics and cooperative cancellation.
