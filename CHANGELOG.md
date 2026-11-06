# Changelog

All notable changes to `chess-openings-db` are documented in this file. The project adheres to [Semantic Versioning](https://semver.org/).

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
