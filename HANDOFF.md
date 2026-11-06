# Agent Handoff (`chess-openings-db`)

## 1. Current Repository State

- **Goal:** Padronização de Governança, Consolidação de Documentação e Alinhamento de Workflows (`chess-openings-db`).
- **Baseline Git Status:** Complete Rust workspace with 6 crates (`core`, `ingest`, `curate`, `storage`, `index`, `cli`). 100% test pass rate across unit, integration, publication crash simulations, and Python regression test suites.
- **Architecture Status:** Aligned with `ADR-0001` (Feasibility Foundations), `ADR-0002` (Bounded Streaming Ingestion & CGB3 Persistent Shards), and `ADR-0003` (Production CPI v2 Index with Bounded External Merge Sort).
- **Format Integrity:**
  - `CGB3` (`shard_XXXX.cgb3`): 100% roundtrip fidelity, dictionary string interning, and physical density (~208 B/game for Masters OTB, ~178–181 B/game for Elite Online).
  - `games.idx`: 16-byte fixed-record memory-mapped global index providing sub-millisecond random game retrieval by 64-bit stable ID.
  - `CPI v2` (`positions.cpi`): 64-byte header, 34-byte memory-mapped sparse index ($O(\log B)$ binary search), and Zstd-compressed position blocks with candidate moves and W/D/L counters.
- **Benchmark Baseline:**
  - Active production CPI baseline: 16 parameter combination runs in `benchmarks/production_cpi/` validated by `scripts/validate_production_cpi_benchmarks.py`.
  - Historical developmental stages: Stage A fixture validation, Stage B single-month baselines, PR-01 progressive scaling (1k–20k), PR-02 100k scale exploration, and fitted Heaps' Law model (`projections_heaps.json`) organized under `benchmarks/results/historical/`.

---

## 2. Workspace & Crate Structure

```
chess-openings-db/
├── crates/
│   ├── core/       # Domain models (CompactMove, PositionKey, GameResult, StableGameId)
│   ├── ingest/     # PGN streaming parser, legal move validation (shakmaty), time control filtering
│   ├── curate/     # Masters/Elite collection policies, prefix-superseding deduplication
│   ├── storage/    # CGB3 container, sharding, games.idx binary search, advisory flock, atomic publication
│   ├── index/      # External merge sort (ExternalPositionIndexer, .crun runs), CPI v2 writer, Explorer queries
│   └── cli/        # CLI interface (codb), benchmarking, and verification tools
├── docs/
│   ├── README.md   # Master documentation navigation index
│   ├── ARCHITECTURE.md # End-to-end architecture, sharding, concurrency, and memory bounds
│   ├── DATA_CONTRACTS.md # Binary formats (CGB3, CPI), models, CLI JSON, and future HTTP API contract
│   ├── CORPUS_AND_DEDUPLICATION.md # Collection inclusion rules, bullet exclusion, dedupe rules, accounting
│   ├── LICENSING.md # Dual-licensing (MIT/Apache-2.0), source data provenance, zero-AGPL boundary
│   ├── QA_STRATEGY.md # Storage crash recovery, reader snapshot isolation, verification gates
│   ├── BENCHMARKS.md # Benchmark methodology, production CPI baselines, dataset checksums, Heaps' Law
│   ├── ROADMAP.md  # Milestone progress and planned Opening Explorer HTTP API service
│   ├── DECISIONS_AND_OPEN_QUESTIONS.md # Accepted trade-offs and open architectural questions for API
│   ├── adr/        # Architecture Decision Records (ADR-0001, ADR-0002, ADR-0003)
│   └── research/   # Historical research tracks preserved for provenance and reproducibility
├── benchmarks/
│   ├── README.md   # Architecture of production baselines vs historical results
│   ├── production_cpi/ # 16 active production CPI baseline runs (ADR-0003)
│   └── results/
│       └── historical/ # Preserved PR-01, PR-02, Stage A/B, and projection artifacts
├── fixtures/       # Deterministic PGN test fixtures (stage_a_validation.pgn)
├── scripts/        # Unified validate.sh, validate_docs.py, benchmark runners, and tests
├── AGENTS.md       # Operational guide for AI agents
├── CONTRIBUTING.md # Contribution guidelines, PR conventions, and quality gates
├── CHANGELOG.md    # Milestone delivery history
├── MANIFEST.sha256 # Cryptographic manifest of core contracts and schemas
└── README.md       # Root repository landing page
```

---

## 3. CLI Commands Reference (`codb`)

```bash
# Build persistent CGB3 dataset from PGN / PGN.zst
codb build --input <file.pgn.zst> --collection <masters-otb|elite-online> --output <dir> [--max-games N]

# Build production CPI position index using bounded-memory external sort
codb build-index --dataset <dir> --depth-plies <N> --collection <masters-otb|elite-online> [--memory-limit-mib 128]

# Query Opening Explorer by move sequence or FEN
codb query --dataset <dir> --moves "e4,e5,Nf3" --json
codb query --dataset <dir> --fen "<fen_string>" --json

# Verify dataset integrity (block checksums, games.idx, manifest accounting, and CPI index)
codb verify --dataset <dir> [--deep]

# Inspect dataset metrics (shards, compression density, CPI statistics)
codb inspect --dataset <dir> [--json]

# Retrieve individual game by 64-bit stable ID
codb get-game --dataset <dir> --id <u64_id>

# Run benchmark suite and emit machine-readable JSON report
codb bench --input <file.pgn> --collection <masters-otb|elite-online> --output-dir <dir> --run-name <name>
```

---

## 4. Verification State

All local quality gates pass with zero warnings and zero errors:
- `cargo fmt --all -- --check`: PASS.
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS.
- `cargo test --workspace`: PASS (all unit, integration, and 40 crash-recovery publication tests pass).
- Python test suites (`scripts/test_*.py`): PASS (21/21 tests in `test_feasibility_alignment.py` and `test_validate_benchmarks.py`, 4/4 in `test_reproduce_runner.py`).
- Production CPI validator (`scripts/validate_production_cpi_benchmarks.py`): PASS (all 16 artifacts valid).
- Historical validator (`scripts/validate_benchmarks.py`): PASS (all 18 artifacts and Heaps' projections valid).
- Documentation validator (`scripts/validate_docs.py`): PASS (all internal links, uppercase basenames, and frontmatter valid).
- Contract manifest verification (`sha256sum -c MANIFEST.sha256`): PASS.

---

## 5. Next Steps for Future API Service

1. **API Crate Scaffolding:** Create `crates/api` (or standalone daemon binary) using `axum`.
2. **Zero-Copy Service Layer:** Expose `/v1/explorer/position` and `/v1/explorer/game/{id}` using memory-mapped zero-copy queries against active `positions.cpi` and `games.idx`.
3. **Hot-Reloading & Inotify Watcher:** Implement graceful version hot-swapping when the dataset `current` symlink transitions.
4. **Client Application Integration:** Connect desktop and web client applications to the local or remote Opening Explorer daemon as specified in [docs/DATA_CONTRACTS.md](docs/DATA_CONTRACTS.md#6-future-http-opening-explorer-api-contract).
