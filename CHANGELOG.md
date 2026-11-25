# Changelog

This file records notable changes to Chess Database API. The project adheres to [Semantic Versioning](https://semver.org/).

---

## [Unreleased]

### Chess Database API

- Added game, position, player and event discovery, individual search metadata, capabilities, and exact filtered Opening Explorer statistics.
- Added derived search indexes, deep verification, local companion bundles and snapshot-bound keyset pagination. Existing canonical games, lookup files and CPI v2 remain reusable.
- Adopted the `chess-database` CLI, `chess-database-*` Rust packages and `chess-database-api:local` image tag. Existing CLI, image, network and configuration aliases remain available. [Naming migration](docs/NAMING_MIGRATION.md).

- Added bounded rating and date projections for filtered Explorer and a two-worker search default with one aggregation slot. Serving checks artifact digests at each startup and reload, with deep structural verification at publication.

### Earlier release milestones
- **Licensing and deployment policy reconciliation:** `docs/LICENSING.md` section 6 is now the single authoritative statement of current policy, with a new decision record `releases/public_deployment_decision.json` for public, non-commercial API deployment (position statistics and single-game PGN retrieval). The existing distribution clearance is unchanged. Earlier private-evaluation and retrieval-disabled guidance is described as historical; the example environment file keeps retrieval enabled; Docker guidance adds a public-instance checklist. Commercial use and bulk export remain outside the decisions, and the publisher's B2B terms, unverified upstream rights, and missing checksum stay disclosed. Immutable R3 artifacts and `benchmarks/gigabase/` evidence are unchanged.
- **Earlier Opening Explorer API migration (PR #8):**
  - At that point, `opening-explorer-api` became the project identity: CLI `opening-explorer`, crates `opening-explorer-*`, `OPENING_EXPLORER_*` environment variables (legacy `CODB_*` accepted as deprecated fallbacks), renamed compose files and image. Binary magics, game IDs, CPI keys, the `codb-` build-identifier prefix, HTTP routes, and historical evidence are unchanged. See [docs/MIGRATION_FROM_CODB.md](docs/MIGRATION_FROM_CODB.md).
  - The root README is a first-visit landing page; the API contract moved to [docs/API_REFERENCE.md](docs/API_REFERENCE.md), with getting-started, use-case, and dataset-release guides and runnable `examples/`.
  - Backward-compatible API fixes: additive `cpi_depth_plies` on position responses; `POST /v1/admin/reload` is no longer cut off by the 15 s read timeout; framework rejections (bad query, 404, 405, timeout) use the standard error envelope; clearer illegal-move and missing-collection messages.
- **Dataset distribution tooling:** `opening-explorer export-pgn` (deterministic streaming PGN export), `scripts/release_assets.py` (prepare, split, verify, reconstruct, install, dry-run-by-default upload), an independent python-chess verifier, a manually triggered `publish-dataset.yml` workflow that only validates and publishes a reviewed draft release, and a distribution gate (`releases/distribution_clearance.json`) that blocks publication until written clearance is recorded. `dataset-masters-otb-r3` was published on 2026-11-16 after the maintainer recorded a distribution clearance (see `docs/LICENSING.md` section 6).

### Added
- **PR #7: Local Gigabase source qualification and evaluation (Draft PR):**
  - A streaming structural/header inventory for the supplied OTB Complete archive, with local archive/member hashes kept distinct from publisher authentication.
  - The historical r2 policy used a reviewed `Source` allowlist, Strong/Confirmed identity evidence, complete dates, and a pre-1970 cutoff. Current r3 uses the publisher's archive-level OTB assertion unless headers contain concrete contradictory evidence, preserves source and identity labels, accepts valid partial or unknown dates, and applies symmetric numeric/title strength rules.
  - A checksummed `source-attribution.jsonl` sidecar that records source and identity evidence per retained game without changing CGB3, CPI v2, or `games.idx` layouts.
  - An optional `CODB_ENABLE_GAME_RETRIEVAL=false` / `--enable-game-retrieval false` API setting that omits the stored-PGN route while leaving position queries and collection metadata available.
  - Source-qualified stable game ID collisions are quarantined and counted in the optional policy's accounting report; existing collection deduplication behavior remains unchanged.
  - `corpus.sh extract-local-7z` for one-member extraction through pinned py7zr, with traversal/link rejection and expansion limits. Its output remains local-only and marked ineligible for a catalog release without a publisher checksum and approved rights.
  - The historical r2 evaluation accounted for 13,450,696 records and retained 156,898 games; it passed deterministic rebuild, package, bootstrap, rollback, and internal Docker QA. Its policy and artifacts are preserved as historical evidence.
  - Current r3 full-source analysis scanned all 13,450,696 records and found 3,296,795 header-level candidates. The full build accounted for 13,450,696 records and retained 3,284,429 unique games, with 4,018 confirmed duplicates, 506 superseded records, and 7,842 stable-ID collision quarantines. The depth-40 CPI indexes 51,909,604 unique positions. The candidate remains a local evaluation; no production release or public service was changed.
  - The 2,700,902,400-byte r3 package passed checksum, extraction, artifact hash, deep-verification, and offline-query checks. Isolated bootstrap returned 1,469,608 games for `e4`; rollback restored the prior 29,640-game Masters snapshot. The real-data Docker API smoke matched CLI position queries and PGN bytes for Masters and Elite, passed all 96 concurrent requests, rejected four invalid dataset starts, and passed restart, deep verification, and graceful shutdown. It used a private network and read-only named volumes without host ports; host bind-mount behavior remains unverified.
  - A selected-position CPI oracle replayed all 3,284,429 retained games to 40 plies and matched direct CPI v2 lookups on 18 opening lines / 17 unique positions, with zero mismatches. It shares the production position-key function and does not test source-to-canonical mapping.
  - The completed r3 source-to-canonical audit matched all 3,284,429 retained games, recorded zero metadata, attribution, move, start-position, or final-board mismatches, and reconciled all 3,287,619 source-attribution pairs. The source-dedupe audit compared all 12,366 duplicate, supersession, and collision outcomes with canonical retrieval and had zero lookup failures. Its 162 strict move-prefix cases all share the starting board and reconcile as confirmed duplicates; 161 end at a different board and one at the same board, so only the former are counted as strict truncation comparisons. Both audits reuse the production parser and deduplicator.
  - A separate complete r3 rebuild used a distinct output root and scratch directory, matched accounting and quality distributions, passed deep verification, and reproduced all ten content artifact hashes. It uses the same CODB implementation and therefore establishes repeatability, not independent parser or deduplicator validation.
  - A local-only independent `python-chess` 1.11.2 sample used 180,000 byte windows and compared 426 selected r3 candidate games. All selected moves and boards matched; the sample recorded three source-attribution differences and two metadata differences, with no parser, legality, or retrieval failures. Quota misses and observed 370,136 KiB peak RSS are documented. The GPL-3.0-or-later runner and wheel are not in the repository or PR, and the sample is not a full-source parse.
  - The final quality report was regenerated from the indexed r3 snapshot after finding that the prior packaged report named an earlier build ID. Its distributions are unchanged and its build ID now matches the package. The corrected package checksum, deep verification, offline query, isolated bootstrap, and rollback all pass. No production release or pointer changed.
  - Historical r2 provenance-corrected rebuild evidence supplies a generated one-source CODB manifest pinned to the registered local PGN hash, labels the license as a publisher claim with upstream rights unresolved, and cross-checks the build report and dataset manifest. Its package passed checksum, extraction, deep verification, and an offline query, then bootstrapped in isolation and passed Docker route-enabled internal QA (96/96 concurrent requests, position and PGN byte matches) before rollback. This evidence applies to r2, not the current r3 snapshot. Host bind mounts and user-facing rights remain unverified.
  - The historical r2 supplemental external-parser check sampled 300,000 deterministic byte windows without `mmap`, compared 227 unique corrected-build games, and found exact metadata/move/board/attribution matches with no parser or legality errors. Four source-tag strata missed the ten-game quota; process peak RSS was measured at 339,388 KiB. The GPL-3.0-or-later parser and runner remain local-only, and this sample does not parse the full source. See `benchmarks/gigabase/INDEPENDENT_PGN_AUDIT_TARGETED.json` and its local runbook.
  - No approved public or production release was created. The 2026-11-15 review of the publisher's current terms records a free closed-beta limit of 50 unpaid testers for up to six months, a ban on direct raw-PGN downloads to end users, and no recorded Evalis project registration. Publisher checksum and heterogeneous upstream-rights gaps remain unresolved; the result is not cleared for user-facing deployment.
- **Reproducible corpus release workflow (PR-A):**
  - `corpus/catalog.json`: pinned source catalog (publisher, URL, archive period, license, expected size, publisher SHA-256, verified SHA-256, acquisition timestamp, provenance confidence, eligibility, notes) for the Lichess broadcast (CC BY-SA 4.0) and standard rated (CC0 1.0) archives.
  - `scripts/corpus.py` / `scripts/corpus.sh`: `plan`, `sources`, `fetch`, `verify-sources`, `pin`, `build`, `verify`, `report`, `package`, `verify-package`, `serve`, `http-check`. Resumable HTTPS downloads with retry/backoff, publisher-checksum verification, atomic finalization, disk-space and size safety limits, offline verification; builds in a private workspace with deep verification before atomic promotion; deterministic release packaging.
  - `codb build --report-json` and `codb report` (machine-readable accounting, rejection reasons, and distributions).
  - Release provenance (`RELEASE/RELEASE.json`) and legal notice (`RELEASE/DATASET_LICENSE.txt`) bundled with every release.
  - `codb bench-lookup` (in-process CPI lookup latency on a published release) and a multi-process HTTP load client in `http-check`.
  - `docs/CORPUS_BUILD.md`, offline fixtures (`fixtures/corpus/`), and `scripts/test_corpus.py`.
- **Dockerized Opening Explorer and portable real releases (PR #6 follow-up):**
  - Multi-stage, non-root CODB API image and Compose deployment with read-only release mounts, restricted host exposure, health checks, bounded runtime settings, and shared-network access at `http://codb:8080`.
  - Package checksum bootstrap and explicit pinned-source rebuild workflow; startup never fetches or rebuilds corpora.
  - Real Masters event audit now qualifies a 29,640-game verified OTB subset; 1 online game and 12,971 unresolved games in the 42,612-game audit baseline are excluded.
  - Atomic immutable release publication, per-process RSS measurement, and real container-to-container HTTP integration evidence for both collections.

### Changed
- **Project renamed to `opening-explorer-api`:** the repository, documentation, and Docker image (`opening-explorer-api:local`) use the new name. The CLI binary is `opening-explorer`; the Cargo packages are `opening-explorer-{core,ingest,curate,storage,index,api,cli}`; Compose files are `compose.yaml`, `compose.named-volume.yaml`, and `compose.debug.yaml`; example settings are `.env.example` and `.env.gigabase.example`. The legacy `CODB_*` environment variables remain accepted as deprecated fallbacks. See [MIGRATION_FROM_CODB.md](docs/MIGRATION_FROM_CODB.md).
- **Immutable identifiers are unchanged:** generated build and release IDs keep the `codb-` prefix; the `CGB3`, `CODB_CPI`, and `CODB_RUN` binary magics, game IDs, CPI keys, HTTP routes, and error types are unchanged; historical benchmark and release evidence is not rewritten.
- **Curation policy revisions `2026.11-r3` / `2026.11-r2`:** Masters r3 applies exact event-level verified OTB tournament IDs from the checked-in audit and excludes verified online and unresolved events; it retains termination and historical provenance filters. Elite r2 rejects explicit aborted, abandoned, no-contest, unplayed, unterminated, and rules-infraction `Termination` values alongside its casual and bullet exclusions. Unknown termination values remain eligible. `ProvisionalRating` cannot be inferred because standard Lichess PGNs expose no provisional marker.
- Policy rejections are reported by stable category (`PolicyRejectionReason::category`).

### Fixed
- Deduplication SQLite cache is 256 KiB, temp storage is disk-backed, and the staging-catalog cache is 512 KiB. The corpus runner records executed-process `VmHWM`; ingestion RSS is measured per host and has no fixed 30 MB cutoff.
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
