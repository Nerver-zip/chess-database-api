---
summary: "Step-by-step reproducible procedure from pinned source catalog to verified Masters OTB and Elite Online releases served over HTTP, with measured results, resource needs, license obligations, and failure recovery."
read_when: "Building, verifying, packaging, or serving the real corpus; integrating a client (for example Evalis) with real opening data; or changing the source catalog."
status: current
---

# Corpus Build & Release Procedure

This document is the operational counterpart of CORPUS_AND_DEDUPLICATION.md (what is accepted), LICENSING.md (what may be redistributed), and ARCHITECTURE.md (where the tooling sits). It covers pinned source builds, event qualification, package bootstrap, and serving through codb or Docker Compose.

The orchestration (`scripts/corpus.sh` → `scripts/corpus.py`) never reimplements ingestion, curation, deduplication, indexing, or publication. The pinned-source workflow uses only the Python standard library; the opt-in local `.7z` extraction command uses the pinned optional dependency in `scripts/requirements-corpus-archive.txt`. The tool invokes `codb build`, `codb build-index`, `codb verify --deep`, and `codb report` for catalog releases.

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
        |  codb publish moves a complete immutable version and atomically switches current
        v
data/releases/<collection>/ { versions/<id>/, current, RELEASE/ }
        |-- corpus.sh package  ->  data/packages/<collection>-<version>.tar + .sha256
        '-- corpus.sh serve    ->  codb serve (both collections)
```

## 2. Prerequisites

| Requirement | Notes |
| :--- | :--- |
| Rust stable toolchain | `cargo build --release -p codb-cli` produces `target/release/codb`. |
| Python 3.11+ | Standard library for the pinned catalog workflow. Install `scripts/requirements-corpus-archive.txt` only when using local `.7z` extraction. |
| Network | HTTPS to `database.lichess.org`. Offline verification (`--offline`, `verify-sources`) needs none. |
| Disk | See §3. The data directory defaults to `./data` (git-ignored); override with `--data-dir` or `CODB_DATA_DIR`. The tool refuses a data directory inside the repository unless it is under a git-ignored directory. |
| Memory | Indexing honors `--memory-limit-mib` (default 128). Measured peaks are in §9. |

`/tmp` may be mounted `noexec` or be small (it is a 512 MiB tmpfs in some sandboxes). The build sets `TMPDIR` to its own workspace under the data directory so deduplication databases and spill files never depend on `/tmp`.

## 3. Resource Estimate (before acquisition)

```bash
./scripts/corpus.sh plan --collection all
```

prints network transfer remaining, input storage, expected accepted games, expected CGB3 and CPI bytes, temporary workspace, the CPI memory limit, and a rebuild-time estimate, computed from the planning figures stored in the catalog (`collections.<c>.planning`). These are estimates, not guarantees. The table below records the delivered releases; Masters is the independently verified OTB subset described in §9.1. Sizes use decimal MB.

| Measure | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Archives | 6 | 7 |
| Network transfer (input archives, one-time) | 142.3 MB | 8,522.4 MB |
| Input archive storage | 142.3 MB | 8,522.4 MB |
| Games encountered | 227,489 | 46,536,762 |
| Accepted unique games | 29,640 | 429,150 |
| Persistent CGB3 shards | 6.0 MB (202.8 B/game) | 71.4 MB (166.3 B/game) |
| `games.idx` | 0.5 MB | 6.9 MB |
| `positions.cpi` | 15.3 MB (515.6 B/game) | 132.1 MB (307.7 B/game) |
| Final deployed version directory | 29,892,368 bytes (1,008.5 B/game) | 210,308,033 bytes (490.1 B/game) |
| Peak temporary workspace (dataset + spill + dedupe DB, any step) | 42.5 MB | 435.7 MB |
| Build, index, verify, and report wall time | 15.3 s | 550.1 s (existing release record) |


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

### 4.2 User-supplied local `.7z` archives

An archive supplied directly by a user can be inventoried and extracted for local evaluation without being added to `corpus/catalog.json`:

```bash
python3 -m pip install -r scripts/requirements-corpus-archive.txt
./scripts/corpus.sh --data-dir data extract-local-7z \
    --archive data/sources/lumbra/OTB_DATABASE.7z \
    --member LumbrasGigabase_OTB_Complete.pgn \
    --output data/sources/lumbra/extracted/LumbrasGigabase_OTB_Complete.pgn \
    --max-expanded-bytes 68719476736 \
    --expected-local-archive-sha256 <locally-recorded-hash> \
    --expected-member-sha256 <locally-recorded-hash>
