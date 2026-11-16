# Chess Openings DB (`codb`)

> **Status: Draft PR #7 — local Gigabase qualification evaluation; not approved for release or production use**
> This repository contains the scalable persistent game storage pipeline, bounded-memory CPI v2 indexer, atomic dataset publication engine, verified real corpus releases, and a Dockerized read-only Opening Explorer API.

A high-performance, compact, and reproducible chess game database and position-centric indexing engine written in Rust. The system curates, deduplicates, compresses, and indexes high-quality chess games to power a position-based Opening Explorer through a lightweight, reusable API.

---

## Delivered Corpus and Scope

The real-corpus pipeline delivers two isolated release collections and a read-only HTTP Opening Explorer. Release sizes, current game and index counts, licensing, provenance audit results, and test evidence are recorded in [docs/CORPUS_BUILD.md](docs/CORPUS_BUILD.md).

Current verified releases contain 29,640 event-qualified Masters OTB games (705,718 positions at depth 40) and 429,150 Elite Online games (6,136,307 positions at depth 30). The real Docker integration served both collections simultaneously; `e4,e5,Nf3` returned 3,902 Masters games and 30,390 Elite games.

The historical 15-million-game storage feasibility work remains in [docs/research/feasibility-report.md](docs/research/feasibility-report.md). Expanding to that future corpus is not required to run or validate the Docker service.

For historical capacity context, the prior 15-million-game estimate projected **7.68 GB baseline** (**6.44 GB optimistic**, **9.98 GB conservative**). Its conservative case left **20 MB (0.2%)** under the 10 GB limit, so that estimate was conditional; it is separate from the current release sizes.

---

## Two Distinct Curated Collections

The database maintains two strictly isolated collections with distinct rating semantics and provenance:

1. **Masters OTB:** A conservative subset of high-rated Lichess broadcast games. Corpus releases accept only exact event identifiers classified as verified OTB by the event-level metadata and linked-source audit; unresolved and verified online events are excluded. The source data is CC BY-SA 4.0.
2. **Elite Online:** High-rated online games filtering for verified high rating tiers (2200+, 2400+, 2500+), clean time controls (classical/rapid/blitz; bullet strictly excluded), and non-aborted games (sourced from Lichess Rated Archives under CC0 1.0).

> [!IMPORTANT]
> OTB FIDE ratings and online platform ratings are **never** conflated or merged. Position statistics and player ratings remain independently queryable with collection isolation.

---

## Service Boundary

The CODB service owns corpus ingestion, curation, legal validation, compact storage, position indexing, and read-only HTTP queries. Docker Compose runs the API independently from the corpus releases, which are mounted as read-only host volumes. Other containers can reach it on a shared Docker network at http://codb:8080; no host port is published by default.

---

## Workspace Architecture

```
chess-openings-db/
├── AGENTS.md       # Operational guide for AI agents
├── HANDOFF.md      # Workstream status, verification log, and next actions
├── CONTRIBUTING.md # Contribution workflows and PR expectations
├── CHANGELOG.md    # Release and milestone delivery log
├── MANIFEST.sha256 # Cryptographic manifest of core contracts and schemas
├── Cargo.toml      # Workspace root configuration
├── crates/
│   ├── core/       # Fundamental domain models (CompactMove, PositionKey, GameResult)
│   ├── ingest/     # Streaming PGN parser (Zstandard streaming, full legal move replay)
│   ├── curate/     # Policy filters, header canonicalization, prefix deduplication
│   ├── storage/    # CGB3 container, sharding, games.idx binary search, advisory flock
│   ├── index/      # CPI v2 index, bounded external sorting, Opening Explorer queries
│   ├── api/        # Production-ready read-only HTTP Opening Explorer service (Axum & Tokio)
│   └── cli/        # CLI tool (codb) for builds, indexing, queries, serving, and verification
├── docs/
│   ├── README.md   # Canonical documentation index
│   ├── ARCHITECTURE.md # End-to-end architecture, sharding, and memory bounds
│   ├── DOCKER_DEPLOYMENT.md # Real release provisioning and Compose operations
│   ├── DATA_CONTRACTS.md # Binary formats (CGB3, CPI), models, and API schemas
│   ├── CORPUS_AND_DEDUPLICATION.md # Collection policies, dedupe, and accounting
│   ├── LICENSING.md # Dual-licensing, dataset provenance, zero-AGPL boundary
│   ├── QA_STRATEGY.md # Storage crash recovery, snapshot isolation, test strategy
│   ├── BENCHMARKS.md # Benchmark methodology, production CPI baselines, Heaps' Law
│   ├── ROADMAP.md  # Milestone progress and planned API service
│   ├── DECISIONS_AND_OPEN_QUESTIONS.md # Accepted decisions and API open questions
│   ├── adr/        # Architecture Decision Records (ADR-0001, ADR-0002, ADR-0003)
│   └── research/   # Historical research tracks preserved for provenance
├── benchmarks/
│   ├── README.md   # Benchmark directory overview and methodology
│   ├── production_cpi/ # Active production CPI baseline measurements (ADR-0003)
│   └── results/
│       └── historical/ # Preserved PR-01, PR-02, Stage A/B, and projection artifacts
├── fixtures/       # Deterministic PGN test fixtures (stage_a_validation.pgn)
├── scripts/        # Unified validate.sh, validate_docs.py, runners, and tests
└── .github/        # GitHub Actions CI workflow
```

