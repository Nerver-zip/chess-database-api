# Chess Openings DB (`codb`)

> **Status: Storage Engineering & Empirical Scale Validation Milestone (PR-02 Complete)**  
> This repository contains the scalable persistent game storage pipeline, bounded-memory streaming ingestion engine, modern Elite Online empirical validation, and empirical scale benchmarks for an independent chess game database and Opening Explorer engine.

A high-performance, compact, and reproducible chess game database and position-centric indexing engine written in Rust. The system curates, deduplicates, compresses, and indexes high-quality chess games to power a position-based Opening Explorer through a lightweight, reusable API.

---

## The Central Feasibility Question

> **Can we build a reliable, reproducible, publicly redistributable chess database containing millions of high-quality games, with full game retrieval and position-based opening statistics, within approximately 10 GB of deployed storage?**

### Conclusion: **Feasibility Confirmed Across Real Datasets at Scale**

Based on reproducible measurements across 117,616 real Lichess Official Broadcast games (22,265 accepted after curation and prefix-verified deduplication) and over 5,000,000 modern Lichess Standard Rated games (up to 100,000 accepted games curated across 2024–2026 archives):

- **Bounded-Memory Streaming Pipeline (PR-02):**
  - **$O(1)$ RAM Scaling:** Eliminates whole-corpus memory retention during ingestion by streaming directly through validation, disk-backed deduplication, and compressed persistent CGB3 shards. Peak RAM stays strictly below **30 MB RSS** even when processing millions of games.
  - **High Throughput:** Reaches **~25,000 games/sec** on local streams and **3,000–4,000 games/sec** on piped network streams with full legal move validation.
- **Persistent Sharded Storage (`CGB3` Container):**
  - **Masters OTB:** **~208 bytes/game** in CGB3 blocks (5.16× compression ratio over PGN) with 100% round-trip fidelity across all supported canonical and provenance fields.
  - **Elite Online:** **~178–181 bytes/game** in CGB3 blocks (~15% more compact than Masters OTB due to typical online game lengths).
  - **Sharding & Global Index:** Encoded in `shard_XXXX.cgb3` with a 16-byte fixed-record `games.idx` memory-mapped binary search table.
  - **Random Retrieval Latency:** Retrieves individual games by 64-bit ID in **698 µs (p50)**, **773 µs (p95)**, and **863 µs (p99)** across 100k-game persistent archives.