```

The command requires one regular member with a plain basename, checks the declared expansion size and compression ratio, extracts into a private temporary directory, confirms the actual size, hashes both archive and member, then atomically installs the output and a `*.local-source.json` record. Archive paths, directories, links, multiple members, output overwrites, and size-limit violations fail closed. The two optional hash arguments pin prior **local computations**; they do not become publisher checksums. The record marks `publisher_checksum_verified: false` and `release_eligible: false`. This local-only path does not bypass catalog rules: a source without a publisher checksum or an approved collection license cannot enter a corpus release.

### 4.3 Local Gigabase evaluation package

The separate `scripts/local_gigabase_eval.py` workflow does not add Gigabase to `corpus/catalog.json` or to the approved releases. It verifies the archive and member against local evidence, rebuilds with the opt-in Gigabase policy, and creates a package that is explicitly marked local-evaluation-only. The archive and PGN are never included in that package. All write targets are confined to git-ignored `data/work/gigabase-eval/`; the standard `data/releases/masters-otb` and Elite roots are not changed.

For the build, the workflow writes a one-source manifest in the ignored work root. It pins the extracted PGN to its registered local SHA-256, leaves the source URL null, and labels the license as `Publisher claim: CC BY-NC 4.0; upstream rights unresolved`. It invokes `codb build --manifest` and checks that both the build report and persistent dataset manifest contain exactly that registered filename, local hash, size, URL, and license label. In this local path, `checksum_verified: true` means CODB matched the build-time bytes to the locally pinned manifest hash; it is not publisher authentication. The registration still records no publisher checksum, unresolved upstream rights, and `release_eligible: false`. See [DATA_CONTRACTS.md](DATA_CONTRACTS.md) for the general checksum field contract.

```bash
python3 scripts/local_gigabase_eval.py register \
  --archive data/sources/lumbra/OTB_DATABASE.7z \
  --pgn data/sources/lumbra/extracted/LumbrasGigabase_OTB_Complete.pgn \
  --local-record benchmarks/gigabase/archive_inventory.json

python3 scripts/local_gigabase_eval.py build \
  --registration data/work/gigabase-eval/source-registration.json \
  --output-root data/work/gigabase-eval/candidate \
  --codb target/release/codb

python3 scripts/local_gigabase_eval.py package \
  --root data/work/gigabase-eval/candidate \
  --output-dir data/work/gigabase-eval/packages \
  --codb target/release/codb

python3 scripts/local_gigabase_eval.py bootstrap \
  --package data/work/gigabase-eval/packages/masters-otb-<version>-local-evaluation.tar \
  --previous-root data/releases/masters-otb \
  --target data/work/gigabase-eval/deploy/masters-otb \
  --codb target/release/codb
```

Bootstrap deep-verifies the package, retains the previous snapshot in the isolated evaluation root, atomically activates the Gigabase snapshot, and runs an offline opening query. A rollback republishes the retained previous immutable version through `codb publish`:

```bash
previous_id="$(basename "$(readlink -f data/releases/masters-otb/current)")"
python3 scripts/local_gigabase_eval.py rollback \
  --target data/work/gigabase-eval/deploy/masters-otb \
  --version "$previous_id" \
  --codb target/release/codb
```

For a restricted local position-statistics service, set `CODB_MASTERS_RELEASE_DIR=data/work/gigabase-eval/deploy/masters-otb` and `CODB_ENABLE_GAME_RETRIEVAL=false` before starting Compose. The disabled route does not serve raw PGN; local CLI retrieval remains an operator-only capability. The package is not an approved public or commercial release. The publisher's CC BY-NC 4.0 statement is retained as a publisher claim, publisher checksum is `null`, and unresolved upstream rights are recorded in `RELEASE/RELEASE.json` and `RELEASE/DATASET_LICENSE.txt`.

### 4.4 Source qualification and its limits

* **Masters OTB.** The broadcast archives can contain both OTB and online games. A separate event audit groups accepted baseline games by the broadcast tournament identifier and records official Lichess tournament metadata plus linked event or standings evidence. Verified online requires explicit event-level online or engine evidence. Verified OTB requires an official physical venue and a linked event page that matches both tournament identity and location. Missing Site venue data and high FIDE ratings do not prove play mode. Events that cannot be corroborated are unresolved and excluded from the release. See the complete per-event record in corpus/masters_event_qualification.json and the policy details in CORPUS_AND_DEDUPLICATION.md. The [Lichess Broadcast help](https://lichess.org/broadcast/help) notes that venue and other tournament fields are optional, so absence is not treated as online evidence.
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

writes RELEASE/ (provenance, license notice, reports, per-step wall time, peak RSS, and peak workspace disk) inside the completed immutable snapshot. The Rust storage publisher deeply verifies the candidate, moves it into the stable data/releases/<collection>/versions/<id> directory, then atomically replaces the relative current symlink. It preserves prior version directories and never removes the active path during transition. A failure before the pointer switch leaves the prior active snapshot untouched; a failed post-switch validation atomically restores the prior target. Failed work and any unreferenced promoted candidate are retained for diagnosis.

`--depth-plies` and `--memory-limit-mib` override the defaults; options are recorded in `RELEASE/RELEASE.json`.

### 6.1 Release layout

```text
data/releases/masters-otb/
  current -> versions/<immutable-build-id>
  versions/<immutable-build-id>/
    manifest.json
    games.idx
    positions.cpi
    shards/shard_0000.cgb3
    RELEASE/{RELEASE.json, DATASET_LICENSE.txt, MASTERS_EVENT_QUALIFICATION.json, reports...}
  manifest.json games.idx positions.cpi shards -> current/...
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