---

## Build and Run the Real Service

Build the host CLI used to provision and verify releases. The explicit rebuild command downloads the pinned sources (about 8.66 GB), then builds, indexes, and deeply verifies both collections. A package bootstrap is also available when verified release archives and their .sha256 sidecars are already present.

```bash
cargo build --locked --release --package codb-cli
./scripts/corpus.sh plan --collection all
./scripts/corpus.sh --data-dir ./data bootstrap --rebuild
./scripts/corpus.sh --data-dir ./data verify
docker compose -f compose.codb.yaml up -d --build
docker compose -f compose.codb.yaml ps
```

Compose never downloads or rebuilds datasets. The API listens as service codb on port 8080 inside its Docker network and publishes no host port by default. See [docs/DOCKER_DEPLOYMENT.md](docs/DOCKER_DEPLOYMENT.md) for package bootstrap, debug queries, cross-project networking, safe updates, resource bounds, and troubleshooting. The complete corpus procedure and release evidence are in [docs/CORPUS_BUILD.md](docs/CORPUS_BUILD.md).

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

### 2. Build Production Position Index (CPI)
Construct a compressed, randomly accessible position index from a completed dataset using bounded-memory external sorting:
```bash
./target/release/codb build-index \
    --dataset data/builds/masters-otb-20k \
    --depth-plies 40 \
    --collection masters-otb \
    --memory-limit-mib 128 \
    --json
```

### 3. Query Opening Explorer
Query opening statistics and candidate next moves by move sequence or FEN:
```bash
# Query by move sequence (SAN or UCI, comma or space delimited):
./target/release/codb query \
    --dataset data/builds/masters-otb-20k \
    --moves "e4,e5,Nf3" \
    --json

# Query by orthodox legal FEN:
./target/release/codb query \
    --dataset data/builds/masters-otb-20k \
    --fen "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6 0 2"
```

### 4. Verify Dataset Integrity
Verify block checksums, manifest accounting, game lookup table, and CPI position index:
```bash
./target/release/codb verify --dataset data/builds/masters-otb-20k --deep
```

### 5. Inspect Dataset Structure
Inspect shard layout, total retained games, CPI index metadata, and physical byte density:
```bash
./target/release/codb inspect --dataset data/builds/masters-otb-20k --json
```

### 6. Retrieve Game by Stable ID
Random access lookup in sub-millisecond page-cache time:
```bash
./target/release/codb get-game --dataset data/builds/masters-otb-20k --id 1234567890
```

