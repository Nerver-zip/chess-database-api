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
| Archives | 6 | 7 |
| Network transfer (input archives, one-time) | 142.3 MB | 8,522.4 MB |
| Input archive storage | 142.3 MB | 8,522.4 MB |
| Games encountered | 227,489 | 46,536,762 |
| Accepted unique games | 42,612 | 429,150 |
| Persistent CGB3 shards | 8.7 MB (204.4 B/game) | 71.4 MB (166.3 B/game) |
| `games.idx` | 0.7 MB | 6.9 MB |
| `positions.cpi` | 21.2 MB (497.9 B/game) | 132.1 MB (307.7 B/game) |
| Final deployed size (current version, incl. manifest) | 30.6 MB (718.9 B/game) | 210.3 MB (490.1 B/game) |
| Peak temporary workspace (dataset + spill + dedupe DB, any step) | 64.4 MB | 432.6 MB |
| Total build, index, verify, and report wall time | 34.0 s | 677.6 s |


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
| `elite-online` | Lichess standard rated games, `database.lichess.org/standard/` | CC0 1.0 | 2015-09, 2015-12, 2016-03, 2016-06, 2016-09, 2016-12, 2017-03 (seven complete monthly archives) |

No other source is approved. TWIC, PGN Mentor, Caissabase, KingBase, commercial databases, and community mirrors are excluded because their redistribution rights or upstream provenance cannot be established ([LICENSING.md](LICENSING.md)).

### 4.2 Source qualification and its limits

* **Masters OTB.** Broadcast archives contain both over-the-board and online events and have no explicit OTB flag. Qualification is executed by `MastersOtbPolicy`: FIDE-rating thresholds, standard chess only, minimum length, online-event marker exclusion (event, site, broadcast URL), pre-1970 provenance requirements, and explicit `Termination` exclusions. The catalog marks these sources `provenance_confidence: medium`. The residual risk (an online event without any marker) is not eliminated and is disclosed in the release notice. `time_control_raw_top20` in the quality report shows raw `TimeControl` strings, whose units are not consistent across broadcasts.
* **Elite Online.** The complete monthly archives are SHA-256 verified end to end. Ratings are Lichess Glicko-2 and are never pooled with FIDE Elo. Existing rated, variant, length, and time-control checks are enforced; explicit aborted, abandoned, no-contest, unplayed, unterminated, and rules-infraction `Termination` values are rejected. The months (2015-09 to 2017-03) were chosen because complete archives of that era are small enough to download in full and verify (0.5–2.2 GB each); modern monthly archives are tens of gigabytes. They span three calendar years at roughly quarterly spacing, but the corpus reflects 2015–2017 Lichess play and rating levels; recent play is not represented. A partial or truncated stream of a larger archive would **not** be a verified archive and is never produced by this tool.

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

All values were measured on the delivery machine (4 cores, 15 GiB RAM, Linux, Rust 1.100 release build, commit recorded in `RELEASE.json`, clean working tree) and are copied from the committed evidence in `benchmarks/corpus/` (validated by `scripts/validate_corpus_evidence.py`).

