# Chess Openings DB (`codb`)

> **Status: Research & Feasibility Milestone (PR-01 Complete)**  
> This repository contains the foundational research, prototypes, and empirical benchmarks establishing the technical, legal, and storage feasibility of an independent chess game database and Opening Explorer engine.

A high-performance, compact, and reproducible chess game database and position-centric indexing engine written in Rust. The system curates, deduplicates, compresses, and indexes high-quality chess games to power a position-based Opening Explorer through a lightweight, reusable API.

---

## The Central Feasibility Question

> **Can we build a reliable, reproducible, publicly redistributable chess database containing millions of high-quality games, with full game retrieval and position-based opening statistics, within approximately 10 GB of deployed storage?**

### The Answer: **YES**

Based on real-world measurements on verified datasets (over 120,000 games from Lichess Official Broadcasts and Lichess Rated Archives):
- **10 Million Games:** Full game retrieval + Position Explorer up to Depth 40 (20 full moves) consumes **~2.82 GB**, leaving **~72% headroom** below the 10 GB target.
- **20 Million Games:** Full game retrieval + Position Explorer up to Depth 40 consumes **~5.27 GB**, providing **~47% headroom** below the 10 GB target.
- **Query Performance:** Lookups on the custom immutable index run in **sub-microsecond latency** (p50: <1.0 µs, p99: 2.0 µs) with negligible memory footprint.

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
│       └── 0001-initial-architecture.md
├── fixtures/       # Deterministic PGN test fixtures and edge-case suites
├── benchmarks/     # Automated benchmark scripts and machine-readable results
└── .github/        # GitHub Actions CI workflow
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

### 3. Run Stage B & C Benchmarks on Real Datasets
```bash
# 1. Download approved reference datasets (Lichess Broadcasts & Lichess Rated)
./scripts/download_sample_data.sh

# 2. Execute automated benchmark harness
./scripts/run_benchmarks.sh
```

Machine-readable JSON reports are written to `benchmarks/results/`.

---

## Research Deliverables

- [Source Inventory & Classification Matrix](docs/research/source-inventory.md)
- [Source Licensing & Legal Architecture](docs/research/source-licenses.md)
- [Corpus Inclusion Policies](docs/research/corpus-policy.md)
- [Deduplication & Conflict Resolution](docs/research/deduplication.md)
- [Compact Storage & Layout Options](docs/research/storage-options.md)
- [Storage Feasibility & Scaling Report](docs/research/feasibility-report.md)
- [Future API Contract & Decoupling](docs/research/api-contract.md)
- [ADR-0001: Initial Architecture Decisions](docs/adr/0001-initial-architecture.md)

---

## Licenses & Data Redistribution

- **Software:** Independently authored and dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
- **Elite Online Data:** Creative Commons CC0 1.0 Universal.
- **Masters OTB Data:** Creative Commons Attribution-ShareAlike 4.0 (CC BY-SA 4.0).
- **Zero AGPL Code Reuse:** Designed and implemented independently from first principles.
