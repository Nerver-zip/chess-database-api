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

| Measure | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Archives | 6 | 4 |
| Network transfer (input archives, one-time) | 142.3 MB | 5,047.9 MB |
| Input archive storage | 142.3 MB | 5,047.9 MB |
| Games encountered | 227,489 | 28,184,178 |
| Accepted unique games | 42,612 | 313,530 |
| Persistent CGB3 shards | 8.7 MB (204.4 B/game) | 52.1 MB (166.0 B/game) |
| `games.idx` | 0.7 MB | 5.0 MB |
| `positions.cpi` | 21.2 MB (497.9 B/game) | 99.4 MB (316.9 B/game) |
| Final deployed size (current version, incl. manifest) | 30.6 MB (718.3 B/game) | 156.4 MB (499.0 B/game) |
| Peak temporary workspace (dataset + spill + dedupe DB, any step) | 63.1 MB | 319.0 MB |
| Total rebuild wall time (build + index + verify + report) | 29.8 s | 479.6 s |

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

All values below were measured on the delivery machine (4 cores, 15 GiB RAM, Linux, `cargo` 1.100, Rust release build, release commit recorded in `RELEASE.json`) and are copied from the committed evidence in `benchmarks/corpus/` (validated by `scripts/validate_corpus_evidence.py`).

