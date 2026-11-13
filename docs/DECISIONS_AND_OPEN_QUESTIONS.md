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

### 1.6 Production Read-Only HTTP API Architecture (ADR-0004)
- **Decision:** Build the Opening Explorer serving layer as a dedicated crate (`codb-api`) using Axum 0.8, Tokio, and Tower, integrated into the CLI via `codb serve`.
- **Rationale:** Axum provides modular, zero-cost routing with first-class state sharing via `Arc`. CPI v2 position queries run 100% lock-free across concurrent worker threads directly against memory-mapped data (`DiskPositionIndex`), achieving sub-millisecond p50 latencies (~220–250 µs). CGB3 game decompression is offloaded to a bounded reader pool (`DatasetReaderPool`) on `tokio::task::spawn_blocking`.
- **Zero-Downtime Reloading:** Pinned snapshots (`DatasetSnapshot`) are swapped atomically via `parking_lot::RwLock<Arc<DatasetSnapshot>>` upon `POST /v1/admin/reload`, allowing seamless updates under live traffic with zero dropped requests.

### 1.7 Corpus Orchestration Outside the Rust Workspace
- **Decision:** Source acquisition, release assembly, packaging, and HTTP validation live in `scripts/corpus.py` (Python standard library) driven by a pinned `corpus/catalog.json`, and invoke the existing `codb` CLI instead of adding download or release logic to the Rust crates.
- **Rationale:** Acquisition and provenance are release-process concerns that change independently of storage formats; keeping them out of `codb-*` preserves the binary-format and determinism contracts and avoids new Rust dependencies (HTTP client, TLS). The catalog fails closed on missing checksums and unapproved licenses.
- **Trade-off:** A second language in the repository; mitigated by stdlib-only code, offline tests in `validate.sh`, and no new dependencies.

---

## 2. Historical Research Classification

The initial research reports created during the exploratory phase:
- [`docs/research/feasibility-report.md`](research/feasibility-report.md)
- [`docs/research/storage-options.md`](research/storage-options.md)

are officially designated as **Historical Research Documents**. They record early trade-off explorations (such as comparative evaluation of SQLite and RocksDB before CGB3 was developed) and initial mathematical derivations. Canonical system contracts, formats, and performance baselines are defined in [docs/ARCHITECTURE.md](ARCHITECTURE.md), [docs/DATA_CONTRACTS.md](DATA_CONTRACTS.md), and [docs/BENCHMARKS.md](BENCHMARKS.md).

---

## 3. Open Architectural Questions for Future Milestones

With the core HTTP serving layer delivered in PR-04, Q-01 through Q-03 remain open for future milestones. Q-04 records a resolved baseline memory issue.

### Q-01: Application-Level In-Memory Block Cache vs OS Page Cache
- **Context:** `positions.cpi` lookups memory-map the sparse index and read/decompress individual Zstd blocks on demand. Single-query p50 latency is ~250 µs under Linux page caching.
- **Options:**
  1. *OS Page Cache Only:* Rely entirely on `mmap` and the Linux kernel buffer cache. Eliminates custom cache invalidation and memory management.
  2. *Application-Level LRU Block Cache:* Maintain a small in-memory LRU cache of recently decompressed `PositionBlock` structures (e.g. 64–256 MiB) to avoid repeated Zstd decompressions for high-frequency root moves.
- **Status:** Open. Current empirical benchmarks demonstrate >3,000 req/s throughput and sub-millisecond latencies using OS page caching alone within an 8.5 MB RSS envelope. A block cache may be evaluated if serving high-concurrency public traffic.

### Q-02: Automatic Inotify Symlink Watcher vs Administrative Reload
- **Context:** ADR-0004 implemented administrative reload (`POST /v1/admin/reload`) which re-reads the active `current` symlinks in 0.84 ms without dropping in-flight requests.
- **Options:**
  1. *Explicit Admin Trigger:* Continue using `POST /v1/admin/reload` triggered by automated deployment scripts after dataset publication.
  2. *Filesystem Inotify Watcher:* Background daemon thread watching directory updates and triggering reload automatically.
- **Status:** Open. Explicit reload provides deterministic deployment synchronization, whereas inotify eliminates the need for orchestration webhooks.

### Q-03: Precomputed Opening Classification (ECO) Engine
- **Context:** Opening Explorer consumers often desire canonical ECO codes and opening names (e.g., "Sicilian Defense: Najdorf Variation") alongside move candidate statistics.
- **Options:**
  1. *Client-Side Lookup:* Clients maintain an ECO dictionary mapping position keys to opening names.
  2. *CPI Sidecar Index:* A lightweight ancillary binary table mapping `PositionKey` to interned ECO string IDs embedded into the dataset directory.
- **Status:** Open for future exploration.

### Q-04: Ingestion Memory Ceiling (resolved)
- **Context:** The original real builds peaked at 44.4 MB (Masters) and 67.3 MB (an Elite pilot) because SQLite used a 32 MiB dedupe cache plus a 4 MiB staging-catalog cache.
- **Action taken:** The dedupe cache was reduced to 256 KiB, SQLite temporary storage moved to disk, and the staging-catalog cache was reduced to 512 KiB. Same-input rebuilds of the current releases produce byte-identical `games.idx`, `positions.cpi`, and shard files.
- **Current measurement:** `codb build` peaked at 29,339,648 bytes (Masters, 42,612 accepted games) and 29,851,648 bytes (Elite, 429,150 games, 8,522,353,902 input bytes). Both measurements are below 30,000,000 bytes; Elite had 148,352 bytes of headroom on the 4-core Linux host. See [`benchmarks/corpus/*/metrics.json`](../benchmarks/corpus/) and [`CORPUS_BUILD.md`](CORPUS_BUILD.md).
- **Limit:** This closes the measured baseline gap, not a platform-independent RSS guarantee; allocator, OS, and compression-library versions can change process memory. Re-measure after runtime changes.
- **Status:** Closed for the delivered baseline; retain the hard 30 MB target and validate any future scale expansion.
