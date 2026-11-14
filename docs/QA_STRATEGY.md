---
summary: "Quality assurance strategy, integrity testing, concurrency, crash recovery, and verification gates."
read_when: "Understanding QA requirements, running regression test suites, or designing crash-safety tests."
status: current
---

# Quality Assurance & Testing Strategy

## 1. Principles and Engineering Guardrails

`chess-openings-db` employs an evidence-based, fail-closed verification strategy designed for mission-critical persistence and high-concurrency analytical queries:

1. **Intouchability of Binary Formats:** Persistent storage (`CGB3`), position index layouts (`CPI v2`), and deduplication rules are immutable contracts. Changes must never silently break backward compatibility or determinism.
2. **Fail-Closed Accounting:** Every encountered game is accounted for across mutually exclusive categories. Any counter discrepancy halts the pipeline and refuses publication.
3. **Snapshot Isolation for Readers:** Live readers holding open file descriptors are guaranteed 100% consistent views of immutable snapshots, even during background index updates or dataset publications.
4. **Crash-Safe Publications:** Datasets use verified immutable versions and atomic current-symlink swaps. Process interruption leaves the prior active snapshot available.

---

## 2. Test Suite Architecture

```
chess-openings-db/
├── crates/
│   ├── core/src/models.rs            # Unit tests for compact moves, position keys, stable IDs
│   ├── curate/src/                   # Unit tests for policy filters and deduplication
│   ├── ingest/tests/
│   │   └── pipeline_tests.rs         # Streaming ingestion, multi-file superseding, limits
│   ├── storage/tests/
│   │   ├── publication_tests.rs      # Concurrency, crash recovery, atomic symlinks (active-reader and atomic version-transition tests)
│   │   ├── persistent_storage_tests.rs # Shard roundtrip, binary search, corruption safety
│   │   └── integrity_tests.rs        # Deep verification, signed boundaries, query plans
│   ├── index/src/                    # Unit tests for CPI v2 blocks, external sort, queries
│   ├── api/tests/
│   │   └── api_tests.rs              # HTTP contract, security, bounded pool concurrency, caching (18 tests)
│   └── cli/src/main.rs               # End-to-end CLI workflows (build, index, query, verify)
└── scripts/
    ├── test_feasibility_alignment.py # Regression test for storage projections & roadmap
    ├── test_reproduce_runner.py      # Regression test for reproduction runner modes
    ├── test_validate_benchmarks.py   # Benchmark schema and mathematical monotonicity tests
    ├── test_bench_http.py            # HTTP benchmark harness percentiles and continuous memory sampler tests
    ├── test_docker_deployment.py     # Compose, read-only mounts, and optional fixture container smoke
    ├── test_masters_event_audit.py   # Offline event provenance and redirect-safety tests
    └── validate_production_cpi_benchmarks.py # Production CPI schema & memory budget validator
```

---

## 3. Storage, Concurrency & Crash Recovery Suites (`crates/storage/tests/`)

### 3.1 Publication & Crash Recovery Tests (`publication_tests.rs`)
The integration suite validates transactional publication, including:
- **Subprocess Termination Simulation (`test_crash_simulation_subprocess_termination`):** Spawns a child process performing publication and forcefully terminates it with `SIGKILL` at various points. Asserts that the prior active dataset version remains intact and readable without corruption.
- **Fault Injection During Symlink Transition:** Verifies that I/O or filesystem errors immediately prior to, during, or after symlink replacement fail closed and restore previous backups cleanly.
- **Reader Snapshot Pinning (`test_live_reader_pinned_to_snapshot_across_publication`):** Demonstrates that an active reader process maintaining open file descriptors continues reading the original snapshot without degradation while a publisher replaces `current` with a newly built dataset version.
- **Broken Symlink Recovery (`test_recovery_filesystem_failure_injection_suite`):** Injects corrupted symlinks, partial directory copies, and missing manifests, verifying that automatic recovery reconstructs valid snapshots or falls back to known-good versions.

### 3.2 Persistent Storage & Corruption Safety (`persistent_storage_tests.rs`)
- **Invalid Magic & Corruption Rejection:** Tests that files with corrupted headers (`CGB3`, `CGCS`, `CODB_CPI`) fail immediately with explicit errors (`StorageError::InvalidMagic`) rather than memory panics or undefined behavior.
- **Binary Search Lookup Integrity:** Validates that `games.idx` accurately locates records across shard boundaries and block offsets.
- **Dataset Supersession & Compaction:** Verifies that when games are updated or superseded, compaction correctly reclaims space while maintaining index integrity.

### 3.3 Deep Integrity Verification (`integrity_tests.rs`)
- **Tamper Detection:** Injects subtle corruptions (e.g., swapped game IDs with valid checksums, count mismatches between manifest and lookup index, corrupted block offsets) and asserts that `--deep` verification catches every anomaly.
- **Signed vs Unsigned 64-bit Boundaries:** Verifies that game IDs crossing the signed `i64::MAX` boundary (`> 0x7FFF_FFFF_FFFF_FFFF`) sort and retrieve correctly in SQLite and binary search indices.
- **Zero Temporary B-Trees:** Uses SQLite `EXPLAIN QUERY PLAN` assertions to verify that staging deduplication catalog queries execute with zero temporary B-tree allocations.

---

## 4. End-to-End CLI & HTTP Service Testing

