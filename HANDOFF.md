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
  - Semaphore-bounded reader pool (`Arc<tokio::sync::Semaphore>`) for CGB3 game decompression offloaded to Tokio's blocking threadpool, strictly capping active readers to configured capacity with asynchronous queue timeouts.
  - Fail-closed candidate snapshot validation before activation: candidate replacement directories are deeply verified (`verify_dataset` + CPI verification) before atomic pointer replacement, safely rejecting corrupted candidates while preserving active snapshot operation.
  - RFC 7232 HTTP conditional request validation: game existence in `games.idx` is verified prior to evaluating conditional headers (returning 404 for missing games, never 304). Supports strong ETags, weak ETags (`W/"..."`), wildcards (`*`), and comma-separated tokens.
  - Secure administrative management: `POST /v1/admin/reload` requires token authentication (`--admin-token` / `CODB_ADMIN_TOKEN`), returns 403 Forbidden when disabled and 401 Unauthorized for missing/invalid tokens, and rejects cross-origin requests. Local administrators can also trigger reloads without network credentials via standard Unix `SIGHUP` (`kill -HUP <pid>`).
  - Restrictive default CORS policy (empty by default, same-origin only), with explicit origins configurable via `--allowed-origins`.
- **Benchmark Baseline:**
  - Active production CPI baseline: 16 parameter combination runs in `benchmarks/production_cpi/` validated by `scripts/validate_production_cpi_benchmarks.py`.
  - HTTP API benchmark: `benchmarks/results/http_api_benchmark.json` validated by `scripts/test_bench_http.py`:
    - Corpus accounting: Tested directly on fast validation fixture (`fixtures/stage_a_validation.pgn`, 1,000 games); documents that PR-03 production 100k datasets require local provisioning.
    - Startup time: **58.54 ms**
    - Memory envelope: **6.18 MB** initial RSS, continuous sampled peak RSS at **9.86 MB** under full concurrent load (< 30 MB ceiling).
    - Query latency: Single-client p50 = **264–361 µs**; Game retrieval p50 = **287 µs**.
    - Concurrency throughput: **4,587 req/s** peak, scaling across 1, 4, 8, 16, and 32 concurrent workers with zero errors.
    - Reader pool saturation contention: 32 concurrent workers requesting against pool capacity 4 queue safely with zero errors.
    - Zero-downtime reload: **3.29 ms** reload duration, **715 live requests handled during swap**, **0 errors**.

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
codb serve --masters-dataset <dir> --elite-dataset <dir> --host 127.0.0.1 --port 8080 [--reader-pool-size 4] [--reader-pool-timeout-secs 5] [--admin-token <token>]

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

## 4. Final Hardening Follow-up (Merge Readiness)

The following narrowly scoped hardening improvements were implemented and verified on PR #5:
1. **Empty Administrative Token Rejection (P2):**
   - `ServerConfig::validate()` strictly rejects empty or whitespace-only `--admin-token` values at startup, ensuring misconfiguration fails immediately rather than allowing authentication with an empty bearer token.
   - Added defensive checks in `routes/admin.rs` (`!token.is_empty() && token == expected_token`).
   - Covered by comprehensive regression tests for missing, empty, whitespace-only, invalid, and valid tokens.
2. **Internal HTTP Error Sanitization (P2):**
   - HTTP 500 responses (`reload_failed`, `storage_error`, `internal_error`) log complete diagnostic details to structured server-side logs via `tracing::error!` without exposing secrets.
   - Response JSON payloads return sanitized generic messages that never expose filesystem paths, storage internals, or low-level diagnostic traces.
   - Stable machine-readable error codes are strictly preserved.
   - Regression tests verify that simulated internal errors with paths and corrupt snapshot errors never leak paths into JSON responses.
3. **Representation-Accurate Position ETags (P3):**
   - `compute_position_etag` incorporates collection identity, dataset build ID, canonical 128-bit position key, and the complete FEN string (including move clock counters) using SHA-256 digest suffixing.
   - Eliminates ETag collisions between differing collections and between equivalent board positions reached via move repetitions that alter FEN halfmove/fullmove clocks.
   - Preserves RFC 7232 conditional caching (`304 Not Modified`) for identical representations across identical transpositions.
4. **Exposed Reader-Pool Timeout Option (P3):**
   - Added `--reader-pool-timeout-secs` to `codb serve` CLI options with environment variable `CODB_READER_POOL_TIMEOUT_SECS` (default: 5 seconds).
   - Validated in `ServerConfig::validate()` that `reader_pool_timeout_secs > 0`.
   - Updated CLI documentation, `--help`, and added CLI option parsing unit tests.

---

## 5. Verification State

All local quality gates pass with zero warnings and zero errors:
- `cargo fmt --all -- --check`: PASS.
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS.
- `cargo test --workspace`: PASS (all unit, integration, publication crash simulations, and 22 API contract, security, and concurrency tests pass).
- Python test suites (`scripts/test_*.py`): PASS (21/21 in feasibility/validation, 4/4 in reproduce runner, 5/5 in bench_http).
- Production CPI validator (`scripts/validate_production_cpi_benchmarks.py`): PASS (all 16 artifacts valid).
- Historical validator (`scripts/validate_benchmarks.py`): PASS (all 18 artifacts and Heaps' projections valid).
- Documentation validator (`scripts/validate_docs.py`): PASS (all internal links, uppercase basenames, and frontmatter valid).
- Contract manifest verification (`sha256sum -c MANIFEST.sha256`): PASS.

---

## 6. Upstream Pull Request & Next Actions

1. **Pull Request Status:** Draft Pull Request [#5](https://github.com/Nerver-zip/chess-openings-db/pull/5) updated and hardened.
   - Branch: `feat/opening-explorer-http-api` -> `main`.
   - Final status recommendation: **READY FOR MERGE**.
2. **Review & Promotion:** Transition PR #5 from Draft to Ready for Review upon stakeholder approval.
3. **Future Incremental Initiatives:** Evaluate future roadmap enhancements (incremental delta shards, precomputed ECO classification trees, and edge cache optimization).
