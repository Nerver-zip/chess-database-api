---
summary: "End-to-end architecture, crate boundaries, data flow, concurrency, and memory bounds."
read_when: "Understanding system design, data flow, concurrency guarantees, and crate relationships."
status: current
---

# System Architecture (`chess-openings-db`)

## 1. Executive Overview

`chess-openings-db` is a modular, high-performance chess database and position indexing engine written in Rust. It ingests large, heterogeneous PGN corpora, enforces strict legal move validation, canonicalizes headers, eliminates duplicates with prefix-supersession, stores compressed games in persistent sharded CGB3 containers, and builds immutable Compact Position Indexes (CPI v2) using bounded-memory external sorting.

The architecture is explicitly designed to operate under strict resource ceilings:
- **Streaming Ingestion Memory:** Peak RAM $\le 30\text{ MB}$ RSS regardless of input corpus size ($O(1)$ memory scaling).
- **Position Indexing Memory:** Peak accumulation buffer strictly bounded by `--memory-limit-mib` (default 128 MiB, configurable down to 16 MiB).
- **Total Deployed Storage Envelope:** $\le 10\text{ GB}$ deployed footprint for 15 million curated games (5M Masters OTB sliced to Depth 40, 10M Elite Online sliced to Depth 30).

---

## 2. Workspace Crate Boundaries

The workspace comprises six decoupled crates within `crates/`:

```
crates/
├── core/       # [codb-core] Fundamental domain types, move packing, and invariants
├── ingest/     # [codb-ingest] Streaming PGN parsing and full legal move replay
├── curate/     # [codb-curate] Policy filtering, header canonicalization, and deduplication
├── storage/    # [codb-storage] CGB3 container, sharding, atomic publishing, and locking
├── index/      # [codb-index] Position extraction, external merge sort, and CPI v2 indexer
└── cli/        # [codb-cli] CLI binary (codb), command dispatch, and verification tools
```

```mermaid
flowchart TD
    CLI["crates/cli (codb binary)"]
    INGEST["crates/ingest (PGN streaming & legal validation)"]
    CURATE["crates/curate (policy & deduplication)"]
    STORAGE["crates/storage (CGB3 shards, games.idx, locks, publication)"]
    INDEX["crates/index (CPI v2, external sort, queries)"]
    CORE["crates/core (models, CompactMove, PositionKey)"]

    CLI --> INGEST
    CLI --> CURATE
    CLI --> STORAGE
    CLI --> INDEX
    CLI --> CORE

    INGEST --> CORE
    CURATE --> CORE
    STORAGE --> CORE
    STORAGE --> INDEX
    INDEX --> CORE
```

### 2.1 `codb-core`
- **Location:** [`crates/core/`](../crates/core/)
- **Responsibilities:**
  - `Collection`: Strictly distinguishes `MastersOtb` from `EliteOnline`.
  - `CompactMove`: 16-bit bit-packed move representation (`from`: 6 bits, `to`: 6 bits, `promotion`: 4 bits).
  - `PositionKey`: 128-bit canonical Zobrist-like position hash capturing board squares, side to move, castling rights, and en passant availability.
  - `GameResult`: Lenient parsing for `1-0`, `0-1`, `1/2-1/2`, and `*`.
  - `StableGameId`: Deterministic 64-bit FNV-1a hash computed over canonical metadata key and normalized move sequence.

### 2.2 `codb-ingest`
- **Location:** [`crates/ingest/`](../crates/ingest/)
- **Responsibilities:**
  - Streaming PGN parser built on `pgn-reader` with transparent Zstandard decompression (`.pgn` or `.pgn.zst`).
  - Strict legal move validation using `shakmaty`: every move in every game is executed on an internal legal board state. Games with illegal moves fail closed and are rejected.
  - Time control evaluation: computes estimated game duration ($T_{\text{est}} = \text{base} + 40 \times \text{increment}$) to filter out bullet/ultrabullet games.

### 2.3 `codb-curate`
- **Location:** [`crates/curate/`](../crates/curate/)
- **Responsibilities:**
  - `policy.rs`: Concrete collection rules (`MastersOtbPolicy` and `EliteOnlinePolicy`), Elo/rating thresholds, minimum plies, and variant rejection.
  - `dedupe.rs`: Normalized metadata keys (`White|Black|Year|Event|Round`), move sequence prefix comparison, and superseding detection.
  - Dual-mode deduplication backends: `DeduplicatorBackend::Memory` (for tests/fixtures) and `DeduplicatorBackend::Disk` (SQLite-backed catalog in staging for multi-million game scaling).

### 2.4 `codb-storage`
- **Location:** [`crates/storage/`](../crates/storage/)
- **Responsibilities:**
  - `format.rs`: CGB3 block container (`CODB-v3`), block-level dictionary interning, and Zstandard compression.
  - `shard.rs`: Multi-shard file management (`shard_XXXX.cgb3`), 32-byte header `CGCS`, block descriptors, and automatic shard splitting.
  - `lookup.rs`: Global memory-mapped binary search index (`games.idx`) providing 16-byte fixed-record random lookup by 64-bit ID.
  - `lock.rs`: Process-level advisory file locking (`.dataset.lock`) using `flock(LOCK_EX | LOCK_NB)` to serialize mutations while permitting concurrent readers.
  - `dataset.rs`: Dataset publication coordinator, staging directory management, atomic symlink swaps, snapshot isolation, and crash recovery.

