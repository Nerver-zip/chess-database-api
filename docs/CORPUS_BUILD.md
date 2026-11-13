---
summary: "Step-by-step reproducible procedure from pinned source catalog to verified Masters OTB and Elite Online releases served over HTTP, with measured results, resource needs, license obligations, and failure recovery."
read_when: "Building, verifying, packaging, or serving the real corpus; integrating a client (for example Evalis) with real opening data; or changing the source catalog."
status: current
---

# Corpus Build & Release Procedure

This document is the operational counterpart of [CORPUS_AND_DEDUPLICATION.md](CORPUS_AND_DEDUPLICATION.md) (what is accepted), [LICENSING.md](LICENSING.md) (what may be redistributed), and [ARCHITECTURE.md](ARCHITECTURE.md#61-corpus-orchestration-layer-outside-the-rust-workspace) (where the tooling sits). It describes how to turn **pinned, checksum-verified Lichess archives** into two immutable, queryable datasets (`masters-otb`, `elite-online`) and serve them with `codb serve`.

The orchestration (`scripts/corpus.sh` → `scripts/corpus.py`, Python standard library only) never reimplements ingestion, curation, deduplication, indexing, or publication. It invokes `codb build`, `codb build-index`, `codb verify --deep`, and `codb report`.

## 1. Pipeline

```text
corpus/catalog.json (pinned sources, licenses, publisher SHA-256)
        |  corpus.sh fetch        HTTPS, resume, retry, size + SHA-256 vs publisher
        v
data/sources/<collection>/*.pgn.zst              (git-ignored)
        |  corpus.sh build        verifies every archive again, then:
        v
codb build --manifest  ->  policy + dedupe + CGB3 + games.idx   (private workspace data/work/)
codb build-index       ->  CPI v2 positions.cpi (bounded memory)
codb verify --deep     ->  deep integrity gate
codb report            ->  quality report
        |  atomic promotion (previous release kept in data/releases/.retired/)
        v
data/releases/<collection>/ { versions/<id>/, current, RELEASE/ }
        |-- corpus.sh package  ->  data/packages/<collection>-<version>.tar + .sha256
        '-- corpus.sh serve    ->  codb serve (both collections)
```

## 2. Prerequisites

| Requirement | Notes |
| :--- | :--- |
| Rust stable toolchain | `cargo build --release -p codb-cli` produces `target/release/codb`. |
| Python 3.11+ | Standard library only; no `pip install`. |
| Network | HTTPS to `database.lichess.org`. Offline verification (`--offline`, `verify-sources`) needs none. |
| Disk | See §3. The data directory defaults to `./data` (git-ignored); override with `--data-dir` or `CODB_DATA_DIR`. The tool refuses a data directory inside the repository unless it is under a git-ignored directory. |
| Memory | Indexing honors `--memory-limit-mib` (default 128). Measured peaks are in §9. |

`/tmp` may be mounted `noexec` or be small (it is a 512 MiB tmpfs in some sandboxes). The build sets `TMPDIR` to its own workspace under the data directory so deduplication databases and spill files never depend on `/tmp`.

## 3. Resource Estimate (before acquisition)

```bash
./scripts/corpus.sh plan --collection all
```

prints network transfer remaining, input storage, expected accepted games, expected CGB3 and CPI bytes, temporary workspace, memory ceilings, and a rebuild-time estimate, computed from the planning figures stored in the catalog (`collections.<c>.planning`). The figures are derived from measured builds (§9) and are estimates, not guarantees. Measured values for the delivered baseline:

RESOURCE_TABLE_PLACEHOLDER

## 4. Inspect the Pinned Source Catalog

```bash
./scripts/corpus.sh sources --collection all     # id, period, license, size, confidence, local status
./scripts/corpus.sh sources --json
```

`corpus/catalog.json` pins, for every archive: source id, collection, publisher, official URL, filename, period, license identifier, expected size, the **publisher-provided SHA-256** (from `https://database.lichess.org/<family>/sha256sums.txt`), the SHA-256 we verified, the acquisition timestamp, provenance confidence, eligibility restrictions, and known limitations. Validation is strict and fail-closed (see `scripts/test_corpus.py`): an entry without a well-formed publisher checksum, with a non-HTTPS URL, with a license that is not `approved`, with a license not allowed for its collection, or a profile that mixes collections is rejected before any download.

Extending the catalog uses the publisher's own checksum list rather than hand-typed hashes:

```bash
./scripts/corpus.sh pin --family broadcast --periods 2026-11 --profile baseline \
    --confidence medium --eligibility "..." --notes "..."
```

Pinned archives live in profiles. `baseline` is the delivered corpus; add further profiles for larger builds.

### 4.1 Approved sources

| Collection | Source | License | Periods in `baseline` |
| :--- | :--- | :--- | :--- |
| `masters-otb` | Lichess official broadcasts, `database.lichess.org/broadcast/` | CC BY-SA 4.0 | 2026-05 … 2026-10 (six monthly archives) |
| `elite-online` | Lichess standard rated games, `database.lichess.org/standard/` | CC0 1.0 | 2016-03, 2016-06, 2016-09, 2016-12 (four complete monthly archives) |

No other source is approved. TWIC, PGN Mentor, Caissabase, KingBase, commercial databases, and community mirrors are excluded because their redistribution rights or upstream provenance cannot be established ([LICENSING.md](LICENSING.md)).

### 4.2 Source qualification and its limits

* **Masters OTB.** Broadcast archives contain both over-the-board and online events and have no explicit OTB flag. Qualification is executed by `MastersOtbPolicy`: FIDE-rating thresholds, standard chess only, minimum length, online-event marker exclusion (event, site, broadcast URL), and a pre-1970 provenance requirement. The catalog marks these sources `provenance_confidence: medium`. The residual risk (an online event without any marker) is not eliminated and is disclosed in the release notice. `time_control_raw_top20` in the quality report shows the raw `TimeControl` strings, whose units are not consistent across broadcasts.
* **Elite Online.** The complete monthly archives are SHA-256 verified end to end. Ratings are Lichess Glicko-2 and are never pooled with FIDE Elo. The 2016 months were chosen because complete archives are small enough to download in full and verify (1.0–1.7 GB each); modern monthly archives are tens of gigabytes. Quarterly months give coverage across a year, but the corpus reflects 2016 Lichess play and 2016 rating levels. A partial or truncated stream of a larger archive would **not** be a verified archive and is never produced by this tool.

## 5. Download and Verify Archives

```bash
./scripts/corpus.sh fetch --collection all           # or --collection masters-otb / elite-online
./scripts/corpus.sh fetch --collection elite-online --source lichess-standard-2016-06   # one archive
./scripts/corpus.sh verify-sources                   # offline re-hash against the publisher checksums
```

Behavior (all covered by offline tests):

* HTTPS only; streaming to `<file>.part`; resume with `Range` after interruption; exponential backoff retries (`--retries`, `--backoff`); a server that ignores `Range` restarts cleanly.
* An archive is finalized **only** when its size equals the pinned size **and** its SHA-256 equals the publisher checksum, via atomic rename. A truncated or mismatching download is never presented as verified; a checksum mismatch deletes the partial file and fails.
* Idempotent: a verified archive is not downloaded again (a sidecar `*.verified.json` records size, mtime, and hash; `--rehash` forces a full re-hash). A corrupt existing file is quarantined as `*.corrupt-<ts>` and refetched.
* Safety limits: `--min-free-bytes` (default 2 GiB headroom), `--max-download-bytes`; `--offline` never touches the network.
* `--update-catalog` records `verified_sha256` and `acquired_at` in the catalog after a verified fetch. Run it from a single process (parallel `--source` fetches should be followed by one offline `fetch --update-catalog` to merge their records).

## 6. Build Both Collections

```bash
./scripts/corpus.sh build --collection masters-otb    # default depth 40 plies
./scripts/corpus.sh build --collection elite-online   # default depth 30 plies
```

`build` first re-verifies every archive and **refuses to build from unverified bytes**. It then runs, in a private workspace `data/work/<run-id>/`:

1. `codb build --manifest … --report-json …` (sources in deterministic `(period, id)` order, disk-backed deduplication),
2. `codb build-index --depth-plies N --memory-limit-mib 128 --json`,
3. `codb verify --deep`,
4. `codb report`,

writes `RELEASE/` (provenance, license notice, reports, per-step wall time, peak RSS, and peak workspace disk), and only then renames the workspace into `data/releases/<collection>/`. A post-promotion `codb verify --deep` runs on the published path; if it fails, the previous release is restored. A failure at any step leaves the active release untouched and the workspace in place for diagnosis. A rerun creates a new immutable version; the previous release is moved to `data/releases/.retired/` and never deleted by the tool.

`--depth-plies` and `--memory-limit-mib` override the defaults; options are recorded in `RELEASE/RELEASE.json`.

### 6.1 Release layout

```text
data/releases/masters-otb/
  current -> versions/<build-id>-cpi-d40
  versions/<build-id>-cpi-d40/{manifest.json, games.idx, positions.cpi, shards/shard_0000.cgb3, ...}
  versions/<build-id>/                 # pre-index snapshot; shards and games.idx are hard links
  manifest.json games.idx positions.cpi shards -> current/...   # convenience symlinks
  RELEASE/{RELEASE.json, DATASET_LICENSE.txt, build_report.json, index_report.json,
           quality_report.json, verify_deep.txt, metrics.json, sources.manifest.json}
```

`RELEASE.json` carries: collection, dataset version, build configuration, source catalog identity (id and content hash), exact input checksums, curation-policy version, accepted game count, index depth, CPI hash, per-artifact SHA-256, license, attribution requirements, the tool's git commit, and the build/verify/serve commands.

## 7. Verify

```bash
./scripts/corpus.sh verify                         # codb verify --deep + artifact re-hash for both releases
./target/release/codb verify --dataset data/releases/masters-otb --deep
./scripts/corpus.sh report --collection all        # regenerate quality reports under data/reports/
```

`codb verify --deep` checks the manifest, shard block checksums, `games.idx`, CPI integrity, collection identity, game counts, and the accounting identity `total_encountered = accepted_unique + confirmed_duplicates + superseded_prior + rejected_by_policy + rejected_variant + rejected_illegal + rejected_malformed + quarantined`.

## 8. Package, Serve, and Query

### 8.1 Package (independently verifiable archives)

```bash
./scripts/corpus.sh package --collection masters-otb
./scripts/corpus.sh verify-package --archive data/packages/masters-otb-<version>.tar
```

The `.tar` contains the current version directory (including `positions.cpi`, shards, `games.idx`, `manifest.json`), the root symlinks, and `RELEASE/` (license notice included); it excludes lock files, the superseded pre-index version, and temporary files. The archive is written deterministically (sorted names, zero mtimes and owners), so the same release yields the same bytes. `package` emits `<archive>.sha256`, then extracts into a fresh directory, re-hashes every artifact, runs `codb verify --deep`, and executes a query before reporting success. **A package on local disk is not a public download**: nothing in this repository uploads datasets anywhere.

### 8.2 Serve

```bash
./scripts/corpus.sh serve                       # 127.0.0.1:8080
# equivalent:
./target/release/codb serve \
  --masters-dataset data/releases/masters-otb \
  --elite-dataset   data/releases/elite-online \
  --host 127.0.0.1 --port 8080 \
  --allowed-origins http://localhost:3000,http://localhost:5173
```

### 8.3 Representative queries

```bash
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8080/ready
curl -s http://127.0.0.1:8080/v1/collections
curl -s 'http://127.0.0.1:8080/v1/explorer/position?collection=masters-otb&moves=e4,e5,Nf3'
curl -s 'http://127.0.0.1:8080/v1/explorer/position?collection=elite-online&moves=e4,e5,Nf3'
curl -s 'http://127.0.0.1:8080/v1/collections/masters-otb/games/<stable-id>'   # PGN
./target/release/codb query --dataset data/releases/masters-otb --moves "e4,e5,Nf3" --json  # must match HTTP
```

Stable game IDs are listed in `games.idx` (32-byte header, 16-byte records, 32-byte footer; see [DATA_CONTRACTS.md](DATA_CONTRACTS.md#23-global-game-lookup-gamesidx)); `corpus.sh http-check` samples them.

### 8.4 Automated HTTP validation

```bash
./scripts/corpus.sh http-check      # starts a server, runs the checks, writes data/reports/http_check.json
```

It asserts, per collection and against the real release: collection identity and `dataset_version` equal to the manifest; `/v1/collections` metadata; W+D+L reconciliation at the position and per candidate move; candidate moves are legal (each is replayed through the API); HTTP equals `codb query` on a stratified position sample (common, mid-frequency, rare, beyond-depth, absent, and transposition pairs); transposed move orders return identical statistics; invalid FEN, illegal moves, ambiguous or missing positions, and unsupported parameters are rejected with 400; real stored games are retrieved and equal `codb get-game`; ids from one collection return 404 in the other; conditional requests return 304; and latency, concurrency (1–32 workers), and startup time are measured.

## 9. Measured Results (this delivery)

MEASURED_RESULTS_PLACEHOLDER

## 10. Local Browser Integration (Evalis development)

* **Origin:** `http://127.0.0.1:8080` (change with `--host`/`--port`).
* **CORS:** same-origin only by default. For a dev server on another origin pass `--allowed-origins http://localhost:3000` (comma separated; `*` allows any origin). Exposed headers: `ETag`, `Content-Type`, `Cache-Control`. Allowed methods: `GET`, `POST`, `OPTIONS`, `HEAD`.
* **Collection identifiers:** `masters-otb`, `elite-online`. Never pool results across collections.
* **Position query:** `GET /v1/explorer/position?collection=<id>&moves=e4,e5,Nf3` or `&fen=<url-encoded FEN>`. Response fields: `collection`, `dataset_version`, `dataset_freshness`, `position_key`, `fen`, `total_games`, `white_wins`, `draws`, `black_wins`, `*_pct`, and `moves[]` (`uci`, `san`, per-result counts, `total_games`, `*_pct`) sorted by frequency. A legal position absent from the index returns `200` with zero counts and an empty `moves` array. Positions deeper than the index depth (40 plies Masters, 30 plies Elite) are absent by construction.
* **Games:** `GET /v1/collections/{collection}/games/{id}` returns canonical PGN (`application/x-chess-pgn`).
* **Unavailable:** `since_year` and `min_rating` filters (HTTP 400 `unsupported_parameter`), opening names/ECO classification, per-position representative games, authentication other than the admin reload token.
* **Failures:** JSON error envelope with stable codes (`invalid_collection`, `invalid_fen`, `invalid_moves`, `missing_position`, `ambiguous_position`, `move_sequence_too_long`, `unsupported_parameter`, `invalid_game_id`, `game_not_found`, `reader_pool_timeout`, …); 404 for unknown game ids; 503 from `/ready` when no collection is loaded. See [DATA_CONTRACTS.md](DATA_CONTRACTS.md#6-opening-explorer-http-api-contract).

## 11. License Obligations

* **Masters OTB (CC BY-SA 4.0):** any public distribution or deployment must show the attribution in `RELEASE/DATASET_LICENSE.txt`, state that the data was modified (filtered, deduplicated, re-encoded, aggregated), and license redistributed adaptations under CC BY-SA 4.0 or a compatible license.
* **Elite Online (CC0 1.0):** no attribution is required; provenance is preserved in the release.
* **Software:** `MIT OR Apache-2.0`; independent of the dataset licenses. The release notice says so and makes no claim over material outside the listed archives.
* Keep `RELEASE/` with any redistributed copy. This is not legal advice (see [LICENSING.md](LICENSING.md#43-release-artifacts-and-notices)).

## 12. Failure Recovery

| Symptom | Meaning | Action |
| :--- | :--- | :--- |
| `giving up after N retries` | Transient network failure; `.part` kept | Rerun `fetch`; it resumes. |
| `sha256 … does not match publisher checksum` | Corrupted transfer or changed upstream | Partial file deleted; rerun. If it recurs, the upstream archive changed: re-`pin` and review before use. |
| `insufficient disk space` | Free space below need + headroom | Free space or pass `--data-dir` on a larger volume. |
| `source … is not verified` at build | Archive missing/tampered | `fetch` again; the build never proceeds. |
| `step '<name>' failed` | A `codb` step failed | Active release untouched; inspect `data/work/<run>/logs/<name>.log`, fix, rerun, then delete the stale workspace. |
| `post-promotion verification failed` | Published copy failed deep verify | The previous release was restored automatically; inspect `data/work/<run>/failed-release`. |
| Interrupted `build` | Workspace left under `data/work/` | Safe to delete; the release directory is only changed by the final rename. |
| Crash during `codb build` inside a dataset root | Staging directory under `versions/.staging_*` | `codb recover --dataset <dir>`; or discard the workspace. |

## 13. Reproducibility Status

* **Sources:** pinned by URL, size, and publisher SHA-256; rebuilds from the same archives are possible as long as Lichess keeps serving them. Lichess has historically not rewritten published monthly archives, but this is not a guarantee.
* **Substantive determinism:** equal archives, order, and build settings give identical accepted games, game IDs, accounting, and CPI hash (asserted by `scripts/test_corpus.py` on fixtures; real-corpus rebuild comparison in §9).
* **Not bitwise identical:** `dataset_build_id`, `build_timestamp`, and therefore `manifest.json` and the version directory name differ per build; shard bytes embed those identifiers' siblings only through the manifest, so shard hashes are compared separately in §9. Full-archive bitwise determinism is **not** claimed.

## 14. Limitations and Remaining Work

* The corpus is a **baseline**, not the 5M Masters / 10M Elite goal; that remains future work ([ROADMAP.md](ROADMAP.md)).
* Masters coverage is six recent monthly broadcast archives; earlier periods and non-Lichess sources need separate provenance review.
* Elite coverage is four complete 2016 months, a different era and rating distribution than present-day Lichess play.
* The Masters OTB qualification is heuristic (see §4.2).
* Releases are local artifacts; publication to an approved host is future work.
* `codb build` RSS grows with the amount of input processed (see §9) and exceeds the older "< 30 MB ingestion" figure; see [BENCHMARKS.md](BENCHMARKS.md).
