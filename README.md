# Chess Openings DB (`codb`)

> **Status: Experimental Research & Feasibility Milestone (PR-01)**  
> This project is currently in the foundational research and prototyping phase. It is **not** a production-ready database or hosted service.

A high-performance, compact, and reproducible chess game database and position-centric indexing engine. The system curates, deduplicates, compresses, and indexes high-quality chess games to power a position-based Opening Explorer through a lightweight, reusable API.

---

## Mission & Feasibility Question

The central engineering question investigated in this repository is:

> **Can we build a reliable, reproducible, publicly redistributable chess database containing millions of high-quality games, with full game retrieval and position-based opening statistics, within approximately 10 GB of deployed storage?**

### Two Distinct Curated Collections

The database maintains two strictly isolated collections with distinct rating semantics and provenance:

1. **Masters OTB:** Over-the-board competitive tournament games, prioritizing verified classical events, high-level masters, and historical tournaments.
2. **Elite Online:** High-rated online games (e.g., Lichess standard rated games), filtering for verified high rating tiers (2200+, 2400+, 2500+), clean time controls, and non-aborted games.

> [!IMPORTANT]
> OTB FIDE ratings and online platform ratings are **never** conflated or merged. Position statistics and player ratings remain independently queryable with collection isolation.

---

## Architectural Boundaries & Relationship to Evalis

- **`chess-openings-db`** owns ingestion, curation, legal validation, compact storage, position indexing, and the future read-only query API.
- **Evalis** owns the graphical user interface, board navigation, local review integration, and private user game analysis.
- **Decoupled:** The future database service is designed to be independently deployable and usable by any third-party chess software. Evalis remains 100% functional in local-first mode without requiring a remote database connection.

---

## Workspace Architecture

```
chess-openings-db/
├── crates/
│   ├── core/       # Fundamental domain models (GameMetadata, Collection, GameResult)
│   ├── ingest/     # Streaming PGN parser (Zstandard streaming, full move legality replay)
│   ├── curate/     # Deduplication, header canonicalization, collection filtering
│   ├── storage/    # Compact binary move encoding, dictionary compression, block store
│   ├── index/      # Canonical 128-bit position keys, next-move aggregation, W/D/L stats
│   └── cli/        # CLI tool (`codb`) for benchmarking, ingestion, and verification
├── docs/
│   ├── research/   # Formal research tracks (licenses, corpus policy, dedupe, feasibility)
│   └── adr/        # Architecture Decision Records
├── fixtures/       # Deterministic PGN test fixtures and edge-case suites
├── benchmarks/     # Automated benchmark scripts and machine-readable results
└── .github/        # GitHub Actions CI workflow
```

---

## Licenses & Data Redistribution

- **Code:** Licensed under dual [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
- **Datasets:** Source datasets are segregated by explicit licensing:
  - Lichess Standard Rated Games: Creative Commons CC0.
  - Lichess Broadcasts: Creative Commons CC BY-SA 4.0.
  - Proprietary or unverified archives (such as TWIC) are strictly excluded from public distribution artifacts.
  - Detailed licensing analysis is maintained in [`docs/research/source-licenses.md`](docs/research/source-licenses.md).

---

## Quickstart

### Prerequisites
- Rust 1.80+ (tested on Rust 1.80+)
- Cargo

### Building and Testing
```bash
cargo build --release
cargo test
```
