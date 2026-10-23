# Chess Openings DB (`codb`)

> **Status: Research & Feasibility Milestone (PR-01 Complete)**  
> This repository contains the foundational research, prototypes, and empirical benchmarks establishing the technical, legal, and storage feasibility of an independent chess game database and Opening Explorer engine.

A high-performance, compact, and reproducible chess game database and position-centric indexing engine written in Rust. The system curates, deduplicates, compresses, and indexes high-quality chess games to power a position-based Opening Explorer through a lightweight, reusable API.

---

## The Central Feasibility Question

> **Can we build a reliable, reproducible, publicly redistributable chess database containing millions of high-quality games, with full game retrieval and position-based opening statistics, within approximately 10 GB of deployed storage?**

### Conclusion: **Preliminary Feasibility Supported — Full-Scale Validation Pending**

Based on reproducible measurements across 117,616 real Lichess Official Broadcast games (`22,265` accepted after curation and prefix-verified deduplication) and 121,332 Lichess Rated games:
- **Directly Measured in PR-01 ($N \le 20{,}000$ accepted games):**
  - **Compact Game Storage (`CGB3`):** **205.5–211.3 bytes/game** (`208.2 B/game` at $N = 20{,}000$, a **5.16×** compression ratio over exported PGN) with 100% round-trip fidelity across all supported canonical and provenance fields (`Date`/`UTCDate`, `GameURL`, `BroadcastURL`, `Site`, `Event`, `Round`, `ECO`, `TimeControl`, `Players`, `Elo`, `Result`, `FEN`, and legal moves).
  - **Position Indexing (`CODB_CPI` v2 vs. SQLite):** Measured directly at Depths **20, 30, 40, and 60**. Persistent `.cpi` files consume **20.26–21.57 bytes per unique position** (including sparse index directories), which is **7.2× to 9.4× smaller on disk than SQLite**.
  - **Query Latency (Warm OS Page Cache, 2,000 Stratified Queries):** On-disk `.cpi` lookups execute in **13.1–14.0 µs (p50)** and **23.7–29.0 µs (p99)** on NVMe storage, and in-memory lookups execute in **0.82–1.14 µs (p50)**, with verified semantic equivalence against SQLite.
- **Modeled Full-Scale Projections (15 Million Games Capacity Target — Option B):**
  - Slicing **Masters OTB (5M capacity ceiling) to Depth 40** and **Elite Online (10M target) to Depth 30** projects to **~6.52 GB core payload + ~0.44 GB ancillary deployment overhead = ~6.96 GB baseline** (**5.95 GB optimistic** to **8.82 GB conservative** under bounded sensitivity scenarios), remaining below the 10 GB ceiling.
  - **Explicit Caveats for PR-02:** Projections from $N = 20{,}000$ to 5M–10M games represent a $250\times$–$500\times$ extrapolation; Elite Online projections currently use Masters OTB scaling parameters as an unvalidated proxy (as the tested `2013-01` Lichess archive had `0` accepted games under `EliteOnlinePolicy`); and the initial legally approved Masters OTB release from Lichess Broadcasts + Public Domain archives will be ~300k–500k curated games (< 0.45 GB), with 5M serving as a long-term architectural capacity target.

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

### 3. Run Stage B & Progressive Scaling Benchmarks
```bash
# Execute automated reproduction harness (Stage A, Stage B, and Progressive Masters OTB 1k-20k series)
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

---

## Licenses & Data Redistribution

- **Software:** Independently authored and dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
- **Elite Online Data:** Creative Commons CC0 1.0 Universal.
- **Masters OTB Data:** Creative Commons Attribution-ShareAlike 4.0 (CC BY-SA 4.0).
- **Zero AGPL Code Reuse:** Designed and implemented independently from first principles.