### 4.1 CLI Workflows (`crates/cli/src/main.rs`)
The CLI crate includes end-to-end test scenarios exercising real user workflows:
- `test_cli_build_inspect_get_game_verify_e2e`: Builds a persistent dataset from PGN fixtures, inspects metadata, retrieves games by stable ID, and runs `--deep` integrity verification.
- `test_cli_build_index_query_and_inspect_verify_e2e`: Builds a dataset, constructs a CPI v2 position index via bounded external sort, executes Opening Explorer queries across SAN, UCI, and FEN formats, and verifies position statistics.
- `test_benchmark_artifacts_and_projections_consistency`: Automatically verifies that all machine-readable benchmark JSON files in `benchmarks/results/historical/` strictly adhere to the authoritative schema.

### 4.2 HTTP Service Integration Suite (`crates/api/tests/api_tests.rs`)
The HTTP layer includes 18 deterministic integration tests verifying end-to-end server behavior:
- **Administrative Reload Security (`test_admin_reload_security`):** Asserts 403 Forbidden when unconfigured, 401 Unauthorized on missing/invalid tokens, 401 on cross-origin triggers (`Origin` header), and 200 OK with valid tokens.
- **Bounded Reader Pool Capacity (`test_bounded_reader_pool_capacity`):** Verifies that concurrent reader acquisitions strictly respect pool capacity (`peak_active_readers <= capacity`) under high request concurrency and release permits cleanly.
- **RFC 7232 Existence Check (`test_conditional_caching_missing_resource_returns_404`):** Verifies that non-existent games return 404 Not Found before evaluating `If-None-Match`, while existing games return 304 with wildcards, strong, or weak ETags.
- **Fail-Closed Version Replacement (`test_actual_version_replacement_a_to_b`, `test_corrupted_replacement_rejected`):** Proves atomic symlink transitions from v1 to v2, and asserts that candidate snapshots with corrupted CPI or checksum errors fail closed with 500 without degrading active serving.
- **Concurrent In-Flight Stability (`test_concurrent_queries_during_version_replacement`):** Verifies zero dropped queries during live symlink reload under background load.

                    scripts/test_corpus.py \
                    scripts/test_docker_deployment.py \
                    scripts/test_masters_event_audit.py
Offline, network-free regression tests (a local HTTP server with fault injection plus tiny synthetic fixtures in `fixtures/corpus/`) cover:
- **Acquisition:** correct, incorrect, and missing checksums; truncated and interrupted transfers (resume with `Range`, retry/backoff, retries exhausted with the partial kept but never finalized); idempotent reruns; corrupt existing files quarantined and refetched; unexpected HTTP responses; pinned-size mismatches; servers that ignore `Range`; offline mode; disk-space safety; unapproved or disallowed licenses; non-HTTPS URLs.
- **Catalog:** schema validation, mixed-collection profile rejection, deterministic `(period, id)` source ordering.
- **Assembly:** exact accounting on a fixture with OTB, online, low-rated, variant, illegal, duplicate, superseded, historical, and missing-metadata records; cross-archive deduplication; source-order independence; identical CPI hashes across rebuilds.
- **Publication:** a failed build, failed index step, or failed deep verification never replaces the active release; repeated builds preserve the previous release and its immutable versions; artifact tampering is detected.
- **Packaging:** required files present, lock/temporary files excluded, checksum emitted, extraction re-verified with `codb verify --deep`, tamper detection, byte-reproducible archives.
- **Serving:** both collections served from generated releases; semantic HTTP checks (WDL reconciliation, collection identity, dataset version, candidate legality, HTTP/CLI equivalence on stratified positions, transposition equality, absent and beyond-depth positions, error behavior, game retrieval, cross-collection isolation); hot reload of a newly published snapshot.

Offline tests cover release checksum verification, unsafe or incomplete package rejection, exact event-audit qualification, publication while active readers pin a snapshot, and Docker/Compose configuration. The fixture container smoke runs when a Docker daemon is available; it does not fetch production archives. Real-corpus verification and the real Docker API integration (python3 scripts/docker_smoke.py --real --data-dir data) are explicit opt-in steps documented in [DOCKER_DEPLOYMENT.md](DOCKER_DEPLOYMENT.md). Ordinary CI never downloads the 8.66 GB source archives.

---

## 5. Automated Verification Gates

Every pull request and commit must satisfy the complete verification gate:

```bash
# 1. Rust code formatting check
cargo fmt --all -- --check

# 2. Rust Clippy linter with zero warnings allowed
cargo clippy --workspace --all-targets -- -D warnings

# 3. Full Rust unit and integration test suite
cargo test --workspace

# 4. Python regression and benchmark test suites
python3 -m unittest scripts/test_feasibility_alignment.py \
                    scripts/test_reproduce_runner.py \
                    scripts/test_validate_benchmarks.py \
                    scripts/test_bench_http.py \
                    scripts/test_corpus.py \
                    scripts/test_docker_deployment.py \
                    scripts/test_masters_event_audit.py
python3 scripts/validate_production_cpi_benchmarks.py
python3 scripts/validate_corpus_evidence.py

# 6. Historical benchmark schema and accounting validator
python3 scripts/validate_benchmarks.py

# 7. Documentation integrity and internal link validator
python3 scripts/validate_docs.py

# 8. Cryptographic contract manifest verification
sha256sum -c MANIFEST.sha256
```

All 8 steps are bundled into the executable test harness:
```bash
./scripts/validate.sh
```
