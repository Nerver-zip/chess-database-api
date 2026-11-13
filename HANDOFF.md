# Agent Handoff (`chess-openings-db`)

## 1. Current Repository State

- **Goal:** PR-A, reproducible production corpus assembly and local Opening Explorer delivery. Branch `feat/corpus-release-pipeline`; Draft PR #6 against `main`.
- **R2 release-build code head:** `7f4fa09` (`docs(data): refresh corpus policy and handoff evidence`), with code fix `5b97671` beneath it. The final R2 evidence and documentation have passed the repository validation gate and are ready for the follow-up commit. Preserve untracked `GOAL.md`; do not commit large `data/` artifacts.
- **Sources:** 13 pinned Lichess archives: six 2026-05…10 broadcast months for `masters-otb` (CC BY-SA 4.0) and seven complete standard-rated archives from 2015-09…2017-03 for `elite-online` (CC0 1.0). Local archives pass `corpus.sh verify-sources`; publisher checksum-list audit matched all 13 pinned SHA-256 values.
- **Workflow:** `scripts/corpus.py` / `scripts/corpus.sh` provide `plan`, `sources`, `fetch`, `verify-sources`, `pin`, `build`, `verify`, `report`, `package`, `verify-package`, `serve`, and `http-check`. The Python tool orchestrates the existing `codb` commands and routes build temp files to `data/work/<run>/tmp`.
- **Curation:** Masters requires ratings, standard chess, event/site/broadcast online-marker exclusion, historical event+venue, and explicit `Termination` filtering. Elite preserves Lichess Glicko-2 as a separate rating pool and applies rated/variant/time-control/casual and termination rules. `ProvisionalRating` is not inferable from Lichess standard PGN exports.
- **Real baseline:**

  | | `masters-otb` | `elite-online` |
  | :--- | ---: | ---: |
  | Archives | 6 | 7 |
  | Accepted unique games | 42,612 (target ≥ 30,000 met) | 429,150 (target ≥ 100,000 met) |
  | Games encountered | 227,489 | 46,536,762 |
  | Index depth / unique positions / move records | 40 / 984,240 / 1,040,109 | 30 / 6,136,307 / 6,739,644 |
  | Peak RSS: build / index | 29.51 MB / 62.13 MB | 29.73 MB / 150.68 MB |
  | Build / index wall time | 30.9 s / 2.67 s | 656.4 s / 18.23 s |
  | Deployed release directory bytes | 30,634,943 | 210,312,125 |

- **Current releases:** both `2026.11-r2` releases were built from clean commit `7f4fa09`; `RELEASE.json` records `git_tree_dirty: false`. IDs, artifact hashes, and package checksums are in `benchmarks/corpus/*/RELEASE.json` and adjacent evidence files.
- **Evidence:** deep verification and local package extraction/deep verification pass for both r2 corpora. HTTP check: 414/414 checks, 131 stratified HTTP-vs-CLI comparisons per collection, 0 mismatches. Direct `e4,e5,Nf3` queries match CLI; stored-game retrieval returned HTTP 200 for one real game in each collection. Independent same-input rebuilds matched `games.idx`, `positions.cpi`, and shard hashes for both collections. A post-build scan found 0 online-event marker matches across all 42,612 accepted Masters PGNs; unmarked online games remain possible.
- **Memory:** current measured `codb build` peaks are 29,507,584 bytes (Masters) and 29,732,864 bytes (Elite), under the 30,000,000-byte ceiling; Elite has 267,136 bytes of headroom on this host. Dedupe cache is 256 KiB, SQLite temp storage is disk-backed, and staging-catalog cache is 512 KiB.
- **Limitations:** Masters OTB attribution remains a medium-confidence marker heuristic; ~69% of accepted games have no physical venue, so unmarked online events cannot be ruled out. Elite covers 2015–2017 only. The future 5M/10M corpus is out of scope. Packages/releases remain local; nothing was uploaded.

