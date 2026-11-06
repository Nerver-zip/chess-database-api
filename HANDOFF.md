# Agent Handoff (`chess-openings-db`)

## 1. Current Repository State

- **Goal:** PR-04: Production-Ready Opening Explorer HTTP API Service.
- **Baseline Git Status:** Complete Rust workspace with 7 crates (`core`, `ingest`, `curate`, `storage`, `index`, `api`, `cli`). 100% test pass rate across unit, integration, publication crash simulations, HTTP API contract/concurrency suites, and Python regression test suites.
- **Architecture Status:** Aligned with `ADR-0001` (Feasibility Foundations), `ADR-0002` (Bounded Streaming Ingestion & CGB3 Persistent Shards), `ADR-0003` (Production CPI v2 Index with Bounded External Merge Sort), and `ADR-0004` (Opening Explorer Read-Only HTTP API Service).
- **Format Integrity:**
  - `CGB3` (`shard_XXXX.cgb3`): 100% roundtrip fidelity, dictionary string interning, and physical density (~208 B/game for Masters OTB, ~178–181 B/game for Elite Online).
  - `games.idx`: 16-byte fixed-record memory-mapped global index providing sub-millisecond random game retrieval by 64-bit stable ID.
  - `CPI v2` (`positions.cpi`): 64-byte header, 34-byte memory-mapped sparse index ($O(\log B)$ binary search), and Zstd-compressed position blocks with candidate moves and W/D/L counters.
- **HTTP Serving Layer (`codb-api` & `codb serve`):**
  - High-performance read-only HTTP service built with Axum 0.8, Tokio, and Tower.
  - Snapshot isolation pinning immutable version directories (`versions/vX/`) with zero lock contention.
  - Lock-free, zero-copy position lookups directly against memory-mapped CPI indexes.
  - Bounded RAII reader pool for CGB3 game decompression offloaded to Tokio's blocking threadpool.
  - RFC 7232 HTTP conditional request validation (`ETag`, `If-None-Match`, `304 Not Modified`) and `Cache-Control` headers.
  - CORS headers exposing `ETag` and permitting `If-None-Match` for web clients.
  - Zero-downtime hot-reloading (`POST /v1/admin/reload`) executing in under 1 ms under live traffic.
- **Benchmark Baseline:**
  - Active production CPI baseline: 16 parameter combination runs in `benchmarks/production_cpi/` validated by `scripts/validate_production_cpi_benchmarks.py`.
  - HTTP API benchmark: `benchmarks/results/http_api_benchmark.json` validated by `scripts/test_bench_http.py`:
    - Startup time: **52.65 ms**
    - Memory envelope: **5.57 MB** initial RSS, peaking at **8.49 MB** under full concurrent load (< 30 MB ceiling).
    - Query latency: Single-client p50 = **227–250 µs**; Game retrieval p50 = **265 µs**.
    - Concurrency throughput: **> 3,000 req/s** with zero errors across 50 concurrent workers.
    - Zero-downtime reload: **0.84 ms** reload duration, **805 live requests handled during swap**, **0 errors**.

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
│   ├── api/        # Production-ready read-only HTTP Opening Explorer service (Axum & Tokio)
│   └── cli/        # CLI interface (codb), daemon server (codb serve), benchmarking, and verification
├── docs/
│   ├── README.md   # Master documentation navigation index
│   ├── ARCHITECTURE.md # End-to-end architecture, sharding, concurrency, and memory bounds
│   ├── DATA_CONTRACTS.md # Binary formats (CGB3, CPI), models, CLI JSON, and HTTP API contract
│   ├── CORPUS_AND_DEDUPLICATION.md # Collection inclusion rules, bullet exclusion, dedupe rules, accounting
│   ├── LICENSING.md # Dual-licensing (MIT/Apache-2.0), source data provenance, zero-AGPL boundary
│   ├── QA_STRATEGY.md # Storage crash recovery, reader snapshot isolation, verification gates
│   ├── BENCHMARKS.md # Benchmark methodology, production CPI baselines, dataset checksums, Heaps' Law
│   ├── ROADMAP.md  # Milestone progress and planned Opening Explorer HTTP API service
│   ├── DECISIONS_AND_OPEN_QUESTIONS.md # Accepted trade-offs and open architectural questions
│   ├── adr/        # Architecture Decision Records (ADR-0001 to ADR-0004)
│   └── research/   # Historical research tracks preserved for provenance and reproducibility
├── benchmarks/
│   ├── README.md   # Architecture of production baselines vs historical results
│   ├── production_cpi/ # 16 active production CPI baseline runs (ADR-0003)
│   └── results/    # Active benchmark outputs (http_api_benchmark.json) and historical archive
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

# Start Opening Explorer HTTP API daemon
codb serve --masters-dataset <dir> --elite-dataset <dir> --host 127.0.0.1 --port 8080

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
- `cargo test --workspace`: PASS (all unit, integration, publication crash simulations, and 12 API contract/concurrency tests pass).
- Python test suites (`scripts/test_*.py`): PASS (21/21 in feasibility/validation, 4/4 in reproduce runner, 5/5 in bench_http).
- Production CPI validator (`scripts/validate_production_cpi_benchmarks.py`): PASS (all 16 artifacts valid).
- Historical validator (`scripts/validate_benchmarks.py`): PASS (all 18 artifacts and Heaps' projections valid).
- Documentation validator (`scripts/validate_docs.py`): PASS (all internal links, uppercase basenames, and frontmatter valid).
- Contract manifest verification (`sha256sum -c MANIFEST.sha256`): PASS.

---

## 5. Upstream Pull Request & Next Actions

1. **Pull Request Status:** Draft Pull Request [#5](https://github.com/Nerver-zip/chess-openings-db/pull/5) submitted and verified.
   - Branch: `feat/opening-explorer-http-api` -> `main`.
   - CI Status: 100% Green on GitHub Actions (Run ID `42256269806`, all 8 validation gates and reproduction runner passed).
2. **Review & Promotion:** Transition PR #5 from Draft to Ready for Review upon stakeholder approval.
3. **Future Incremental Initiatives:** Evaluate future roadmap enhancements (incremental delta shards, precomputed ECO classification trees, and edge cache optimization).