### 2.5 `codb-index`
- **Location:** [`crates/index/`](../crates/index/)
- **Responsibilities:**
  - `position.rs`: Canonical position extraction (`extract_game_positions_detailed`), intra-game repetition tracking, game result aggregation, and CPI v2 format writer (`positions.cpi`).
  - `external.rs`: Bounded-memory external merge sort pipeline (`ExternalPositionIndexer`), temporary run file emission (`.crun`), and deterministic hierarchical K-way merge.
  - Opening Explorer query execution: sub-millisecond memory-mapped sparse index lookup and block decompression for SAN, UCI, and legal FEN inputs.

### 2.6 `codb-cli`
- **Location:** [`crates/cli/`](../crates/cli/)
- **Responsibilities:**
  - CLI command parsing via `clap`: `build`, `build-index`, `query`, `verify`, `inspect`, `get-game`, `bench`.
  - Progress reporting and structured JSON diagnostic output.
  - Benchmark harnesses and verification gates.

---

## 3. End-to-End Data Flow

```mermaid
sequenceDiagram
    autonumber
    participant Source as PGN / PGN.zst Source
    participant Ingest as codb-ingest (Stream & Legal Replay)
    participant Curate as codb-curate (Policy & Deduplication)
    participant Storage as codb-storage (CGB3 Shards & games.idx)
    participant Index as codb-index (External Sort & CPI v2)
    participant Consumer as Opening Explorer / API Consumer

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
    Consumer->>Index: Query FEN or Move sequence
    Index->>Consumer: PositionStats (W/D/L counters, next move candidates)
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
    │   ├── shard_0000.cgb3 # First compressed persistent game shard (CGCS header)
    │   ├── shard_0001.cgb3 # Additional shard if corpus exceeds shard split threshold
    │   └── positions.cpi   # Production Compact Position Index (CPI v2)
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

`chess-openings-db` provides strong concurrency guarantees:
- **Lock-Free Read Snapshots:** Readers resolve `current` to its target canonical path (e.g., `versions/v1`) and pin open file descriptors. Readers require no locks, will never block on concurrent writes, and see a 100% consistent immutable snapshot even during active publication.
- **Serialized Mutations via Advisory Flock:** Mutations (`build`, `build-index`, `recover`) acquire an exclusive lock on `.dataset.lock` via `flock(fd, LOCK_EX | LOCK_NB)`. If another mutation process holds the lock, incoming operations fail closed immediately with `StorageError::Lock`.
- **Atomic Symlink Publication:** New versions are completely staged in `versions/.staging_<pid>_<timestamp>_<nonce>`, verified with deep integrity checks, and published via atomic symlink swap (`rename(temp_symlink, current)`). Readers observing `current` never observe a partial or half-written state.

---

## 6. Bounded-Memory Architecture

Memory limits are strictly enforced across all components:

| Component | Ingestion / Processing Strategy | Peak Memory Guarantee |
| :--- | :--- | :--- |
| **PGN Ingestion** | Streaming tokenization via `pgn-reader`, immediate legal validation | $< 30\text{ MB}$ RSS regardless of corpus size |
| **Deduplication** | Disk-backed SQLite index in staging directory | $< 64\text{ MB}$ buffer cache ceiling |
| **CGB3 Storage** | Single active block buffer ($N = 512$ games) flushed to disk | $\approx 2\text{ MB}$ uncompressed block RAM |
| **Position Indexing** | External merge sort with partitioned `.crun` spill files | Strictly bounded by `--memory-limit-mib` (default 128 MiB) |
| **Opening Query** | Memory-mapped sparse index ($34\text{ B/entry}$) + single block cache | $< 10\text{ MB}$ query overhead |

---

## 7. Integration Boundaries (Client Applications)

```mermaid
graph LR
    subgraph Engine["chess-openings-db (Core Storage & Indexing Engine)"]
        direction TB
        DB_STORE["Persistent CGB3 Archive<br/>(15M games < 3 GB)"]
        DB_INDEX["Production CPI v2 Index<br/>(D40/D30 < 5 GB)"]
        DB_API["Future Read-Only Service<br/>(HTTP / gRPC Explorer)"]
    end

    subgraph Client["Client Application"]
        direction TB
        CLIENT_WEB["Web/Desktop Frontend<br/>(Board, Moves, Reviews)"]
        CLIENT_LOCAL["Local Review Engine<br/>(Stockfish WASM)"]
        CLIENT_SYNC["Accounts & Sync Service<br/>(PostgreSQL)"]
    end

    CLIENT_WEB -->|HTTP GET /api/v1/explorer| DB_API
    DB_API --> DB_INDEX
    DB_API --> DB_STORE
```

- **Domain Decoupling:** `chess-openings-db` has zero dependency on UI frameworks, Stockfish engine binaries, or account synchronization.
- **Local-First Independence:** Client applications function 100% locally in guest mode without requiring a running `chess-openings-db` service. When configured, client applications query the opening explorer API to annotate games with opening tree statistics and win/draw/loss frequencies.
