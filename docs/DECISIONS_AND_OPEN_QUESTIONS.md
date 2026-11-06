---
summary: "Accepted architectural decisions, trade-off analysis, and open questions for the future API."
read_when: "Reviewing rationale behind core design decisions or addressing open questions for the API service."
status: current
---

# Decisions & Open Questions

## 1. Summary of Accepted Architectural Decisions

The following architectural decisions have been evaluated, implemented, and accepted into production:

### 1.1 Dual Collection Isolation
- **Decision:** Strictly segregate `masters-otb` and `elite-online` into independent data streams, shards, indexes, and queries.
- **Rationale:** FIDE Elo and online platform Glicko-2 ratings possess distinct mathematics, inflation rates, and error bounds. Pooling them destroys analytical validity and corrupts opening win rates.

### 1.2 16-Bit Packed Move Representation (`CompactMove`)
- **Decision:** Pack all standard moves into 16 bits (`from`: 6b, `to`: 6b, `promo`: 4b).
- **Rationale:** Replaying against a legal board state resolves all captures, checks, and castling flags without needing variable-length SAN strings or expensive parsing during random game retrieval.

### 1.3 Persistent Sharding (`shard_XXXX.cgb3`) and Binary Search Index (`games.idx`)
- **Decision:** Split persistent archives into bounded shards (e.g. 500 MB / 500k games) indexed by a fixed-record 16-byte binary search table.
- **Rationale:** Prevents multi-gigabyte monolithic files, simplifies incremental distribution over CDN or cloud object stores, and delivers sub-millisecond game retrieval via $O(\log N)$ binary search.

### 1.4 Bounded-Memory External Sorting for CPI Indexing
- **Decision:** Stream games from immutable CGB3 shards, accumulate observations in RAM up to `--memory-limit-mib` (default 128 MiB), flush sorted runs (`.crun`), and execute a deterministic hierarchical K-way merge.
- **Rationale:** Eliminates the memory cliff of in-memory maps, allowing 15 million games to be indexed on commodity development machines or modest CI runners with deterministic, byte-for-byte reproducible output.

### 1.5 Depth Slicing (Option B: Masters D40 / Elite D30)
- **Decision:** Slicing Masters OTB to Depth 40 and Elite Online to Depth 30 plies.
- **Rationale:** Masters OTB theory extends deeply into classical lines, whereas online play diversifies widely in early plies. Depth 30 for Elite Online preserves high-frequency opening statistics while constraining conservative deployed storage to 9.98 GB, strictly satisfying the 10 GB envelope constraint.

---

## 2. Historical Research Classification

The initial research reports created during the exploratory phase:
- [`docs/research/feasibility-report.md`](research/feasibility-report.md)
- [`docs/research/storage-options.md`](research/storage-options.md)

are officially designated as **Historical Research Documents**. They record early trade-off explorations (such as comparative evaluation of SQLite and RocksDB before CGB3 was developed) and initial mathematical derivations. Canonical system contracts, formats, and performance baselines are defined in [docs/ARCHITECTURE.md](ARCHITECTURE.md), [docs/DATA_CONTRACTS.md](DATA_CONTRACTS.md), and [docs/BENCHMARKS.md](BENCHMARKS.md).

---

## 3. Open Architectural Questions for the Future API Service

As the project prepares for the future HTTP/gRPC Opening Explorer API, several architectural decisions remain open for consideration:

### Q-01: In-Memory Block Cache vs. Exclusive Relying on OS Page Cache
- **Context:** Currently, `codb query` memory-maps the sparse index ($34\text{ B/entry}$) and reads/decompresses individual Zstd position blocks on demand.
- **Options:**
  1. *OS Page Cache Only:* Rely entirely on `mmap` and the Linux kernel buffer cache.
  2. *Application-Level LRU Decompressed Block Cache:* Maintain a small in-memory LRU cache of recently decompressed `PositionBlock` structures (e.g. 64–256 MiB) to avoid repeated Zstd decompressions for root opening positions (e.g., `1. e4`, `1. d4`).
- **Status:** Open. Preliminary benchmark latency ($p_{50} \approx 600\text{ }\mu\text{s}$) suggests OS page caching is sufficient for low-to-medium loads, but a high-throughput public API may benefit from a small root block cache.

### Q-02: Dataset Version Hot-Reloading Mechanism
- **Context:** When a background indexing or update job publishes a new dataset version (updating the `current` symlink), active API server threads must transition to the new version smoothly.
- **Options:**
  1. *Filesystem Inotify Watcher:* Listen for symlink changes on `current` and atomically reload `positions.cpi` and `games.idx` references.
  2. *Per-Request Generation Pointer:* Use an `ArcSwap` reference cell holding open file handles, replacing the pointer upon receiving a SIGHUP signal or scheduled check.
- **Status:** Open. Option 2 provides clean lock-free reads and ensures active in-flight requests finish cleanly against their pinned snapshot.

### Q-03: HTTP Daemon Framework Selection
- **Context:** The API layer should be lightweight, fast, and secure.
- **Options:**
  1. `axum`: Modern, modular, built on `tokio` and `tower`, excellent ergonomic routing and state sharing.
  2. `actix-web`: High raw throughput, mature ecosystem, slightly heavier dependencies.
- **Status:** Recommendation: `axum` due to its alignment with modern Rust asynchronous idioms and minimal footprint.

### Q-04: Transposition Graph Traversal vs Single-Position Queries
- **Context:** The current CPI v2 index maps individual canonical positions to candidate next moves and outcome frequencies.
- **Options:**
  1. *Single-Ply Lookups:* Client queries position $P$; server returns candidate moves from $P$. Client navigates move-by-move.
  2. *Sub-Tree Prefetching:* Client queries position $P$ with depth parameter $D \le 3$; server returns a mini-tree of candidate branches.
- **Status:** Open. Single-ply lookups match the standard Lichess Opening Explorer behavior and minimize network payload size.