### 9.1 Corpus composition and Masters event qualification

The Masters release is the subset supported by deterministic event-level evidence, not the full 42,612-game source-qualified baseline. The audit classifies 29,640 games across 622 events as verified OTB, 1 game across 1 event as verified online, and 12,971 games across 365 events as unresolved. Only verified OTB event IDs enter `masters-otb`; the audited release is 360 games below the earlier 30,000-game planning target. The unresolved set is excluded rather than inferred from missing venue fields or high ratings. The complete event evidence and audit hash are in [`benchmarks/corpus/masters-otb/masters_event_qualification.json`](../benchmarks/corpus/masters-otb/masters_event_qualification.json).

| | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Accepted unique games | **29,640** (event-verified OTB subset) | **429,150** |
| Index depth | 40 plies | 30 plies |
| Unique indexed positions | 705,718 | 6,136,307 |
| Position-to-move records | 744,558 | 6,739,644 |
| Unknown-result games (excluded from W/D/L) | 252 | 0 |
| Distinct players / events (exact-name hash estimates) | 6,487 / 17,954 | 17,573 / 4,004 |
| Time control missing | 11,351 | 712 |
| Deployed version directory bytes | 29,892,368 | 210,308,033 |

Accounting reconciles exactly (`is_reconciled` and `retained_matches_accepted` are true):

* `masters-otb`: 227,489 encountered = 29,640 accepted + 20 duplicates + 5 superseded + 196,097 policy rejections + 1,727 variant + 0 illegal + 0 malformed + 0 quarantined.
* `elite-online`: 46,536,762 encountered = 429,150 accepted + 45 duplicates + 16 superseded + 46,107,551 policy rejections + 0 variant + 0 illegal + 0 malformed + 0 quarantined.

Masters policy rejections include 15,409 unresolved-provenance and 2 verified-online game rejections, alongside the rating, termination, and minimum-ply filters. Elite policy rejections include `aborted_or_infraction` 228,742, `below_rating_threshold` 32,542,858, `bullet_or_ultrabullet` 13,334,669, `missing_rating` 20, and `too_few_plies` 1,262. The original 42,612-game Masters input audit remains in the release evidence, including events excluded from the OTB release.

Masters PGN `Site` values often identify a broadcast rather than a physical venue. Verification therefore uses official Lichess tournament metadata plus a linked event or standings source whose hashed evidence corroborates the tournament identity and venue. Verified-online events require event-level online evidence. Missing `Site` data alone is not evidence of online play. Elite remains based on seven complete monthly Lichess rated archives from 2015–2017; its source IDs, archive checksums, and licenses are preserved in the release manifest.

### 9.2 Build performance and memory (Linux, 4 cores)

RSS for the final Masters release is measured per executed CODB process. The runner reads that process's `/proc` `VmHWM` while it is alive and falls back to `wait4` child resource usage if a short command exits before sampling; neither measurement includes the Python orchestrator. The Elite release predates that measurement correction; its stored `metrics.json` RSS values are retained as historical build records and are not directly comparable. Ingestion remains streaming and RSS is measured per build host; there is no fixed 30 MB release cutoff.

