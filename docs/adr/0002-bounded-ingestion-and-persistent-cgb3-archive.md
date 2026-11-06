---
summary: "Decisions on bounded-memory streaming ingestion, persistent CGB3 sharding, and global lookup index."
read_when: "Reviewing the transition to O(1) RAM streaming, disk-backed deduplication, and CGB3 container sharding."
status: current
---

# ADR 0002: Scalable Bounded-Memory Ingestion, Persistent CGB3 Container Sharding, and Global Game Lookup

## Status
Accepted

## Context
PR-01 established foundational feasibility for the Evalis Opening Explorer database, demonstrating CGB3 block compression (~208 bytes/game for Masters OTB) and immutable CPI v2 position indexing. However, PR-01 exhibited five architectural limitations preventing deployment at scale (15M target games):
1. **Unbounded RAM Retention**: `ingest_curated_stream()` retained every accepted game in a heap-allocated `Vec<ParsedGame>`, causing memory usage to scale linearly with accepted game count.
2. **In-Memory Deduplication**: `Deduplicator` maintained `Vec<CompactMove>` and strings for all seen games in memory, exhausting RAM on large corpora.
3. **Absence of Persistent Storage Container**: CGB3 existed only as single compressed byte blocks in memory rather than a structured on-disk container with sharding and stable random-access game retrieval.
4. **Unvalidated Elite Online Metrics**: The January 2013 Lichess sample yielded 0 accepted games under `EliteOnlinePolicy`, leaving online compression and position growth unmeasured empirically.
5. **Lack of Verified Atomic Publishing and Manifests**: No persistent dataset manifest, integrity verification, or crash-safe staging existed.

## Decisions

### 1. Incremental Streaming Pipeline
Refactor ingestion into an incremental streaming pipeline:
`PGN Stream -> Streaming Parser -> Legal Move Validation -> Policy Filter -> Deduplication -> Persistent Archive Writer -> Finalized Dataset`.
The ingestion pipeline flushes games into compressed CGB3 blocks as they are accepted, holding at most one block buffer (default 512 games) in RAM. Total accepted game retention in RAM is eliminated ($O(1)$ memory scaling with corpus size).

### 2. Scalable Bounded-Memory Deduplication
Introduce a dual-mode deduplication architecture:
- `DeduplicatorBackend::Memory`: In-memory hash maps for fast unit tests, deterministic fixtures, and CI runs.
- `DeduplicatorBackend::Disk`: Embedded disk-backed temporary storage (using SQLite in the staging build directory) for large-scale ingestion (>20,000 games). It indexes normalized metadata keys and move sequence hashes on disk, fetching move sequences on demand to verify exact equality and prefix supersession. Memory consumption is bounded to the database cache size (<64 MB) regardless of corpus size. The temporary store is discarded upon finalization.

### 3. Persistent CGB3 Shard Container (`shard_XXXX.cgb3`)
Implement a persistent binary shard format:
- **Shard Header** (32 bytes): Magic `b"CGCS"`, format version (`u32`), shard index (`u16`), collection ID (`u8`), block count (`u32`), reserved padding.
- **Block Directory**: Array of block descriptors recording `file_offset: u64`, `compressed_size: u32`, `uncompressed_size: u32`, `game_count: u32`, and `checksum: u32`.
- **Compressed Blocks**: Sequential CGB3 dictionary-interned blocks compressed with Zstandard.
Shards split automatically at configurable thresholds (e.g. 500 MB or 500,000 games) to prevent massive monolithic files.

### 4. Global Persistent Game Lookup (`games.idx`)
Implement a persistent, memory-mapped global lookup index:
- Fixed 16-byte record per game:
  - `game_id: u64` (FNV-1a 64-bit stable identifier)
  - `shard_id: u16`
  - `block_index: u32`
  - `game_index_in_block: u16`
- Entries are sorted strictly by `game_id` during finalization.
- Random retrieval uses binary search over `memmap2`-backed slices: $O(\log N)$ time, $O(1)$ RAM, sub-microsecond page-cache latency.
- Non-contiguous hash IDs are supported natively. Potential collisions are handled deterministically by grouping identical IDs and verifying candidate metadata/moves against the requested game.

### 5. Supersession and Final Compaction
When an incoming game is a verified prefix extension of a previously ingested game:
- If the prior game is in the uncommitted in-memory block buffer, it is replaced in-place.
- If the prior game has been committed to a shard block on disk, the previous game location is tombstoned and the active lookup points to the new game.
- At dataset finalization, any blocks containing tombstones are compacted and rewritten, ensuring the published dataset contains 100% active, non-superseded canonical games with zero dangling records.

### 6. Atomic Staging and Manifest Verification
All builds construct artifacts inside a staging directory (`<dataset>.staging.<uuid>`). Only after:
1. All shards and lookup files are written,
2. SHA-256 checksums are calculated,
3. Accounting invariants are verified (`accepted_unique == retained_canonical_games`),
4. Structural and block integrity are self-checked,
is `manifest.json` written and the directory atomically renamed to the finalized target path.

## Consequences
- **Positive**: Memory usage during ingestion is strictly bounded by configuration rather than corpus size.
- **Positive**: Multi-block archives can be built, stored, verified, and queried by stable game ID across shards in microseconds without full scans.
- **Positive**: Build artifacts are crash-safe and reproducible.
- **Positive**: Deduplication across multiple files, shards, and batches is fully preserved.
- **Neutral**: Disk I/O increases slightly during build due to temporary deduplication indexing, but deployed footprint remains compact and read-optimized.