**Sources actually used** (every archive downloaded in full over HTTPS and verified against the publisher's SHA-256 list; 5,190.2 MB transferred in total):

| Source id | Collection | License | Size (bytes) | SHA-256 (publisher = verified) |
| :--- | :--- | :--- | ---: | :--- |
| `lichess-broadcast-2026-05` | masters-otb | CC-BY-SA-4.0 | 22,912,287 | `d212987f7972a70268809ff6c726acff8bf0987161aacd7647b160f8564f6a53` |
| `lichess-broadcast-2026-06` | masters-otb | CC-BY-SA-4.0 | 22,797,518 | `65905d611c0d78d90ab57cd9edd3821f4119b46c22b5489fd7e265a74c6767b3` |
| `lichess-broadcast-2026-07` | masters-otb | CC-BY-SA-4.0 | 24,535,400 | `714d0eb99f99fca8d791142038b6c59b5ca6a51b3339bd3891a92f4bdffcbf0c` |
| `lichess-broadcast-2026-08` | masters-otb | CC-BY-SA-4.0 | 31,010,232 | `e227c35c3207ebade754849c1825982e2717ae82c013013d896778e27be77724` |
| `lichess-broadcast-2026-09` | masters-otb | CC-BY-SA-4.0 | 18,414,369 | `27a39d035767a30901b89602f33f2a7b27d4ab5025842071165fa079f8fff1df` |
| `lichess-broadcast-2026-10` | masters-otb | CC-BY-SA-4.0 | 22,630,351 | `d9c0c78c7ed8cd200723c9972b01a1cd35fd8568dd4b6324fbcc3ee7c07374c9` |
| `lichess-standard-2016-03` | elite-online | CC0-1.0 | 1,042,258,644 | `11e8667a82a02199855f3aca83d6d4091d194dc4becf6d02a8001f0d9623b5c6` |
| `lichess-standard-2016-06` | elite-online | CC0-1.0 | 1,093,562,696 | `97ccde214a81d5e58867bedbeb510dfdd99418d9fb31b0995f5320a74b7d4502` |
| `lichess-standard-2016-09` | elite-online | CC0-1.0 | 1,213,809,361 | `3a4d18db787db558083aacb7c27ab3588f0745c1baaca37846b3f0d8b9677db3` |
| `lichess-standard-2016-12` | elite-online | CC0-1.0 | 1,698,298,025 | `285d09bcb7f47af2d58594ecee7da8c18f9e800fd7df2e0158b4fb5429f9a904` |

### 9.1 Corpus composition

| | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Accepted unique games | **42,612** (target ≥ 30,000: met) | **313,530** (target ≥ 100,000: met) |
| Index depth | 40 plies | 30 plies |
| Unique indexed positions | 984,240 | 4,615,463 |
| Position→move records | 1,040,109 | 5,056,633 |
| Unknown-result games (excluded from W/D/L) | 310 | 85 |
| Distinct players (exact-name estimate) | 8,119 | 13,864 |
| Distinct events (exact-name estimate) | 26,503 | 2,982 |

Accounting (reconciles exactly; `is_reconciled` and `retained_matches_accepted` are true):

* `masters-otb`: encountered 227,489 = accepted 42,612 + duplicates 29 + superseded 11 + policy 183,102 + variant 1,735 + illegal 0 + malformed 0 + quarantined 0.
* `elite-online`: encountered 28,184,178 = accepted 313,530 + duplicates 42 + superseded 12 + policy 27,870,594 + variant 0 + illegal 0 + malformed 0 + quarantined 0.

Policy rejections by reason: `masters-otb`: `below_rating_threshold` 155,153, `missing_rating` 24,823, `online_event_in_masters_source` 2,424, `too_few_plies` 702. `elite-online`: `below_rating_threshold` 19,993,852, `bullet_or_ultrabullet` 7,875,574, `too_few_plies` 1,168.

Game dates by month: `masters-otb`: 2025-09: 3, 2025-10: 3, 2026-01: 6, 2026-03: 31, 2026-04: 137, 2026-05: 6,660, 2026-06: 8,200, 2026-07: 6,199, 2026-08: 9,488, 2026-09: 5,320, 2026-10: 6,542, 2026-11: 23. `elite-online`: 2016-02: 112, 2016-03: 64,592, 2016-05: 248, 2016-06: 93,918, 2016-08: 200, 2016-09: 80,212, 2016-11: 82, 2016-12: 74,166. (Broadcast games can carry a date earlier or later than the archive month; Elite games dated outside the four archive months are Lichess records whose `UTCDate` differs from the archive window.)

Rating distribution (lower player rating, 100-point bins) is in each `quality_report.json`; `masters-otb` ratings are FIDE Elo and `elite-online` ratings are Lichess Glicko-2 and are never pooled. Elite speed classes (`blitz`/`rapid`/`classical`/`unknown`): blitz 283,953, classical 754, rapid 28,367, unknown 456. For Masters the `TimeControl` tag has no consistent unit and is missing for many games, so only raw values are reported.

### 9.2 Build performance (one pass, measured by the orchestrator via kernel `rusage` and workspace sampling)

`masters-otb`:

| Step | Wall time | Peak RSS | Peak workspace disk |
| :--- | ---: | ---: | ---: |
| build | 26.65 s | 44.5 MB | 18.6 MB |
| build-index | 2.68 s | 62.8 MB | 63.1 MB |
| verify | 0.30 s | 29.2 MB | 34.9 MB |
| report | 0.12 s | 29.2 MB | 34.9 MB |

`elite-online`:

| Step | Wall time | Peak RSS | Peak workspace disk |
| :--- | ---: | ---: | ---: |
| build | 463.48 s | 67.6 MB | 122.3 MB |
| build-index | 13.72 s | 133.5 MB | 319.0 MB |
| verify | 1.56 s | 104.6 MB | 161.3 MB |
| report | 0.62 s | 29.6 MB | 161.3 MB |

Source processing throughput (`codb build` step): Masters 8,536 games/s and 5.3 MB/s of compressed input; Elite 60,811 games/s and 10.9 MB/s of compressed input. CGB3 build and CPI build durations are the `build` and `build-index` rows above.

**Memory, stated precisely.** Ingestion, indexing, and serving use different amounts of memory and the figures are not interchangeable:

* *Ingest/build (`codb build`)*: peak RSS **44.5 MB** (Masters) and **67.6 MB** (Elite). This is **above the "< 30 MB ingestion" figure** quoted in earlier documents. `codb build` holds the deduplication SQLite cache (configured at 32 MB) and the staging-catalog cache (4 MB) on top of the process baseline (~12 MB); a single-archive build peaked at 22.5 MB, so the peak grows with accepted games until the caches fill. It stays bounded (the ADR-0002 deduplication ceiling is < 64 MB), but the Elite peak slightly exceeds that sum. Nothing here changed that behavior; see [DECISIONS_AND_OPEN_QUESTIONS.md](DECISIONS_AND_OPEN_QUESTIONS.md) (Q-04).
* *Index (`codb build-index`)*: peak RSS **62.8 MB** (Masters) and **133.5 MB** (Elite) with `--memory-limit-mib 128`. The limit bounds in-memory position buffering; process RSS also includes the runtime baseline and read buffers.
* *Serve (`codb serve`, both collections loaded)*: peak RSS **144.6 MB** after the HTTP workload (`VmHWM`). This includes file-backed pages of the memory-mapped `positions.cpi` and `games.idx` files that were touched (shared page cache), not private heap.

### 9.3 Query and API performance (`corpus.sh http-check`, quiet machine, keep-alive HTTP/1.1, Python client)

| | `masters-otb` | `elite-online` |
| :--- | :--- | :--- |
| Position query latency (1,000 sequential mixed requests) | p50 210 µs / p95 455 µs / p99 490 µs | p50 196 µs / p95 300 µs / p99 350 µs |
| Game retrieval latency (200 requests) | p50 1346 µs / p95 1576 µs / p99 1852 µs | p50 1077 µs / p95 1207 µs / p99 1326 µs |
| Throughput by client concurrency (req/s; workers 1,4,8,16,32) | 1:5303, 4:6112, 8:6265, 16:6178, 32:6521 | 1:4903, 4:6322, 8:6870, 16:5730, 32:6450 |
| Errors under concurrency | 0 | 0 |

API startup (process start to `/ready`, both collections): **1.36 s**. The Python client is single-process and limits the concurrency numbers; they are lower bounds on server capacity. In-process CPI lookups on the Masters dataset (`codb bench-dataset`, copy of the release, warm page cache, 2,000 stratified queries): p50 14.0 µs, p95 20.6 µs, p99 31.3 µs (`cpi_disk_*`); random CGB3 game retrieval p50 852 µs, p99 1,075 µs (100% correct). For Elite the same benchmark's random game retrieval measured p50 624 µs, p95 733 µs, p99 1,447 µs (100% correct); its depth-indexing comparison phase was stopped after the retrieval measurement because it re-indexes the whole corpus four times with a SQLite baseline and was not needed for the delivery gates, so no in-process Elite CPI lookup figure exists; HTTP latencies above are the position-lookup evidence for Elite.

### 9.4 Correctness evidence

`http-check` ran **414 checks with 0 failures**. Per collection it compared HTTP to `codb query` on **131 stratified positions** with **0 mismatches** (strata: 40 common, 40 mid-frequency, 40 rare, 3 beyond-depth, 1 absent, 3 transposition pairs ×2 orders, plus the initial position); identical counts for Elite. Beyond-depth and absent positions return zero statistics. `codb verify --deep` passed for both releases and for the extracted packages.

### 9.5 Reproducibility measured on the real corpus

`masters-otb` was built three times from the same six archives and `elite-online` twice from the same four archives (the first builds ran from a working tree with uncommitted changes; the final builds ran from a clean commit). In every case `games.idx`, `positions.cpi`, and every `shard_*.cgb3` file were byte-identical (identical SHA-256) and the accounting was identical; only `manifest.json` (build id and timestamp) and the version directory name differed. Complete-directory bitwise determinism is **not** claimed because build ids and timestamps differ per build.

### 9.6 Storage feasibility against the earlier projection

Measured bytes per unique indexed position are 21.6 B (Masters, depth 40) and 21.5 B (Elite, depth 30); per accepted game the persistent CGB3+`games.idx` cost is 220.4 B (Masters) and 182.0 B (Elite). The Heaps' Law fit in [BENCHMARKS.md](BENCHMARKS.md#5-mathematical-heaps-law-scaling--feasibility-projections) predicted 1,000,346 (Masters) and 4,821,991 (Elite) unique positions at these sizes against 984,240 and 4,615,463 measured (−1.6% and −4.3%). Applying the measured bytes to the same model at 5M Masters / 10M Elite games gives roughly 7.2–7.3 GB deployed, consistent with (about 5% below) the existing 7.68 GB baseline and within the 10 GB envelope. This is an extrapolation of 118× (Masters) and 32× (Elite) from the measured sizes and assumes the fitted exponent holds; it is not a measurement, and no approved source exists to build 5M Masters games. Storage is not redesigned on the basis of these numbers.

Distinct storage figures that must not be conflated: **final deployed storage** (current version, above); **downloaded source archives** (5,190.2 MB for both baselines, can be deleted after a build); **temporary build space** (peak workspace row above, released after promotion); **simultaneously retained versions** — the pre-index version directory shares shards and `games.idx` by hard link with the indexed version (no duplicated payload; only the small `manifest.json` and the CPI differ), and a rebuild retains the entire previous release under `data/releases/.retired/` until you delete it, i.e. up to 2× the deployed size on disk during and after a rebuild.

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
* **Not bitwise identical as a whole:** `dataset_build_id` and `build_timestamp` differ per build, so `manifest.json` and the version directory name differ. Measured on the real corpus (§9.5), `games.idx`, `positions.cpi`, and every shard file were byte-identical across rebuilds. Complete-directory bitwise determinism is **not** claimed.

## 14. Limitations and Remaining Work

* The corpus is a **baseline**, not the 5M Masters / 10M Elite goal; that remains future work ([ROADMAP.md](ROADMAP.md)).
* Masters coverage is six recent monthly broadcast archives; earlier periods and non-Lichess sources need separate provenance review.
* Elite coverage is four complete 2016 months, a different era and rating distribution than present-day Lichess play.
* The Masters OTB qualification is heuristic (see §4.2).
* Releases are local artifacts; publication to an approved host is future work.
* `codb build` peak RSS exceeds the older "< 30 MB ingestion" figure (§9.2) and grows with accepted games until the deduplication cache fills; see [DECISIONS_AND_OPEN_QUESTIONS.md](DECISIONS_AND_OPEN_QUESTIONS.md) (Q-04).
* For ~69% of accepted Masters games the PGN `Site` is only a broadcast URL, so no physical venue is recorded; Masters provenance rests on the publisher (Lichess broadcast organizers) and the marker heuristic.