| Collection | Step | Wall time | Peak RSS | Peak workspace disk |
| :--- | :--- | ---: | ---: | ---: |
| Masters | build | 13.195 s | 23,015,424 bytes | 15,380,480 bytes |
| Masters | build-index | 1.840 s | 47,996,928 bytes | 42,479,616 bytes |
| Masters | verify | 0.219 s | 21,082,112 bytes | 26,013,696 bytes |
| Masters | report | 0.081 s | 9,195,520 bytes | 26,021,888 bytes |
| Elite | build | 528.811 s | 30,224,384 bytes recorded by the earlier measurement method | 164,081,664 bytes |
| Elite | build-index | 18.239 s | 152,408,064 bytes recorded in its release | 435,736,576 bytes |
| Elite | verify | 2.182 s | 137,420,800 bytes recorded in its release | 215,597,056 bytes |
| Elite | report | 0.883 s | 30,224,384 bytes recorded by the earlier measurement method | 215,605,248 bytes |

The Elite artifacts were not rebuilt for this follow-up. The values above are the release's archived measurements; process RSS varies by OS, allocator, and runtime. `--memory-limit-mib 128` bounds the indexer's configured observation buffer, not total RSS including runtime and mapped files.

### 9.3 Query and API performance

`corpus.sh http-check --requests 1000 --per-stratum 40 --sample-games 6000` used keep-alive HTTP/1.1. Sequential requests used one client process; concurrent load used one OS process per client. The four-core host was shared by client and server.

| Measure | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Position queries (1,000 requests), p50 / p95 / p99 | 167.9 / 294.9 / 341.5 µs | 190.6 / 308.6 / 349.8 µs |
| Game retrieval (200 requests), p50 / p95 / p99 | 1,354.7 / 1,487.6 / 1,587.3 µs | 1,154.2 / 1,235.2 / 1,268.3 µs |
| Startup to `/ready`, both datasets | 1.664 s | 1.664 s |
| Concurrent throughput, 1 / 4 / 8 / 16 / 32 client processes | 4,110 / 10,671 / 5,020 / 5,186 / 4,692 req/s | 4,294 / 8,127 / 5,745 / 3,549 / 3,190 req/s |
| Errors under concurrency | 0 | 0 |

At 16–32 client processes the shared four-core host is oversubscribed, increasing tail latency; these are single-machine measurements, not dedicated-host capacity claims.

In-process lookups (`codb bench-lookup`, 5,000 sampled games; excludes HTTP, JSON, and SAN):

| Measure | `masters-otb` | `elite-online` |
| :--- | ---: | ---: |
| Warm hit lookups, p50 / p95 / p99 | 4.950 / 10.442 / 17.779 µs | 5.601 / 11.561 / 19.954 µs |
| Warm miss lookups, p50 / p95 / p99 | 4.528 / 5.398 / 9.557 µs | 4.608 / 5.428 / 17.559 µs |

### 9.4 Correctness, package, and Docker evidence

The real HTTP/CLI run passed **414/414 checks** with **262 stratified position comparisons** and no mismatches. It checked health/readiness, both collection identities, legal candidates and W/D/L totals, invalid input errors, cross-collection game isolation, and stored PGN retrieval. For `moves=e4,e5,Nf3`, Masters returned **3,902** games (W 1,294 / D 1,690 / B 918; top moves Nc6 3,501, Nf6 353, d6 46); Elite returned **30,390** (W 14,325 / D 2,404 / B 13,661; top moves Nc6 23,951, Nf6 3,291, d6 1,799). Both HTTP bodies matched `codb query --json`; each collection returned a real stored PGN. Full bodies and PGN hashes are in [`http_examples.json`](../benchmarks/corpus/http_examples.json).

The final Masters and Elite package SHA-256 values are recorded in [`package_checksums.json`](../benchmarks/corpus/package_checksums.json). A fresh Path A bootstrap verified both sidecars, extracted both immutable release trees, and passed `codb verify --deep` for each collection. Path B remains an explicit source rebuild; it downloads the pinned 8.66 GB archive set only when `bootstrap --rebuild` is run.

The real Compose smoke used an independent digest-pinned curl client on a shared Docker network and reached `http://codb:8080`. The image was 98,871,472 bytes; the service ran as UID 10001 with a read-only root filesystem and both data volumes read-only, published no host ports, reached healthy/readiness, and served both real releases. It returned 3,902 Masters and 30,390 Elite games for the opening baseline, retrieved a stored PGN from each, rejected invalid requests with HTTP 400 and an unknown game with 404, and returned 403 for administrative reload while auth was disabled. **96/96** concurrent requests passed; restart preserved results and deep verification; SIGTERM shutdown completed gracefully. The sampled real-container peak was 163,045,376 bytes (159,224 KiB), below the configured 512 MiB container cap. The smoke used read-only named-volume copies because the Docker-in-Docker daemon could not access the workspace bind paths; normal Compose deployment uses read-only host bind mounts. Evidence is in [`docker_real_check.json`](../benchmarks/corpus/docker_real_check.json).

