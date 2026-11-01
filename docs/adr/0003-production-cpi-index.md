# ADR 0003: Production Chess Position Index (CPI v2) — Bounded-Memory External Sorting, Deterministic Aggregation, Atomic Publication & Explorer Queries

## Status
Accepted

## Context
PR-01 and PR-02 established scalable PGN ingestion, compact CGB3 block storage (~208 bytes/game for Masters OTB, ~177 bytes/game for Elite Online), persistent sharding, and random-access game retrieval via `games.idx`.

However, position indexing in prior milestones relied on an in-memory aggregation map (`ImmutablePositionIndex`), which stored every unique position and move candidate in a heap-allocated `BTreeMap`. While effective for benchmarks up to 20,000–100,000 games, this architecture cannot scale to the 15-million-game target (5M Masters OTB at depth 40 plies and 10M Elite Online at depth 30 plies):
1. **Unbounded RAM Allocation**: Extracting 30–40 plies across 15 million games yields hundreds of millions of raw position observations. Buffering them entirely in memory would require dozens of gigabytes of RAM, directly violating the bounded-memory design principle ($\le 128\text{ MiB}$ default limit).
2. **Deterministic Run Aggregation**: Games arrive in arbitrary order. To build the immutable, compressed CPI v2 format (which requires strictly sorted canonical `PositionKey` entries partitioned into contiguous blocks), entries must be globally ordered and aggregated across transpositions without non-deterministic variations.
3. **Data Integrity & Chess Rules**:
   - Starting positions with orthodox legal FENs must be indexed correctly, while non-orthodox or illegal FEN setups must fail closed.
   - Intra-game transpositions (e.g., three-fold repetitions or piece cycling) must not inflate game counts or result counters within the indexing depth window.
   - Incomplete games or games with unknown outcomes (`*` result) must be excluded from win/draw/loss aggregation while still verifying move legality.
4. **Crash Safety & Atomic Dataset Publication**: When adding or updating a position index on an existing dataset, the publication must be fully atomic, crash-safe, and self-verifying, ensuring live readers remain pinned to valid snapshots and failed builds never leave corrupt or half-written artifacts.

## Decisions

