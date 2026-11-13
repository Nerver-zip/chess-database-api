# Changelog

All notable changes to `chess-openings-db` are documented in this file. The project adheres to [Semantic Versioning](https://semver.org/).

---

## [Unreleased]

### Added
- **Reproducible corpus release workflow (PR-A):**
  - `corpus/catalog.json`: pinned source catalog (publisher, URL, archive period, license, expected size, publisher SHA-256, verified SHA-256, acquisition timestamp, provenance confidence, eligibility, notes) for the Lichess broadcast (CC BY-SA 4.0) and standard rated (CC0 1.0) archives.
  - `scripts/corpus.py` / `scripts/corpus.sh`: `plan`, `sources`, `fetch`, `verify-sources`, `pin`, `build`, `verify`, `report`, `package`, `verify-package`, `serve`, `http-check`. Resumable HTTPS downloads with retry/backoff, publisher-checksum verification, atomic finalization, disk-space and size safety limits, offline verification; builds in a private workspace with deep verification before atomic promotion; deterministic release packaging.
  - `codb build --report-json` and `codb report` (machine-readable accounting, rejection reasons, and distributions).
  - Release provenance (`RELEASE/RELEASE.json`) and legal notice (`RELEASE/DATASET_LICENSE.txt`) bundled with every release.
  - `codb bench-lookup` (in-process CPI lookup latency on a published release) and a multi-process HTTP load client in `http-check`.
  - `docs/CORPUS_BUILD.md`, offline fixtures (`fixtures/corpus/`), and `scripts/test_corpus.py`.

### Changed
- **Masters OTB policy (`masters-otb/2026.11-r1`):** records whose event, site, or broadcast URL carries an online-event marker are rejected (`OnlineEventInMastersSource`); a pre-1970 date is accepted only together with a named event and venue (`HistoricalWithoutProvenance`). No binary format changed.
- **Elite Online policy (`elite-online/2026.11-r1`):** casual-labelled events are rejected (`CasualUnratedGame`).
- Policy rejections are reported by stable category (`PolicyRejectionReason::category`).

### Fixed
- Deduplication SQLite cache lowered from 32 MB to 2 MB: byte-identical output, `codb build` peak RSS 29.8 MB (Masters) / 36.9 MB (Elite, 429k games) instead of 44 / 67 MB.
- `docs/DATA_CONTRACTS.md` documented HTTP 422 for unsupported query parameters; the implementation (and the error table) return HTTP 400 `unsupported_parameter`.

---

## [0.5.0] - 2026-11-06

### Added
- **Opening Explorer HTTP Service (`codb-api`):**
  - Implemented production-ready, read-only HTTP service using Axum 0.8, Tokio, and Tower.
  - Position query endpoint `GET /v1/explorer/position`:
    - Orthodox legal FEN parsing or comma/whitespace-separated move sequences.
    - Zero-copy, lock-free queries directly against memory-mapped CPI v2 indexes.
    - Full candidate move statistics (UCI, SAN, white wins, draws, black wins, total games, and win percentages calculated from white's perspective).
    - Returns HTTP 200 with zero counts and empty moves array for unencountered legal positions.
  - Full game retrieval endpoint `GET /v1/collections/{collection}/games/{id}`:
    - Collection-qualified lookups by 64-bit stable ID via `games.idx` memory-mapped binary search table.
    - Offloaded Zstd CGB3 block decompression to Tokio blocking threadpool via bounded RAII `DatasetReaderPool`.
    - Returns canonical PGN payload (`application/x-chess-pgn`).
  - Collections metadata endpoint `GET /v1/collections` returning loaded collections, snapshot versions, total games, and CPI index status.
  - Operational health probes: `GET /health` (liveness) and `GET /ready` (readiness).
  - Administrative reload endpoint `POST /v1/admin/reload`: zero-downtime hot reloading of dataset snapshots with authentication token protection (`--admin-token`), cross-origin request rejection, and native Unix `SIGHUP` signal handling.
  - Fail-closed snapshot replacement validation: candidate snapshots are deeply verified before activation, rejecting corrupted versions with HTTP 500 (`reload_failed`) while preserving active snapshot serving.
  - Semaphore-bounded reader pool (`DatasetReaderPool`): active readers are strictly bounded by configured capacity via `tokio::sync::Semaphore` with asynchronous timeouts.
  - RFC 7232 HTTP conditional caching: game existence verified before evaluating `If-None-Match` (returning 404 for missing resources, never 304), with support for weak ETags, wildcards, and comma-separated lists.
  - Restrictive default CORS policy (same-origin only) with configurable explicit origins.
  - Strict input validation rejecting mutually exclusive parameters (`fen` and `moves`), unsupported parameters (`since_year`, `min_rating` with HTTP 422), and move sequences exceeding ply limits.
  - Standard JSON error envelope with machine-readable error codes (`invalid_collection`, `invalid_fen`, `invalid_moves`, `moves_too_long`, `unsupported_parameter`, `game_not_found`, etc.).
  - Production middleware stack: CORS headers, request timeout (`15s`), and structured tracing.
- **Unified CLI Integration (`codb serve`):**
  - Added `serve` subcommand to `codb-cli` with flags for `--masters-dataset`, `--elite-dataset`, `--host`, `--port`, `--allowed-origins`, `--request-timeout-secs`, `--max-move-plies`, and `--reader-pool-size`.
  - Graceful shutdown handling SIGINT and SIGTERM OS signals.
- **Architecture Decision Record (ADR-0004):**
  - Added [`docs/adr/0004-opening-explorer-http-api.md`](docs/adr/0004-opening-explorer-http-api.md) documenting framework selection, lock-free concurrency, snapshot pinning, and decompression threadpools.
- **Performance Benchmarking Harness (`scripts/bench_http.py`):**
  - Automated benchmark measuring startup time, RSS memory progression under concurrent load, single-client p50/p95/p99 latencies, concurrency scaling, and zero-downtime hot-reload under live traffic.
  - Output report written to [`benchmarks/results/http_api_benchmark.json`](benchmarks/results/http_api_benchmark.json).
  - Unit test suite [`scripts/test_bench_http.py`](scripts/test_bench_http.py) integrated into `scripts/validate.sh`.

### Changed
- **Server Hardening & Merge Readiness (PR #5 Final Hardening):**
  - Administrative token validation: `ServerConfig::validate()` strictly rejects empty or whitespace-only `--admin-token` values at startup, preventing accidental authentication with empty bearer tokens.
  - Internal HTTP error sanitization: HTTP 500 responses (`reload_failed`, `storage_error`, `internal_error`) log detailed diagnostics server-side without leaking secrets, but return sanitized messages to prevent exposing filesystem paths or storage internals.
  - Representation-accurate position ETags: position ETags incorporate collection identity, dataset build ID, canonical 128-bit position key, and the complete FEN string, eliminating collisions between differing FEN move counters or collections while preserving conditional caching (`304 Not Modified`).
  - Reader pool timeout CLI exposure: added `--reader-pool-timeout-secs` (`CODB_READER_POOL_TIMEOUT_SECS`, default 5s) to `codb serve`, validated to ensure timeout is greater than 0 seconds.

---

## [0.4.0] - 2026-11-06

### Added
- **Governance Standardization:**
  - Created [`AGENTS.md`](AGENTS.md) detailing operational instructions, 6-crate architectural boundaries, formatting/clippy rules, concurrency invariants, and documentation maintenance policy.
  - Created [`HANDOFF.md`](HANDOFF.md) establishing durable handoff logs, verification states, and next actions.
  - Created [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`CHANGELOG.md`](CHANGELOG.md) documenting contribution standards and release milestones.
  - Created [`MANIFEST.sha256`](MANIFEST.sha256) tracking cryptographic integrity of core fixtures, contracts, and schema definitions.
- **Canonical Documentation Architecture (`docs/`):**
  - Promoted normative specifications from `docs/research/` to canonical uppercase documents in `docs/`:
    - [`docs/README.md`](docs/README.md): Central navigable documentation index.
    - [`docs/adr/README.md`](docs/adr/README.md): Navigable ADR index and decision register.
    - [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): End-to-end crate boundaries, data flow, sharding (`shard.rs`), advisory flock (`lock.rs`), and memory ceilings.
    - [`docs/DATA_CONTRACTS.md`](docs/DATA_CONTRACTS.md): Unification of CGB3 container, CPI v2 index, domain models, CLI JSON schemas, and future HTTP API contract.
    - [`docs/CORPUS_AND_DEDUPLICATION.md`](docs/CORPUS_AND_DEDUPLICATION.md): Collection inclusion policies (`MastersOtbPolicy`, `EliteOnlinePolicy`), bullet exclusion formula ($T_{\text{est}} < 180\text{s}$), multi-key deduplication, prefix supersession, and fail-closed accounting.
    - [`docs/LICENSING.md`](docs/LICENSING.md): Consolidated source dataset inventory, provenance matrix, dual-licensing (`LICENSE-MIT` / `LICENSE-APACHE`), and clean-room zero-AGPL boundary.
    - [`docs/QA_STRATEGY.md`](docs/QA_STRATEGY.md): Comprehensive test strategy covering publication recovery, reader snapshot isolation, staging fault injection, and verification gates.
    - [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md): Authoritative benchmark methodology, production CPI baselines, empirical dataset checksums, and Heaps' Law scaling projections.
    - [`docs/ROADMAP.md`](docs/ROADMAP.md): Milestone progression tracking completed capabilities and the planned Opening Explorer HTTP/gRPC service.
    - [`docs/DECISIONS_AND_OPEN_QUESTIONS.md`](docs/DECISIONS_AND_OPEN_QUESTIONS.md): Accepted technical decisions, trade-off matrix, and open architectural questions for the future API.
- **Automated Documentation Validation:**
  - Created [`scripts/validate_docs.py`](scripts/validate_docs.py) to automatically verify Markdown link resolution, canonical uppercase filenames, required YAML frontmatter (`summary`, `read_when`, `status: current`), and ADR synchronization.
  - Created [`scripts/validate.sh`](scripts/validate.sh) as the single unified verification entrypoint executing formatting, Clippy, workspace tests, Python regression suites, benchmark validators, and doc validation.

### Changed
- **Sanitized Benchmarks Organization:**
  - Clarified separation between active production CPI baselines in `benchmarks/production_cpi/` and historical/runtime results in `benchmarks/results/`.
  - Created [`benchmarks/README.md`](benchmarks/README.md) explaining the directory architecture and validation requirements.
  - Moved intermediate artifacts from prior stages (`masters_1k..20k.json`, `pr02_scale_*.json`, `stage_a_validation.json`, `stage_b_*.json`, `projections_heaps.json`) to [`benchmarks/results/historical/`](benchmarks/results/historical/).
  - Updated [`scripts/compare_collections.py`](scripts/compare_collections.py), [`scripts/fit_heaps.py`](scripts/fit_heaps.py), [`scripts/test_feasibility_alignment.py`](scripts/test_feasibility_alignment.py), [`scripts/test_validate_benchmarks.py`](scripts/test_validate_benchmarks.py), [`scripts/validate_benchmarks.py`](scripts/validate_benchmarks.py), [`scripts/reproduce_benchmarks.sh`](scripts/reproduce_benchmarks.sh), and CLI in-tree test paths to reference the historical directory without breaking test suites.
- **CI Workflow Alignment:**
  - Updated [`.github/workflows/ci.yml`](.github/workflows/ci.yml) to execute the complete verification gate.

---

## [0.3.0] - 2026-11-04

### Added
- **Production Position Indexing (`codb-index` / CPI v2):**
  - Implemented [`ExternalPositionIndexer`](crates/index/src/external.rs) delivering bounded-memory external merge sort with configurable memory ceilings (`--memory-limit-mib`, 16–128 MiB).
  - Emitted temporary run files (`.crun`) with 30-byte records, 8-byte record count, and 32-byte SHA-256 footers.
  - Hierarchical K-way merge with fan-in bounding (`--max-merge-fan-in`, default 64).
  - Production CPI v2 binary container (`positions.cpi`) with 64-byte header and 34-byte zero-allocation memory-mapped sparse index ($O(\log B)$ block binary search).
- **Opening Explorer Queries (`codb query`):**
  - Sub-millisecond position lookups by orthodox legal FEN or SAN/UCI move sequence.
  - Returned candidate next moves with white wins, draws, black wins, and total game counters.
- **Atomic Dataset Publication & Recovery (`codb-storage`):**
  - Atomic index publication via hard-links and isolated staging directories (`versions/.staging_*`).
  - Active snapshot pinning allowing live readers to query immutable datasets without locking or degradation during publications.
- **Calibrated Production CPI Benchmarks:**
  - 16 matrix combination benchmark artifacts in `benchmarks/production_cpi/` covering 1k–100k games across memory limits (16, 32, 128 MiB) and merge fan-ins (2, 64).

---

## [0.2.0] - 2026-10-25

### Added
- **Streaming Ingestion & $O(1)$ Memory Scaling (`codb-ingest`):**
  - Streaming PGN parser built on `pgn-reader` with transparent Zstd decompression and full legal move replay via `shakmaty`.
  - Bounded memory ingestion maintaining peak RAM strictly below 30 MB RSS across builds of 50,000 to 100,000 games.
- **Persistent Sharded Storage Container (`codb-storage` / CGB3):**
  - Implemented `shard_XXXX.cgb3` container with 32-byte header (`CGCS`), Zstd-compressed blocks, and block descriptors.
  - Global memory-mapped binary search index (`games.idx`) with 16-byte fixed records for sub-millisecond random game retrieval by 64-bit ID.
  - Advisory process locking via `flock` (`.dataset.lock`).
- **Disk-Backed Deduplication (`codb-curate`):**
  - Added `DeduplicatorBackend::Disk` using embedded SQLite in staging to bound deduplication memory on multi-million game archives.
- **Scale Evaluation:**
  - Empirical 100k scale exploration across Masters OTB and Elite Online (`pr02_scale_*.json`).

---

## [0.1.0] - 2026-10-23

### Added
- **Foundational Architecture (Stage A Prototype):**
  - Initial 6-crate workspace structure (`core`, `ingest`, `curate`, `storage`, `index`, `cli`).
  - 16-bit bit-packed move encoding (`CompactMove`) and 128-bit `PositionKey`.
  - Block-level dictionary string interning in CGB containers (~208 bytes/game for Masters OTB).
  - Dual collection isolation (`masters-otb` and `elite-online`).
  - Deterministic Stage A validation fixture (1,000 games).
  - Heaps' Law scaling derivation ($V(N) = k \cdot N^\alpha$) projecting Option B 15M game storage to 7.68 GB baseline (9.98 GB conservative within 10 GB envelope).
