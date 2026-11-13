# Agent Handoff (`chess-openings-db`)

## 1. Current Repository State

- **Goal:** PR-A: Reproducible Production Corpus Assembly & Local Opening Explorer Delivery (branch `feat/corpus-release-pipeline`, Draft PR against `main`).
- **What exists now:** the engine from PR-01…PR-04 (7 crates, CGB3, CPI v2, HTTP API) **plus** a pinned source catalog and an orchestration layer that turns real, checksum-verified Lichess archives into two versioned releases and serves them:
  - `corpus/catalog.json` — 10 pinned archives (6 Lichess broadcast months 2026-05…10 for `masters-otb`, CC BY-SA 4.0; 4 complete Lichess standard-rated months 2016-03/06/09/12 for `elite-online`, CC0 1.0), each with publisher SHA-256, verified SHA-256, acquisition timestamp, provenance confidence, eligibility, notes.
  - `scripts/corpus.py` / `scripts/corpus.sh` — `plan`, `sources`, `fetch`, `verify-sources`, `pin`, `build`, `verify`, `report`, `package`, `verify-package`, `serve`, `http-check`. Stdlib only; drives the `codb` CLI; never reimplements ingestion/curation/indexing/publication.
  - `codb build --report-json` and `codb report` (accounting, rejection reasons, distributions).
  - Curation fixes (policy revision `r1`, no binary-format change): Masters online-event exclusion, pre-1970 games require named event + venue, Elite casual-event rejection.
  - `docs/CORPUS_BUILD.md` (procedure + measured results), `benchmarks/corpus/` (validated evidence), `fixtures/corpus/` (synthetic offline fixtures).
- **Real corpus actually built locally (not published anywhere):**

  | | `masters-otb` | `elite-online` |
  | :--- | ---: | ---: |
  | Archives | 6 (Lichess broadcasts 2026-05…10) | 7 (Lichess standard 2015-09, 2015-12, 2016-03/06/09/12, 2017-03) |
  | Accepted unique games | 42,612 (target ≥ 30,000 met) | 429,284 (target ≥ 100,000 met) |
  | Games encountered | 227,489 | 46,536,762 |
  | Index depth / unique positions / move records | 40 / 984,240 / 1,040,109 | 30 / 6,136,363 / 6,739,709 |
  | Peak RSS: build / index | 29.8 MB / 61.7 MB | 36.9 MB / 151.1 MB |
  | Build / index wall time | 25 s / 2.7 s | 657 s / 29.6 s |

  Release identifiers, hashes and all other numbers: `benchmarks/corpus/*/RELEASE.json`. Both releases were also packaged with `corpus.sh package` and independently re-verified (local archives only).
- **Verification evidence:** `codb verify --deep` PASS for both releases and extracted packages; `corpus.sh http-check`: 414/414 checks, 131 HTTP-vs-CLI stratified comparisons per collection, 0 mismatches; HTTP position p50 ≈ 220–230 µs; in-process `codb bench-lookup` p50 ≈ 5–6 µs; multi-process load up to ≈ 8–10k req/s (client and server on one 4-core host); 4 Masters builds and 2 same-input Elite builds gave byte-identical `games.idx`, `positions.cpi` and shards.
- **Known limitations / discrepancies (do not hide):**
  1. Memory: dedupe cache lowered 32 MB → 2 MB (identical output). Masters `codb build` 29.8 MB (within the 30 MB budget); Elite 36.9 MB (still above). Q-04 partially resolved.
  2. Masters OTB qualification is a marker heuristic over event/site/broadcast-slug; ~69% of accepted Masters games have no venue. Catalog confidence is `medium`; "no online games" cannot be proven.
  3. Elite corpus is seven complete archives from 2015-09 to 2017-03 (older era, later months dominate); modern archives are tens of GB and not downloaded. Not the 5M/10M target.
  4. `ProvisionalRating` and `AbortedOrInfraction` policy variants are still never produced.
  5. Load numbers are same-host (client + server share 4 cores); no dedicated-host capacity test.
  6. Releases are local artifacts only; no upload was performed or approved.
- **Format Integrity (unchanged):** `CGB3` (~204 B/game Masters, ~166 B/game Elite measured), `games.idx` 16-byte records, `CPI v2` (~21.5 B per indexed position).
- **HTTP Serving Layer:** unchanged from PR-04 (see §3). Documentation correction: unsupported parameters return HTTP 400 `unsupported_parameter` (docs previously said 422 in one place).

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

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`: PASS.
- Python suites (`scripts/test_*.py` incl. new `test_corpus.py`, 49 offline tests): PASS.
- `validate_production_cpi_benchmarks.py`, `validate_corpus_evidence.py`, `validate_benchmarks.py`, `validate_docs.py`, `sha256sum -c MANIFEST.sha256`: PASS.
- `./scripts/validate.sh`: PASS locally on the final tree (result also recorded in the PR description).
- Real-corpus validation is opt-in and was executed locally (§1); CI never downloads archives.

---

## 6. Next Actions

1. Maintainer review of the Draft PR; decide the remaining Q-04 gap (Elite build 36.9 MB vs the 30 MB budget).
2. Decide an approved distribution destination before publishing any `corpus.sh package` archive; keep `RELEASE/DATASET_LICENSE.txt` with it.
3. Evalis integration PR: consume `data/releases/{masters-otb,elite-online}` via `codb serve` (CORS: `--allowed-origins`), see `docs/CORPUS_BUILD.md` §10.
4. Optional expansion: pin more Elite/Masters periods in new catalog profiles; review further Masters sources for redistribution rights before use; improve OTB qualification if a Lichess flag appears.