**Sources actually used** (every archive downloaded in full over HTTPS and verified against the publisher's SHA-256 list; 8,664.7 MB transferred in total):

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
| `lichess-standard-2015-09` | elite-online | CC0-1.0 | 547,459,020 | `c3f9b718b2dd80545ac00d0ee8122f863353a7b901fea6ba0be06bff5675bfa7` |
| `lichess-standard-2015-12` | elite-online | CC0-1.0 | 757,824,089 | `83c6a8583e618d1321cb655e579010ccb1531593fbcf44622da5e8a5d2e99c4f` |
| `lichess-standard-2017-03` | elite-online | CC0-1.0 | 2,169,142,067 | `b319297e9270285635d615cbcd19a9b0d44747be3ff91d5b3eac41175b794320` |

### 9.1 Corpus composition

| | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Accepted unique games | **42,612** (target ≥ 30,000: met) | **429,150** (target ≥ 100,000: met) |
| Index depth | 40 plies | 30 plies |
| Unique indexed positions | 984,240 | 6,136,307 |
| Position→move records | 1,040,109 | 6,739,644 |
| Unknown-result games (excluded from W/D/L) | 310 | 0 |
| Distinct players (exact-name estimate) | 8,119 | 17,573 |
| Distinct events (exact-name estimate) | 26,503 | 4,004 |
| Deployed release directory bytes (`du -sb`) | 30,634,943 | 210,312,125 |

Accounting reconciles exactly (`is_reconciled` and `retained_matches_accepted` are true):

* `masters-otb`: 227,489 encountered = 42,612 accepted + 29 duplicates + 11 superseded + 183,110 policy + 1,727 variant + 0 illegal + 0 malformed + 0 quarantined.
* `elite-online`: 46,536,762 encountered = 429,150 accepted + 45 duplicates + 16 superseded + 46,107,551 policy + 0 variant + 0 illegal + 0 malformed + 0 quarantined.

Policy rejections by reason: `masters-otb`: `aborted_or_infraction` 3,052, `below_rating_threshold` 152,977, `missing_rating` 24,285, `online_event_in_masters_source` 2,424, `too_few_plies` 372. `elite-online`: `aborted_or_infraction` 228,742, `below_rating_threshold` 32,542,858, `bullet_or_ultrabullet` 13,334,669, `missing_rating` 20, `too_few_plies` 1,262.

**Source coverage by game date.** `masters-otb`: 2025-09: 3, 2025-10: 3, 2026-01: 6, 2026-03: 31, 2026-04: 137, 2026-05: 6,660, 2026-06: 8,200, 2026-07: 6,199, 2026-08: 9,488, 2026-09: 5,320, 2026-10: 6,542, 2026-11: 23. `elite-online`: 2015-08: 14, 2015-09: 7,219, 2015-11: 24, 2015-12: 20,398, 2016-02: 112, 2016-03: 64,583, 2016-05: 248, 2016-06: 93,886, 2016-08: 200, 2016-09: 80,197, 2016-11: 82, 2016-12: 74,126, 2017-02: 107, 2017-03: 87,954. Elite source coverage is seven complete monthly archives across 2015–2017. Dates outside an archive month occur in Lichess PGNs; qualifying games from each archive are processed in full, without first-N sampling or a cap. Contributions are uneven, reflecting archive size and historical rating levels; no per-period balancing is applied. Broadcast PGNs may also contain dates outside their archive month.

**Missing metadata and provenance limits.** Masters: time control is missing for 17,295 games (and uses inconsistent units when present); event and site/source ID are present, but about 69% of accepted games have only a broadcast URL in `Site`, not a physical venue. Elite: time control is missing for 712 games. Ratings are FIDE Elo for Masters and Lichess Glicko-2 for Elite; they are never pooled. Elite speed classes are blitz 393,240, classical 929, rapid 34,269, unknown 712. Masters reports only raw `TimeControl` values.

### 9.2 Build performance (Linux, 4 cores; one measured pass)

| Collection | Step | Wall time | Peak RSS | Peak workspace disk |
| :--- | :--- | ---: | ---: | ---: |
| Masters | build | 30.912 s | 29.51 MB | 18.71 MB |
| Masters | build-index | 2.670 s | 62.13 MB | 64.42 MB |
| Masters | verify | 0.309 s | 29.51 MB | 34.87 MB |
| Masters | report | 0.105 s | 29.51 MB | 34.88 MB |
| Elite | build | 656.415 s | 29.73 MB | 163.91 MB |
| Elite | build-index | 18.225 s | 150.68 MB | 432.58 MB |
| Elite | verify | 2.122 s | 137.23 MB | 215.60 MB |
| Elite | report | 0.870 s | 29.73 MB | 215.61 MB |

**Memory, stated precisely.** `codb build` peaked at 29,507,584 bytes (29.51 MB) for Masters and 29,732,864 bytes (29.73 MB) for Elite, both below the 30,000,000-byte ingestion ceiling. Elite had 267,136 bytes of measured headroom on this host. To reach that result, the deduplication SQLite cache was reduced from 32 MiB to 256 KiB, its temp store uses disk, and the staging-catalog cache was reduced from 4 MiB to 512 KiB. These are measurements on the delivery machine; RSS can vary by OS and allocator.

The external position indexer was run with `--memory-limit-mib 128`. Its process RSS was 62.13 MB (Masters) and 150.68 MB (Elite); that option bounds the indexer's configured observation buffer, not total RSS including runtime, mappings, and other process memory. `codb serve` peaked at 183,472,128 bytes (`VmHWM`) during HTTP checks, including touched memory-mapped pages of `positions.cpi` and `games.idx` (shared page cache, not private heap).

### 9.3 Query and API performance

`corpus.sh http-check` used keep-alive HTTP/1.1. Sequential requests use one client process; concurrent load uses one OS process per client. The four-core host was shared by client and server.

| Measure | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Position queries (1,000 requests), p50 / p95 / p99 | 190.0 / 601.5 / 631.5 µs | 180.4 / 288.7 / 324.2 µs |
| Game retrieval (200 requests), p50 / p95 / p99 | 1,400.5 / 1,797.7 / 2,223.1 µs | 1,083.7 / 1,164.8 / 1,242.1 µs |
| Startup to `/ready`, both datasets | 1.715 s | 1.715 s |
| Concurrent throughput, 1 / 4 / 8 / 16 / 32 client processes | 3,220 / 8,765 / 8,912 / 3,805 / 3,436 req/s | 4,460 / 10,452 / 6,763 / 5,551 / 3,089 req/s |
| Errors under concurrency | 0 | 0 |

At 16–32 client processes the shared four-core host is oversubscribed, increasing tail latency; these are single-machine measurements, not dedicated-host capacity claims.

In-process lookups (`codb bench-lookup`, `DiskPositionIndex::query`; 5,000 sampled games, warm page cache; excludes HTTP, JSON, and SAN):

| Measure | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Hit lookups, p50 / p95 / p99 | 5.00 / 10.53 / 10.96 µs (68,145 lookups) | 5.60 / 11.35 / 12.61 µs (49,686 lookups) |
| Miss lookups, p50 / p95 / p99 | 4.40 / 5.01 / 7.29 µs (9,735 lookups) | 4.56 / 5.23 / 7.75 µs (7,098 lookups) |

### 9.4 Correctness evidence

`http-check` ran **414 checks with 0 failures**. It compared HTTP with `codb query` for **131 stratified positions per collection** (40 common, 40 mid-frequency, 40 rare, 3 beyond-depth, 1 absent, and transposition pairs in both orders): 262 comparisons, 0 mismatches. W/D/L and candidate totals reconcile, candidate moves are legal, invalid inputs return the documented errors, and cross-collection game IDs return 404. `codb verify --deep` passed for both releases and for both extracted packages.

A direct local HTTP capture is stored in [`benchmarks/corpus/http_examples.json`](../benchmarks/corpus/http_examples.json). For `moves=e4,e5,Nf3`, Masters returned 5,685 games (W 1,872 / D 2,541 / B 1,272; top moves Nc6 5,044, Nf6 574, d6 64); Elite returned 30,390 (W 14,325 / D 2,404 / B 13,661; top moves Nc6 23,951, Nf6 3,291, d6 1,799). In both cases the HTTP response matched `codb query --json`. Stored games `9216809156860181735` (Masters) and `9226102694647866711` (Elite) returned HTTP 200 with `application/x-chess-pgn`. A post-build scan streamed all 42,612 accepted Masters PGNs and found zero event, site, or broadcast-link values matching the policy's online-event markers; the scan result is in [`online_marker_audit.json`](../benchmarks/corpus/masters-otb/online_marker_audit.json). This does not rule out online events without any marker; source confidence remains medium.

### 9.5 Reproducibility measured on the real corpus

An independent second build from each release's pinned `sources.manifest.json` produced byte-identical `games.idx`, `positions.cpi`, and `shard_0000.cgb3` files. Both repeat builds reconciled accounting and passed `codb verify --deep`; exact hashes and elapsed times are in [`benchmarks/corpus/reproducibility_check.json`](../benchmarks/corpus/reproducibility_check.json). The build IDs, timestamps, release manifests, and catalog metadata are expected to differ; complete-directory bitwise identity is not claimed. The later `2026.11-r2` change updates only the policy descriptors and catalog revision; the filtering logic used for both builds was already present.

### 9.6 Storage feasibility against the earlier projection

Measured CPI density is 21.55 B per unique position (Masters, depth 40) and 21.52 B (Elite, depth 30). CGB3 plus `games.idx` costs 220.40 B per accepted Masters game and 182.28 B per Elite game. With the historical Heaps parameters (`k=49.88` / `37.63`, `α=0.9293`), predicted current-size position counts are 1,000,346 / 6,455,321 versus 984,240 / 6,136,307 measured (-1.6% / -4.9%).

Applying the current measured byte densities to that same model at 5M Masters and 10M Elite games projects about **7.76 GB deployed**, including the historical 0.44 GB allowance for ancillary files and alignment. This is an extrapolation from much smaller historical runs (not a measured release); no approved source currently supplies the 5M Masters target. The r2 release directories occupy 30,634,943 bytes (Masters) and 210,312,125 bytes (Elite); independently verified local package tar files occupy 30,648,320 and 210,329,600 bytes. No dataset or package was uploaded externally.

Distinct storage figures: final deployed release size; downloaded source archives (8,664,654,059 bytes total, removable after a build); peak temporary workspace (table above); and retained old versions. The release workflow keeps the prior release under `data/releases/.retired/` until manually removed, so a rebuild can temporarily require up to roughly twice the deployed size.

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
| `database or disk is full` in standalone `codb build` | SQLite temporary files may be using a full or size-limited system temp directory | Set `TMPDIR` to a writable directory with enough space. `corpus.sh build` already scopes temporary files to `data/work/<run>/tmp`. |
| `post-promotion verification failed` | Published copy failed deep verify | The previous release was restored automatically; inspect `data/work/<run>/failed-release`. |
| Interrupted `build` | Workspace left under `data/work/` | Safe to delete; the release directory is only changed by the final rename. |
| Crash during `codb build` inside a dataset root | Staging directory under `versions/.staging_*` | `codb recover --dataset <dir>`; or discard the workspace. |

## 13. Reproducibility Status

* **Sources:** pinned by URL, size, and publisher SHA-256; rebuilds from the same archives are possible as long as Lichess keeps serving them. Lichess has historically not rewritten published monthly archives, but this is not a guarantee.
* **Substantive determinism:** equal archives, order, and build settings give identical accepted games, game IDs, accounting, and CPI hash (asserted by `scripts/test_corpus.py` on fixtures; real-corpus rebuild comparison in §9).
* **Not bitwise identical as a whole:** `dataset_build_id` and `build_timestamp` differ per build, so `manifest.json` and the version directory name differ. Measured on the real corpus (§9.5), `games.idx`, `positions.cpi`, and every shard file were byte-identical across rebuilds. Complete-directory bitwise determinism is **not** claimed.

## 14. Limitations and Remaining Work

* This is a real, useful baseline, not the future 5M Masters / 10M Elite goal ([ROADMAP.md](ROADMAP.md)).
* Masters coverage is six recent monthly Lichess broadcast archives. OTB qualification remains a medium-confidence marker heuristic; broadcast archives can include online events, and the pipeline cannot prove that every accepted record is OTB. About 69% of accepted Masters games have no physical venue in `Site`.
* Elite coverage consists of seven complete monthly archives from 2015-09 to 2017-03. It spans multiple years but reflects old Lichess play and rating levels; recent games are absent and no per-period balancing is applied.
* `ProvisionalRating` cannot be filtered from the standard PGN export because it contains no provisional-rating marker. Explicit aborted, abandoned, no-contest, unplayed, unterminated, and rules-infraction `Termination` values are rejected; unknown termination values remain eligible.
* The measured Elite ingestion peak is 29.73 MB, 267,136 bytes below the 30,000,000-byte ceiling on this host. Re-measure after changing Rust, allocator, or runtime versions.
* The 128 MiB index setting bounds the configured observation buffer; total process RSS is higher due to runtime and mapped data. HTTP concurrency measurements share the same 4-core host as the client.
* Releases and verified package archives remain local. Publishing requires a separately approved destination and retaining the license notice.