### 9.5 Reproducibility measured on the real corpus

An independent second Masters build and CPI indexing pass from the pinned archives produced byte-identical `games.idx`, `positions.cpi`, and `shard_0000.cgb3` files. The unchanged Elite artifacts match two previously verified independent builds byte-for-byte. Both collections reconcile accounting and pass `codb verify --deep`; release IDs, exact hashes, and elapsed times are in [`reproducibility_check.json`](../benchmarks/corpus/reproducibility_check.json). Build IDs, timestamps, release manifests, and catalog metadata differ as expected; complete-directory bitwise identity is not claimed.

## 9.6 Storage and Scale

The release-specific artifact sizes and measured runtime RSS are recorded with the current evidence in this section. The historical 15-million-game feasibility model is research, not a dataset built or qualified by this release; see [research/feasibility-report.md](research/feasibility-report.md). This follow-up does not expand the corpus beyond the two provisionable releases.

## 10. Container and Browser Access

For the container deployment, the internal API address is http://codb:8080. The default Compose file publishes no host port. A separate Compose project can join the same external Docker network; the exact setup and a loopback-only debug mapping are documented in [DOCKER_DEPLOYMENT.md](DOCKER_DEPLOYMENT.md).

Container-to-container requests do not use browser CORS. A browser needs a reachable HTTP origin and either an explicitly allowed CORS origin or a same-origin reverse proxy. The default service allows no cross-origin browser access. Position and stored-game API contracts are specified in [DATA_CONTRACTS.md](DATA_CONTRACTS.md#6-opening-explorer-http-api-contract).

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
| `post-publication verification failed` | Published copy failed deep verify | The previous current target is restored automatically when one exists; inspect the candidate workspace and stable release root. |
| Interrupted `build` | Workspace left under `data/work/` | Safe to delete; the active release root changes only when the atomic current symlink is switched. |
| Crash during `codb build` inside a dataset root | Staging directory under `versions/.staging_*` | `codb recover --dataset <dir>`; or discard the workspace. |

## 13. Reproducibility Status

* **Sources:** pinned by URL, size, and publisher SHA-256; rebuilds from the same archives are possible as long as Lichess keeps serving them. Lichess has historically not rewritten published monthly archives, but this is not a guarantee.
* **Substantive determinism:** equal archives, order, and build settings give identical accepted games, game IDs, accounting, and CPI hash (asserted by `scripts/test_corpus.py` on fixtures; real-corpus rebuild comparison in §9).
* **Not bitwise identical as a whole:** `dataset_build_id` and `build_timestamp` differ per build, so `manifest.json` and the version directory name differ. Measured on the real corpus (§9.5), `games.idx`, `positions.cpi`, and every shard file were byte-identical across rebuilds. Complete-directory bitwise determinism is **not** claimed.

## 14. Limitations and Remaining Work

* This is a real, useful baseline, not the future 15-million-game goal ([ROADMAP.md](ROADMAP.md)).
* Masters coverage is six recent monthly Lichess broadcast archives. The corpus release contains only the event-level verified OTB subset. Verified online and unresolved events are excluded. The audit is based on accepted games in the source-qualified baseline; its linked-page matching rule, missing-event evidence, and aggregate unresolved counts remain visible in each release. It does not assert that every event in the complete broadcast archives has been classified.
* Elite coverage consists of seven complete monthly archives from 2015-09 to 2017-03. It spans multiple years but reflects old Lichess play and rating levels; recent games are absent and no per-period balancing is applied.
* `ProvisionalRating` cannot be filtered from the standard PGN export because it contains no provisional-rating marker. Explicit aborted, abandoned, no-contest, unplayed, unterminated, and rules-infraction `Termination` values are rejected; unknown termination values remain eligible.
* Build RSS is recorded per executed process and host. The final Masters run measured 23,015,424 bytes; the unchanged Elite release retains earlier RSS measurements that are not directly comparable because the prior orchestration included Python parent memory.
* The 128 MiB index setting bounds the configured observation buffer; total process RSS is higher due to runtime and mapped data. HTTP concurrency measurements share the same 4-core host as the client.
* Releases and verified package archives remain local. Publishing requires a separately approved destination and retaining the license notice.