The release-level files and benchmark JSON are the source of truth for build IDs, hashes, and package checksums. The R2 builds and full local validation are complete; the follow-up commit, push, and Draft PR refresh remain.
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
│   ├── CORPUS_BUILD.md # Reproducible corpus build/release procedure + measured results
│   ├── DECISIONS_AND_OPEN_QUESTIONS.md # Accepted trade-offs and open architectural questions (Q-04: build RSS)
│   ├── adr/        # Architecture Decision Records (ADR-0001 to ADR-0004)
│   └── research/   # Historical research tracks preserved for provenance and reproducibility
├── benchmarks/
│   ├── README.md   # Architecture of production baselines vs historical results
│   ├── production_cpi/ # 16 active production CPI baseline runs (ADR-0003)
│   ├── corpus/     # Real Masters/Elite release evidence + HTTP check (validated by validate_corpus_evidence.py)
│   └── results/    # Active benchmark outputs (http_api_benchmark.json) and historical archive
├── corpus/         # Pinned source catalog (catalog.json) — sources, licenses, publisher checksums
├── fixtures/       # Deterministic PGN fixtures (stage_a_validation.pgn, corpus/ synthetic offline fixtures)
├── scripts/        # validate.sh, corpus.sh/corpus.py (acquisition, build, release, serve), validators, tests
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
codb build --input <file.pgn.zst> --collection <masters-otb|elite-online> --output <dir> [--max-games N] [--report-json <file>]
codb build --manifest <sources.json> --collection <...> --output <dir>   # pinned inputs, used by corpus.sh

# Quality report (distributions, coverage, missing metadata)
codb report --dataset <dir> [--output <json>]

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

## 4. Operating Procedure (Local)

```bash
cargo build --release -p codb-cli
./scripts/corpus.sh plan
./scripts/corpus.sh fetch                      # ~5.2 GB, resumable, SHA-256 verified
./scripts/corpus.sh build --collection masters-otb
./scripts/corpus.sh build --collection elite-online
./scripts/corpus.sh verify
./scripts/corpus.sh http-check --per-stratum 40 --sample-games 6000 --requests 1000
./scripts/corpus.sh serve                      # 127.0.0.1:8080
./scripts/corpus.sh package --collection masters-otb   # optional
```

Data lives in `data/` (git-ignored; override with `CODB_DATA_DIR`). Never commit archives or releases. See `docs/CORPUS_BUILD.md` (including failure recovery).

---

## 5. Verification State (this branch)

- Focused `cargo test -p codb-curate -p codb-ingest`: PASS after the termination filter; formatting and release build pass.
- `corpus.sh verify-sources --collection all --profile baseline`: PASS for 13 local source archives.
- `corpus.sh verify --collection all --profile baseline`: deep verification PASS for both r2 releases; both local packages passed extraction and deep verification.
- `http-check`: 414/414; 262 HTTP-vs-CLI comparisons, 0 mismatches. Direct real-data HTTP/CLI capture and game retrieval evidence is under `benchmarks/corpus/`.
- `./scripts/validate.sh`: PASS on the final R2 worktree (all eight gates, including Rust Clippy/tests, Python regression tests, production and corpus evidence, docs, and manifest). The script rewrote a historical timing snapshot as designed; that generated change was restored. GitHub CI for the pushed follow-up commit is still pending.
---

## 6. Next Actions

1. Commit the validated R2 snapshots, marker-audit evidence, documentation, and handoff; do not stage `GOAL.md`.
2. Push the feature branch and replace the stale Draft PR #6 description with current R2 evidence; keep it Draft and do not publish packages.
3. Inspect CI for the pushed commit. GitHub CLI status-check access was denied by the configured token; use the connected GitHub integration or report the access limit.
4. Maintainer follow-up: improve or explicitly accept the remaining uncertainty for Masters events without OTB/online markers; keep Elite recency and provisional-rating limitations explicit.