### 1. Bounded-Memory External Merge Sort Pipeline
We introduce [`ExternalPositionIndexer`](file:///home/nerver/Desktop/dev/chess-openings-db/crates/index/src/external.rs) in `crates/index`:
- Streams canonical games from immutable CGB3 shard snapshots without loading all games into memory.
- Buffers compact 24-byte observations `(PositionKey, CompactMove, GameResult)` in RAM up to a configurable ceiling (`--memory-limit-mib`, default 128 MiB).
- When the buffer reaches capacity, in-memory records are sorted by `(PositionKey, CompactMove)` and coalesced into 30-byte records `(key: u128, move: u16, w: u32, d: u32, b: u32)`.
- Coalesced records are flushed to temporary `.crun` run files on disk (format version 1, magic `CODB_RUN`, 30-byte records, 8-byte record count, and 32-byte trailing SHA-256 footer).
- If the number of run files exceeds the maximum fan-in $K$ (`--max-merge-fan-in`, default 64), a deterministic multi-pass hierarchical K-way merge is performed into intermediate runs, strictly bounding open file descriptors and merge buffer memory.
- The final K-way merge streams directly into [`StreamingCpiWriter`](file:///home/nerver/Desktop/dev/chess-openings-db/crates/index/src/position.rs), guaranteeing byte-for-byte identical output regardless of buffer limit or fan-in.

### 2. Strict Position Extraction Semantics
In [`crates/index/src/position.rs`](file:///home/nerver/Desktop/dev/chess-openings-db/crates/index/src/position.rs), [`extract_game_positions_detailed`](file:///home/nerver/Desktop/dev/chess-openings-db/crates/index/src/position.rs) enforces chess rules:
- **Starting Position Handling**: Standard orthodox starting positions and custom legal orthodox FEN setups (e.g. from FIDE broadcast relays) are supported. Games with illegal FEN setups fail closed.
- **Intra-game Repetition Deduplication**: A `HashSet<PositionKey>` tracks positions visited within the game's indexing depth window. If a position is revisited, it is skipped so that a single game contributes at most once to a position's `total_games` and move frequency statistics.
- **Game Result Accounting**: Games with unknown result (`*` / `GameResult::Star`) are counted in `games_processed` and `excluded_unknown_result_games`, but strictly omitted from W/D/L aggregation. Games with decisive results (`1-0`, `0-1`, `1/2-1/2`) update `white_wins`, `black_wins`, and `draws` respectively.

### 3. Production CPI v2 Format Compatibility
The pipeline produces byte-compatible CPI v2 artifacts (`positions.cpi`):
- **CPI Header** (40 bytes): Magic `CODB_CPI` (8 bytes), format version `2` (4 bytes), positions per block `u32`, total positions `u64`, total move pairs `u64`, block count `u32`, sparse index offset `u64`.
- **Compressed Blocks**: Sequential blocks of up to `positions_per_block` positions, compressed with Zstandard (`zstd-level`, default 3). Each block contains `u16` position count, binary position records (16-byte key, `u32` total games, `u32` W/D/L, candidate move list), and trailing CRC32 checksum.
- **Sparse Index**: In-memory or mmap-backed array of `(first_key: u128, file_offset: u64, compressed_len: u32, uncompressed_len: u32)` for $O(\log B)$ block location via binary search.

### 4. Atomic Dataset Publication and Recovery
In [`crates/storage/src/dataset.rs`](file:///home/nerver/Desktop/dev/chess-openings-db/crates/storage/src/dataset.rs), [`build_dataset_position_index`](file:///home/nerver/Desktop/dev/chess-openings-db/crates/storage/src/dataset.rs) implements atomic, transactional index publication:
1. Pins and verifies the active source snapshot via `resolve_dataset_snapshot(dataset_dir)`.
2. Reuses immutable CGB3 shards and `games.idx` via hard-links (falling back to file copies across filesystems) into a staged version directory `versions/.staging_index_<uuid>`.
3. Streams games from the pinned snapshot through `ExternalPositionIndexer` directly into `positions.cpi` in the staging directory.
4. Deeply verifies `positions.cpi` (magic, version, block decompression, CRC32, sparse index monotonic ordering).
5. Updates `manifest.json` with `position_index: PositionIndexMetadata` and commits the version directory `versions/<build_id>`.
6. Atomically switches the `current` symlink to `versions/<build_id>` via `renameat`.
7. Updates root convenience symlinks (`shards`, `games.idx`, `manifest.json`, and `positions.cpi`).
8. If interrupted by crash or power loss, [`recover_dataset`](file:///home/nerver/Desktop/dev/chess-openings-db/crates/storage/src/dataset.rs) cleans up staging directories and reconciles symlinks without data loss.

### 5. Opening Explorer CLI and Query Interface
- CLI subcommands:
  - `codb build-index --dataset <PATH> [--depth-plies <N>] [--collection <C>] [--memory-limit-mib <M>] [--json]`
  - `codb query --dataset <PATH> [--moves <MOVES>] [--fen <FEN>] [--json]`
- Candidate moves are decoded dynamically into Standard Algebraic Notation (SAN) alongside UCI representation.
- Structured JSON and formatted human-readable output report position keys, legal FEN strings, game counts, and candidate move statistics with percentages rounded to 1 decimal place.

## Alternatives Considered

1. **SQLite-backed Position Indexing**:
   - PR-01 and PR-02 benchmarked SQLite disk indexes alongside CPI. SQLite required 7.75x more disk storage than CPI (~160 B/pos vs ~20.6 B/pos) due to B-tree page overhead and uncompressed move tables. Storing 15M games in SQLite would require >60 GB, drastically exceeding the 10 GB limit.
2. **LSM-tree Key-Value Store (RocksDB / Sled)**:
   - While LSM-trees handle bounded-memory writes well, their write amplification, compaction background CPU overhead, and index metadata bloat would degrade deterministic publication and exceed deployment storage limits.
3. **Direct In-Place Appending**:
   - Appending to a live position index corrupts state on process crash and invalidates concurrent readers. An immutable snapshot-based external sort with atomic symlink publication provides true snapshot isolation with zero read-side locks.

## Consequences

- **Positive**: Memory usage during position index generation is strictly bounded to `--memory-limit-mib` (default 128 MiB).
- **Positive**: Index generation is 100% deterministic and byte-identical across arbitrary buffer sizes and merge fan-ins.
- **Positive**: Storage projections confirm the combined 15-million-game dataset (5M Masters D40 + 10M Elite D30) fits comfortably within the 10 GB limit (~7.47 GB baseline, ~9.66 GB conservative) with zero double-counting.
- **Positive**: Atomic symlink activation ensures non-disruptive, crash-safe publication and zero-downtime queries for concurrent readers.
