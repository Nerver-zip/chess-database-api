# Agent Development Guide (`chess-openings-db`)

Begin every agent turn by reading `AGENTS.md` and `HANDOFF.md`, then verify the current Git state with `git status`. Consult `README.md` and [docs/README.md](docs/README.md) as needed to navigate relevant technical specifications. Inspect the source code before modifying behavior. At the end of every substantive turn, update `HANDOFF.md` with current state, verification evidence, and next actions.

Documentation is a maintained product artifact, not a byproduct of development. Update the relevant canonical document when production behavior, data formats, or architecture change. Keep historical research distinct from current behavior, and never describe prototype experiments as production. Preserve persisted binary layout contracts, error types, and compatibility contracts. Keep architectural diagrams synchronized with their implementations. Use uppercase basenames with a lowercase `.md` extension for repository-authored Markdown in `docs/`; canonical documents must use concise `summary`, `read_when`, and `status: current` YAML frontmatter. Root Markdown files (`README.md`, `AGENTS.md`, `HANDOFF.md`, `CONTRIBUTING.md`, `CHANGELOG.md`) must NOT contain frontmatter. Run `./scripts/validate.sh` before completing any task.

---

## 1. Core Engineering Invariants

1. **English Language Requirement:** All project code, docstrings, comments, documentation, commit messages, and user-visible strings must be in **English**.
2. **Untouchability of Core & Binary Formats:** Never break the backward compatibility, bit-level layout, or determinism of `CGB3` (`shard_XXXX.cgb3`), `CPI v2` (`positions.cpi`), `games.idx`, or deduplication policies (`crates/curate/src/policy.rs`, `dedupe.rs`).
3. **Zero AGPL Code Reuse:** Never import, copy, or derive logic from `lila-openingexplorer` or any other AGPL-3.0 codebase. All components in `chess-openings-db` must remain independently authored and dual-licensed under MIT OR Apache-2.0.
4. **Collection Segregation:** Keep `masters-otb` and `elite-online` strictly segregated across data pipelines, persistent shards, and query engines. Never pool or average FIDE Elo ratings with online platform ratings.
5. **Fail-Closed Accounting:** Every encountered game must be accounted for across mutually exclusive categories (`total_encountered = accepted_unique + confirmed_duplicates + superseded_prior + rejected_by_policy + rejected_variant + rejected_illegal + rejected_malformed + quarantined`). If counters diverge or `retained_canonical_games != accepted_unique`, fail closed immediately.
6. **Bounded Memory Guarantees:** Ingestion memory must stay strictly below $30\text{ MB}$ RSS ($O(1)$ memory scaling). Position indexing must strictly respect `--memory-limit-mib` (default 128 MiB) via external sorting and partitioned `.crun` spill files.
7. **Advisory Locking & Concurrency Invariants:** Mutations (`build`, `build-index`, `recover`) must acquire an exclusive advisory lock on `.dataset.lock` using `flock(LOCK_EX | LOCK_NB)`. Readers are completely lock-free, reading pinned immutable snapshots under `versions/vX/`.
8. **Atomic Dataset Publication:** Datasets must be built in staging directories (`versions/.staging_*`), deeply verified, and published via atomic symlink replacement (`rename`). Crashes or power losses must leave the active snapshot untouched.
9. **No Stale Projections:** Never reintroduce stale, superseded storage estimates or non-reconciled numbers in documentation or code comments.

---

## 2. Workspace Overview (The 6 Crates)

The workspace is organized into six dedicated crates:

- **[`codb-core`](crates/core):** Foundational domain models (`Collection`, `CompactMove`, `PositionKey`, `GameResult`, `GameMetadata`, `IngestionStats`). Contains bit-packing logic, stable 64-bit ID computation, and core invariant definitions.
- **[`codb-ingest`](crates/ingest):** High-throughput streaming PGN parser supporting transparent `.zst` decompression and full legal move validation via `shakmaty`. Computes time control duration ($T_{\text{est}} = \text{base} + 40 \times \text{inc}$) to exclude bullet games.
- **[`codb-curate`](crates/curate):** Policy filters (`MastersOtbPolicy`, `EliteOnlinePolicy`) and multi-key deduplication (`Deduplicator`). Implements normalized metadata keys, move-slice prefix supersession, and disk-backed SQLite staging catalog.
- **[`codb-storage`](crates/storage):** CGB3 block container (`CODB-v3`), multi-shard management (`shard_XXXX.cgb3`), 16-byte fixed-record `games.idx` memory-mapped binary search index, process-level advisory file locking (`.dataset.lock`), and atomic symlink publication.
- **[`codb-index`](crates/index):** Position extraction (`extract_game_positions_detailed`), bounded-memory external merge sort pipeline (`ExternalPositionIndexer`), `.crun` temporary run emission, hierarchical K-way merge, CPI v2 container (`positions.cpi`), and Opening Explorer query execution.
- **[`codb-cli`](crates/cli):** The unified command-line tool (`codb`) providing commands for `build`, `build-index`, `query`, `verify`, `inspect`, `get-game`, and `bench`.

---

## 3. Formatting, Linting & Verification Commands

Before concluding any work, the following commands must execute cleanly:

```bash
# 1. Format check
cargo fmt --all -- --check

# 2. Strict Clippy check with warnings denied
cargo clippy --workspace --all-targets -- -D warnings

# 3. Workspace test suite
cargo test --workspace

# 4. Python regression test suites
python3 -m unittest scripts/test_feasibility_alignment.py \
                    scripts/test_reproduce_runner.py \
                    scripts/test_validate_benchmarks.py

# 5. Production CPI benchmark validator
python3 scripts/validate_production_cpi_benchmarks.py

# 6. Historical benchmark schema and accounting validator
python3 scripts/validate_benchmarks.py

# 7. Documentation integrity & markdown link validator
python3 scripts/validate_docs.py

# 8. Cryptographic contract manifest verification
sha256sum -c MANIFEST.sha256
```

All 8 checks are executed sequentially by the unified verification entrypoint:
```bash
./scripts/validate.sh
```

---

## 4. Documentation Policy

- Canonical architectural and normative documents live in `docs/` with uppercase basenames (`docs/ARCHITECTURE.md`, `docs/DATA_CONTRACTS.md`, `docs/CORPUS_AND_DEDUPLICATION.md`, etc.).
- Canonical documents must contain valid YAML frontmatter:
  ```markdown
  ---
  summary: "Brief description of document purpose."
  read_when: "When a developer or agent should consult this."
  status: current
  ---
  ```
- Historical research documents remain under `docs/research/` with `status: historical` or `status: reference` frontmatter and must not be treated as production specifications.
- Every canonical document must be indexed in [docs/README.md](docs/README.md).
- Markdown links must resolve to real files and existing target anchors. Run `python3 scripts/validate_docs.py` to assert documentation integrity.