### 7. Run Opening Explorer HTTP Server (`codb serve`)
Start the high-performance, asynchronous, read-only HTTP query service exposing `/v1/explorer/position`, `/v1/collections/{collection}/games/{id}`, `/v1/collections`, `/health`, and `/ready`:
```bash
# Start server mounting datasets with bounded reader pool and admin reload token:
./target/release/codb serve \
    --masters-dataset data/builds/masters-otb-100k \
    --elite-dataset data/builds/elite-online-100k \
    --host 127.0.0.1 \
    --port 8080 \
    --reader-pool-size 4 \
    --reader-pool-timeout-secs 5 \
    --admin-token "your-admin-secret"

# Query opening statistics by SAN move sequence:
curl -s "http://127.0.0.1:8080/v1/explorer/position?moves=e4,e5,Nf3&collection=masters-otb" | jq .

# Query by orthodox legal FEN:
curl -s "http://127.0.0.1:8080/v1/explorer/position?fen=rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR%20b%20KQkq%20-%200%201" | jq .

# Retrieve canonical game in PGN format:
curl -s "http://127.0.0.1:8080/v1/collections/masters-otb/games/1048220"

# Inspect loaded collections and dataset versions:
curl -s "http://127.0.0.1:8080/v1/collections" | jq .

# Liveness and readiness health checks:
curl -s "http://127.0.0.1:8080/health"
curl -s "http://127.0.0.1:8080/ready"

# Trigger zero-downtime hot-reload after publishing a new dataset version:
curl -s -X POST "http://127.0.0.1:8080/v1/admin/reload" \
    -H "Authorization: Bearer your-admin-secret" | jq .

# Or trigger zero-downtime reload locally via standard Unix SIGHUP:
kill -HUP <pid>
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

For complete step-by-step reproduction instructions, dataset checksums, and combined corpus assembly, see the [Benchmark Reproduction Guide](docs/BENCHMARKS.md).

Machine-readable JSON reports are written to `benchmarks/results/` (with completed milestones archived in [`benchmarks/results/historical/`](benchmarks/results/historical/)).

### Local Gigabase evaluation

The opt-in `--gigabase-source-policy` path and `corpus.sh extract-local-7z` support local evaluation of a user-supplied Lumbra's Gigabase archive. The current r3 evaluation retained 3,284,429 unique games from 13,450,696 encountered records; its full-source analysis and reproducibility evidence are in [`benchmarks/gigabase/FINAL_REPORT.md`](benchmarks/gigabase/FINAL_REPORT.md). Local SHA-256 values are not publisher checksums, the source carries publisher-stated CC BY-NC 4.0 terms with unresolved upstream rights, and this path does not create an approved or publicly distributable corpus release. See [Corpus Build](docs/CORPUS_BUILD.md), [source qualification policy](docs/CORPUS_AND_DEDUPLICATION.md), and [licensing limits](docs/LICENSING.md).

---

## Canonical Engineering Documentation

- **Master Index:** [Documentation Index](docs/README.md)
- **System Architecture:** [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- **Data Contracts & Formats:** [docs/DATA_CONTRACTS.md](docs/DATA_CONTRACTS.md)
- **Corpus Build & Release Procedure:** [docs/CORPUS_BUILD.md](docs/CORPUS_BUILD.md)
- **Corpus & Deduplication Policy:** [docs/CORPUS_AND_DEDUPLICATION.md](docs/CORPUS_AND_DEDUPLICATION.md)
- **Licensing & Data Provenance:** [docs/LICENSING.md](docs/LICENSING.md)
- **QA & Testing Strategy:** [docs/QA_STRATEGY.md](docs/QA_STRATEGY.md)
- **Benchmarks & Scaling Methodology:** [docs/BENCHMARKS.md](docs/BENCHMARKS.md)
- **Project Roadmap & Milestones:** [docs/ROADMAP.md](docs/ROADMAP.md)
- **Decisions & Open Questions:** [docs/DECISIONS_AND_OPEN_QUESTIONS.md](docs/DECISIONS_AND_OPEN_QUESTIONS.md)
- **Architecture Decision Records (ADRs):** [docs/adr/README.md](docs/adr/README.md)
  - [ADR-0001: Initial Architecture Decisions](docs/adr/0001-initial-architecture.md)
  - [ADR-0002: Bounded-Memory Ingestion, CGB3 Sharding & Global Game Lookup](docs/adr/0002-bounded-ingestion-and-persistent-cgb3-archive.md)
  - [ADR-0003: Production CPI Index, Bounded-Memory External Sorting & Explorer Queries](docs/adr/0003-production-cpi-index.md)
  - [ADR-0004: Opening Explorer Read-Only HTTP API Service](docs/adr/0004-opening-explorer-http-api.md)
- **Governance & Contributor Guides:**
  - [Agent Development Guide](AGENTS.md)
  - [Handoff Log](HANDOFF.md)
  - [Contributing Guidelines](CONTRIBUTING.md)
  - [Release Changelog](CHANGELOG.md)

---

## Historical Research Tracks

Initial exploratory research deliverables preserved for historical reference and reproducibility:
- [Storage Feasibility & Scaling Report](docs/research/feasibility-report.md)
- [Compact Storage & Layout Options](docs/research/storage-options.md)
- [Source Inventory & Classification Matrix](docs/research/source-inventory.md)
- [Source Licensing & Legal Architecture](docs/research/source-licenses.md)
- [Corpus Inclusion Policies (Draft)](docs/research/corpus-policy.md)
- [Deduplication & Conflict Resolution (Draft)](docs/research/deduplication.md)
- [Benchmark Reproduction Guide (Draft)](docs/research/benchmark-reproduction.md)
- [Future API Contract & Decoupling (Draft)](docs/research/api-contract.md)

---

## Licenses & Data Redistribution

- **Software:** Independently authored and dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
- **Elite Online Data:** Creative Commons CC0 1.0 Universal.
- **Masters OTB Data:** Creative Commons Attribution-ShareAlike 4.0 (CC BY-SA 4.0).
- **Zero AGPL Code Reuse:** Designed and implemented independently from first principles.