- **Empirical Position Diversity & Scaling (Heaps' Law):**
  - **Online Diversity:** Elite Online exhibits **~40% higher position diversity** at Depth 20 and **~20% higher diversity** at Depth 30 than Masters OTB, accurately reflecting diverse online opening repertoires vs. classical grandmaster theory.
  - **Dual-Corpus Scaling Fits:** Masters OTB ($\alpha_{D40} = 0.9293$, $k = 49.88$, $R^2 = 0.99998$) and Elite Online ($\alpha_{D30} = 0.9293$, $k = 37.63$, $R^2 = 0.99999$).
- **Recalibrated 15-Million-Game Deployment Projections (Option B):**
  - Slicing **Masters OTB (5M games) to Depth 40** and **Elite Online (10M games) to Depth 30** projects to:
    - **Baseline:** **7.47 GB** (Core payloads: 1.04 GB Masters games + 1.81 GB Elite games + 1.72 GB Masters D40 index + 2.45 GB Elite D30 index + 0.44 GB ancillary overhead).
    - **Optimistic Scenario:** **6.28 GB**.
    - **Conservative Scenario:** **9.66 GB** (incorporating sensitivity margins).
  - **Conclusion:** Fully feasible within the 10 GB envelope under all tested scenarios.

---

## Two Distinct Curated Collections

The database maintains two strictly isolated collections with distinct rating semantics and provenance:

1. **Masters OTB:** Over-the-board competitive tournament games, prioritizing verified classical events, high-level masters, and historical tournaments (sourced from Lichess Official Broadcasts under CC BY-SA 4.0 and Public Domain historical archives).
2. **Elite Online:** High-rated online games filtering for verified high rating tiers (2200+, 2400+, 2500+), clean time controls (classical/rapid/blitz; bullet strictly excluded), and non-aborted games (sourced from Lichess Rated Archives under CC0 1.0).

> [!IMPORTANT]
> OTB FIDE ratings and online platform ratings are **never** conflated or merged. Position statistics and player ratings remain independently queryable with collection isolation.

---

## Architectural Boundaries & Relationship to Evalis

- **`chess-openings-db`** owns ingestion, curation, legal validation, compact storage, position indexing, and the future read-only query API.
- **Evalis** owns the graphical user interface, board navigation, local review integration, and private user game analysis.
- **Decoupled:** The future database service is independently deployable and usable by third-party applications. Evalis remains 100% functional in local-first mode without requiring a remote database connection.

---

## Workspace Architecture

```
chess-openings-db/
├── crates/
│   ├── core/       # Fundamental domain models (GameMetadata, Collection, CompactMove, PositionKey)
│   ├── ingest/     # Streaming PGN parser (Zstandard streaming, full legal move replay)
│   ├── curate/     # Deduplication, header canonicalization, collection filtering
│   ├── storage/    # 16-bit binary move encoding, dictionary compression, block-zstd container
│   ├── index/      # Canonical 128-bit position keys, next-move aggregation, W/D/L stats
│   └── cli/        # CLI tool (`codb`) for benchmarking, ingestion, and verification
├── docs/
│   ├── research/   # Formal research tracks (licenses, corpus policy, dedupe, feasibility)
│   │   ├── source-inventory.md
│   │   ├── source-licenses.md
│   │   ├── corpus-policy.md
│   │   ├── deduplication.md
│   │   ├── storage-options.md
│   │   ├── feasibility-report.md
│   │   └── api-contract.md
│   └── adr/        # Architecture Decision Records
│       ├── 0001-initial-architecture.md
│       └── 0002-bounded-ingestion-and-persistent-cgb3-archive.md
├── fixtures/       # Deterministic PGN test fixtures and edge-case suites
├── benchmarks/     # Automated benchmark scripts and machine-readable results
└── .github/        # GitHub Actions CI workflow
```

---

## CLI Usage (`codb`)

### 1. Build Persistent Curated Dataset
Stream large PGN or PGN.zst sources directly into compressed, sharded CGB3 archives:
```bash
./target/release/codb build \
    --input data/masters_combined.pgn.zst \
    --collection masters-otb \
    --output data/builds/masters-otb-20k \
    --max-games 20000
```

### 2. Verify Dataset Integrity
Verify block checksums, manifest accounting, and optional deep decompression of every move sequence:
```bash
./target/release/codb verify --dataset data/builds/masters-otb-20k --deep
```

### 3. Inspect Dataset Structure
Inspect shard layout, total retained games, and physical byte density:
```bash
./target/release/codb inspect --dataset data/builds/masters-otb-20k
```

### 4. Retrieve Game by Stable ID
Random access lookup in sub-millisecond page-cache time:
```bash
./target/release/codb get-game --dataset data/builds/masters-otb-20k --id 1234567890
```

---

## Reproducing the Benchmarks

### 1. Build the Release Binary
```bash
cargo build --release
```

### 2. Run Deterministic Stage A Validation (1,000 games)
```bash
./target/release/codb bench \
    --input fixtures/stage_a_validation.pgn \
    --collection masters-otb \
    --run-name stage_a_validation
```

### 3. Run Stage B & Progressive Scaling Benchmarks
```bash
# Lightweight CI validation (no external data required):
./scripts/reproduce_benchmarks.sh --ci

# Full empirical reproduction (when external source datasets are present in data/):
./scripts/reproduce_benchmarks.sh --all
```

For complete step-by-step reproduction instructions, dataset checksums, and combined corpus assembly, see the [Benchmark Reproduction Guide](docs/research/benchmark-reproduction.md).

Machine-readable JSON reports are written to `benchmarks/results/`.

---

## Research Deliverables

- [Source Inventory & Classification Matrix](docs/research/source-inventory.md)
- [Source Licensing & Legal Architecture](docs/research/source-licenses.md)
- [Corpus Inclusion Policies](docs/research/corpus-policy.md)
- [Deduplication & Conflict Resolution](docs/research/deduplication.md)
- [Compact Storage & Layout Options](docs/research/storage-options.md)
- [Storage Feasibility & Scaling Report](docs/research/feasibility-report.md)
- [Benchmark Reproduction & Methodology Guide](docs/research/benchmark-reproduction.md)
- [Future API Contract & Decoupling](docs/research/api-contract.md)
- [ADR-0001: Initial Architecture Decisions](docs/adr/0001-initial-architecture.md)
- [ADR-0002: Bounded-Memory Ingestion, CGB3 Sharding & Global Game Lookup](docs/adr/0002-bounded-ingestion-and-persistent-cgb3-archive.md)

---

## Licenses & Data Redistribution

- **Software:** Independently authored and dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
- **Elite Online Data:** Creative Commons CC0 1.0 Universal.
- **Masters OTB Data:** Creative Commons Attribution-ShareAlike 4.0 (CC BY-SA 4.0).
- **Zero AGPL Code Reuse:** Designed and implemented independently from first principles.
