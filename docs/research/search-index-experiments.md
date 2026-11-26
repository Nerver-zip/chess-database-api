---
summary: "Reproduction instructions for production baseline and search-index layout experiments."
read_when: "Comparing candidate search indexes or repeating the legacy performance gate."
status: reference
---

# Search index experiments

## Corpus

Use the existing indexed packages for Masters OTB R3 and Elite Online R1. Verify the published parts, reconstructed Masters archive, and Elite archive against the committed release manifests before installing them into separate scratch dataset roots. See [Dataset releases](../DATASET_RELEASES.md). The experiment reads canonical games and leaves the installed snapshots intact.

## Legacy baseline

Build the baseline binary from `575d988dad9e69468e637dee05090008d364bccb` with Rust 1.100.0 and `cargo build --locked --release -p opening-explorer-cli`. Keep this binary alongside the candidate for paired comparisons. Run both binaries against the same verified CGB3, lookup and CPI files. Record CPU affinity, toolchain, and server configuration. Run measurements serially without other active builds.

```bash
taskset -c 0,1 python3 scripts/bench_legacy_api.py \
  --binary data/baseline/opening-explorer-main \
  --revision 575d988dad9e69468e637dee05090008d364bccb \
  --masters data/baseline/datasets/masters-otb \
  --elite data/baseline/datasets/elite-online \
  --deep --repetitions 3 --rounds 300 --workers 4 \
  --output data/baseline/warm.json
```

Repeat with `--evict --output data/baseline/eviction-advised.json` to advise kernel eviction of the snapshot files before launching the server. This is a best-effort startup condition. The server verifies and reads artifacts before readiness, so the first HTTP requests can already touch resident pages. In-process CPI samples report both the first and second query passes from a fresh process. Conditional requests exercise the existing 304 contract. The PGN probes sample eight stable IDs distributed across each collection's lookup file.

The normalized Explorer body digests exclude the build identifier and timestamp, which change when an enriched snapshot is published. ETags and PGN body hashes are also retained separately. Compare same-version runs for exact ETag equality. The bare-host harness samples process RSS, which includes resident mapped pages. Container accounting and the 512 MiB limit require a separate Docker run.

## Position posting layout probe

The probe replays a contiguous window of canonical games, sorts occurrences by position, game and ply, and compares fixed records, grouped delta encoding, and an ordered SQLite table. Pass no depth for full-game coverage. Run a second offset to make the dependence on physical corpus order visible. The Zstandard streams measure compression potential without random-access directories or checkpoints.

```bash
cargo build --locked --release -p opening-explorer-cli --example measure_search_layout
target/release/examples/measure_search_layout \
  --dataset data/baseline/datasets/elite-online \
  --games 25000 --depth 30 --sqlite --temp-dir data/baseline
```

The probe holds its sample in memory. A production builder needs a bounded spill and merge pipeline and must validate each index against its source snapshot. These measurements are sample comparisons and cannot establish full-corpus storage, query latency or deployment resource requirements.

## Initial measurements

The [legacy-main data](../../benchmarks/search/LEGACY_MAIN.json) records three warm and three eviction-advised runs with both published canonical snapshots from main at `575d988`. All expected HTTP status codes held. Median sequential HTTP p50/p95/p99 was 1.10/2.74/3.33 ms in the warm runs and 1.00/2.63/3.12 ms in the eviction-advised runs. Median four-client throughput was 1,698 and 1,741 requests per second. Use the raw case-level measurements and their run-to-run spread when comparing the candidate.

The [layout probe data](../../benchmarks/search/LAYOUT_PROBES.json) compares the first 25,000 physical games in each collection. At opening depth, the grouped directory plus uncompressed gap payload was 20.1 MB for Masters and 14.9 MB for Elite, compared with 33.2 MB and 25.4 MB for the SQLite posting table. At full depth the grouped layout exceeded fixed-width records in both samples because single-occurrence keys account for much of the added coverage. The grouped layout provides ordinal seeking while maintaining predictable bounded construction. SQLite remains useful for secondary metadata indexes, where its ordering and filter planner can avoid separate file formats for each projection.

The updated [Elite R1 opening build](../../benchmarks/search/ELITE_R1_OPENING_BUILD.json) indexed 13,191,619 occurrences over 6,558,404 distinct keys. It used 560.5 MB for both artifacts and peaked at 996.4 MB of sampled allocated staging blocks. The 128 MiB configuration covered sort records. The full process peaked at 284,968 KiB RSS. Build and deep verification together took 115.0 seconds on the recorded host. Removing redundant first-occurrence checkpoints and separating per-game URLs from event sites reduced the D30 artifact total from 924.7 MB in the initial build. The initial [CPI reconciliation](../../benchmarks/search/ELITE_R1_INITIAL_ORACLE.json) matched all 414 probed keys and all 3,940 sampled occurrences.

The [initial full Elite build](../../benchmarks/search/ELITE_R1_INITIAL_FULL_BUILD.json) retained 34,630,867 occurrences over 27,748,260 distinct keys. It used the initial checkpoint and event layout. The counts measure the corpus coverage. Its file sizes describe that earlier layout.

The [compact depth-80 Elite build](../../benchmarks/search/ELITE_R1_DEPTH80_BUILD.json) retained 29,009,713 occurrences across 22,304,090 distinct keys. Catalog and postings total 1.03 GB. Build and deep verification took 110.1 seconds. Process RSS peaked at 746,048 KiB and sampled scratch allocation peaked at 1.73 GB. The [full-coverage HTTP probe](../../benchmarks/search/ELITE_R1_FULL_COVERAGE_FLOW.json) replayed a canonical PGN with python-chess, then found the same stable ID at ply 60 and at its terminal board on ply 97. The terminal match had no continuation. Both sampled p95s were below 1.4 ms on the native host.

For additional comparisons, use `scripts/bench_search_build.py` to sample allocated scratch blocks, including open unlinked files, and `scripts/bench_search_api.py` for metadata, position, filtered and mixed HTTP workload timings. The [Evalis workflow smoke probe](../../scripts/verify_search_api.py) validates pagination, metadata, PGN and filtered counts against a running service.

The [compact Elite audit](../../benchmarks/search/ELITE_R1_ORACLE.json) matched all 789 sampled CPI keys. It verified 7,832 occurrences with canonical replay across 256 strided games. The [full-depth Elite audit](../../benchmarks/search/ELITE_R1_FULL_ORACLE.json) matched the same CPI key set and verified 20,754 sampled occurrences over full mainlines.

The [first alternating comparison](../../benchmarks/search/PAIRED_SHARED_CPU.json) matched legacy bodies, ETags and canonical artifact inventories. Its in-process CPI measures stayed within tolerance. Several HTTP timing medians exceeded the proposed limits with client and server sharing the same two CPUs. Run the benchmark with independent client and server affinity to examine this scheduling effect.

```bash
taskset -c 0,1 python3 scripts/bench_legacy_pair.py \
  --baseline data/baseline/opening-explorer-main \
  --baseline-revision 575d988dad9e69468e637dee05090008d364bccb \
  --candidate target/release/chess-database --candidate-revision candidate \
  --masters data/baseline/datasets/masters-otb \
  --elite data/baseline/datasets/elite-online \
  --server-cpus 6,7 --pairs 5 --rounds 300 \
  --output-dir data/paired-legacy-search
```

The [separated-CPU comparison](../../benchmarks/search/PAIRED_SEPARATE_CPU.json) met all proposed HTTP tolerances across five alternating runs. Sequential p50/p95/p99 changed by −4.7%/−7.2%/−13.7%. Concurrent p95 changed by +1.3%, p99 by +1.2%, and throughput by +2.2%. Response bodies and ETags matched on the same pinned legacy snapshots. The in-process warm-hit p95 increased by 12.1% for Elite and 8.5% for Masters in this run, while the earlier shared-CPU run met the in-process limits. These samples predate the CPI decoder reuse change.

The [dedicated CPI repeat](../../benchmarks/search/DEDICATED_CPI_PRE_DECODER.json) used seven alternating pairs and 10,000 sampled games per collection. Elite warm hit and miss p95 increased 5.8% and 13.3%. The final CPI reader reuses one Zstandard decoder context per worker. With this change, the [dedicated CPI comparison](../../benchmarks/search/DEDICATED_CPI.json) found warm hit p95 decreased 25.1% for Elite and 16.9% for Masters. Warm miss p95 decreased 27.8% and 24.6%. All measured warm CPI quantiles met the proposed limits.

```bash
taskset -c 0,1 python3 scripts/bench_cpi_pair.py \
  --baseline data/baseline/opening-explorer-main \
  --candidate target/release/chess-database \
  --masters data/baseline/datasets/masters-otb \
  --elite data/baseline/datasets/elite-online \
  --output data/cpi-search-pair.json
```

The [warm comparison after decoder reuse](../../benchmarks/search/PAIRED_FINAL_WARM.json) alternated five baseline and candidate pairs with 500 request-corpus rounds per phase. The client used logical CPUs 0 and 1 and the server used 4 and 5, on the same two physical cores. The two remaining physical cores ran the isolated Docker guest and Masters builders. The median sequential HTTP p50/p95/p99 changed by −1.4%/−3.1%/−9.6%. Concurrent p50/p95/p99 changed by +1.5%/+2.4%/+3.8% and throughput changed by −3.5%. Bodies, ETags and canonical artifact inventories matched. All aggregate HTTP and CPI measures met the proposed tolerances. The report retains individual PGN timing samples and the median startup, server CPU and RSS measurements.

In the matching [eviction-advised run](../../benchmarks/search/PAIRED_FINAL_EVICTION_ADVISED.json), the mixed sequential p50 was 7.8% higher, outside the proposed tolerance. The sequential p95 was 3.3% lower, concurrent p95 was 12.4% lower and concurrent throughput was 10.8% higher. Median per-case sequential p50 decreased across Explorer cases and increased 2.5% across PGN cases. The pooled p50 lies near the boundary between the faster Explorer requests and the slower PGN requests.

The [separate-physical-core eviction-advised repeat](../../benchmarks/search/PAIRED_ISOLATED_COLD.json) compared the final binary across five paired trials of 300 rounds. Client logical CPUs 0 and 4 and server logical CPUs 1 and 5 belong to different physical cores. The repeat satisfied all the proposed tolerances. Sequential p50/p95/p99 changed by −16.6%/−5.3%/−12.1%. Concurrent p95 decreased 6.7% and throughput increased 5.2%. Same-version bodies and ETags matched. The median PGN case p95 decreased in both sequential and concurrent phases.

The [warm repeat with the same affinity](../../benchmarks/search/PAIRED_ISOLATED_WARM.json) used three paired trials and satisfied all the proposed tolerances. Sequential p50/p95/p99 changed by +2.4%/−2.0%/−2.8%, concurrent p95 changed by +0.2%, and throughput was effectively unchanged. In-process warm hit p95 decreased by 23.4% for Elite and 17.7% for Masters. To reproduce both final comparisons, use the paired runner above with `taskset -c 0,4`, `--server-cpus 1,5` and `--rounds 300`. Use `--pairs 5 --evict` for the eviction-advised runs and `--pairs 3` for warm runs.

## Compact Elite production checks

The [companion install](../../benchmarks/search/ELITE_R1_BUNDLE_INSTALL.json) reproduced both search artifact hashes against a separate installation of the published Elite snapshot. The installed version passed deep verification and retained the same canonical artifact inventory.

The [native workflow](../../benchmarks/search/ELITE_R1_NATIVE_FLOW.json) verified the search-to-PGN flow, filtered counts and the legacy conditional request. In the [native workload](../../benchmarks/search/ELITE_R1_NATIVE_WORKLOAD.json), player prefix p95 was 12.3 ms and positional page p95 was below 2 ms for the sampled start, common and medium positions. The date-sorted common event name query took 849 ms at p95. The broad initial-position filtered aggregate returned the documented 422 work-limit response. All 300 legacy requests during four-client search overload returned 200. Search admission returned 429 when the single worker was occupied.

The [final Elite flow](../../benchmarks/search/ELITE_R1_FINAL_NATIVE_FLOW.json) and [mixed workload](../../benchmarks/search/ELITE_R1_FINAL_NATIVE_WORKLOAD.json) use the indexed player-name and date-range query plans. Player prefix p95 was 1.5 ms and the selective filtered aggregate was 4.2 ms. The broad common-event date sort remained an expensive operation at 584 ms p95. All 300 legacy requests succeeded during four-client mixed traffic.

## Constrained Docker

The host's read-only cgroup mount blocked constrained containers. An isolated Ubuntu guest running Docker under QEMU TCG used local copies of the verified production Elite snapshots and a pinned Debian Bookworm runtime. The [first Docker run](../../benchmarks/search/ELITE_R1_DOCKER.json) exercised a legacy snapshot, atomic reload to the enriched snapshot and rollback. It used a 512 MiB limit, two-CPU quota, read-only dataset and root filesystem, and a nonroot UID. The legacy environment fallback, conditional requests, and discovery-to-PGN flow passed. All 120 legacy requests in the mixed run succeeded. Expensive filtered queries reached the deadline under emulation. Guest timings describe this validation environment. The first run's file cache included pages charged when the corpus was copied outside the container.

The [final-binary disabled run](../../benchmarks/search/ELITE_R1_DOCKER_DISABLED.json) served the enriched snapshot under the same limits. Search and search metadata returned 403, legacy PGN returned 200, and all three Compose network aliases reached readiness from a sibling container. The separate cold deep-startup attempt was stopped before readiness under TCG. Its cgroup recorded file-cache pressure and zero OOM events. The completed serving trials used the normal startup checks and artifacts already deeply verified on the native host.

The [final-binary Docker serving run](../../benchmarks/search/ELITE_R1_DOCKER_RELEASE.json) completed the Elite workflow and returned 200 for all 120 legacy requests under search load. The live rollback request took 7.8 seconds. All 108 Explorer requests and 106 PGN requests sent across the rollback window succeeded. Explorer counts and canonical PGN bytes matched across the version switch. The serving cgroup recorded zero OOM events. Its measured peak includes only memory charged to the service cgroup, and file pages retained from earlier runs can be charged elsewhere.

Reproduce the request flows with the release binary in the standard runtime image and a local dataset mount. Start with a validated legacy snapshot and run `scripts/verify_search_api.py --mode legacy`. Switch the dataset's `current` symlink to the enriched version and reload as described in [Docker deployment](../DOCKER_DEPLOYMENT.md#safe-dataset-updates). Run the same probe without `--mode` and use `scripts/bench_search_api.py --workers 4` for concurrent traffic. After rollback, check legacy mode again. For a separate enriched start with discovery and search metadata disabled, use `--mode disabled`.

## Masters R3 depth 40

The [compact production build](../../benchmarks/search/MASTERS_R3_OPENING_BUILD.json) and the [first encoder run](../../benchmarks/search/MASTERS_R3_INITIAL_OPENING_BUILD.json) both passed deep verification. The compact build preserved the published canonical inventory and recorded 130,852,249 occurrences over 54,827,221 distinct keys and 3,284,429 games. The compact catalog and postings use 5.57 GB combined, down from 7.10 GB for the first layout. The compact builder used a 128 MiB sort buffer and peaked at 2,139,720 KiB process RSS, including touched mapped files. Peak sampled allocated staging and scratch blocks were 10.89 GB. Both wall times include pauses for other measurements. The compact builder used 1,191 seconds of total CPU time, and deep verification took 387 seconds of wall time after the benchmark pauses.

The [Masters audit](../../benchmarks/search/MASTERS_R3_ORACLE.json) replayed 256 games at deterministic strides through canonical order. It checked 10,136 occurrences and matched all 741 distinct position probes to CPI first-visit, continuation and result counts. The probe set includes the initial position and cases at the CPI coverage boundary.

The [Masters API workflow](../../benchmarks/search/MASTERS_R3_NATIVE_FLOW.json) passed with explicit discovery and metadata opt-in. The [initial Masters workload](../../benchmarks/search/MASTERS_R3_NATIVE_WORKLOAD_PRE_PLANNER.json) exposed a player prefix scan and timeouts in the broad year range sorted by date. The [final Masters workload](../../benchmarks/search/MASTERS_R3_NATIVE_WORKLOAD.json) used the existing player alias index and date-order index. Observed player prefix p95 was 136 ms before and 1.7 ms after the change. All five date-sorted year queries passed at 3.2 ms p95 after the change. The HTTP runs used different CPU affinity. Direct query-plan checks on the same catalog showed the player-name scan and year sort gave way to index range seeks. The sampled selective filtered aggregate took 3.2 ms p95, while broad aggregates returned the work-limit response. All 300 legacy requests in mixed traffic returned 200. The [interruption recovery](../../benchmarks/search/MASTERS_R3_RECOVERY.json) removed the abandoned production-build stage and preserved the active symlink.

```bash
cargo build --locked --release -p opening-explorer-cli --example audit_search_index
target/release/examples/audit_search_index \
  --dataset data/search-validation/masters-otb-compact --sampled-games 256
```

## Final hardening on both production snapshots

The [native before/after report](../../benchmarks/search/HARDENING_NATIVE.json) compares the pre-hardening PR (`6a29e6e929cac08a4ae27feac282a67e5201e1c5`) with the serving implementation at `3518a8e7f38b4497a5491dca926b54440da57848`. The candidate executable SHA-256 is `0436ed664d6305b0d3d01dc79bec92a48fb7833b1b190d2cdf832e1941c38c31`. Both binaries used the pinned enriched Masters R3 D40 and Elite R1 D30 versions named in the reports. The first candidate probe remains in the report as an earlier measurement.

| Masters request | Pre-hardening serial | Hardened serial | Hardened p95 |
| --- | --- | --- | ---: |
| Initial position, both ratings at least 2200 | 5/5 HTTP 422 | 5/5 HTTP 200 | 1,195 ms |
| Sicilian after `e4,c5`, both ratings at least 2400 | 5/5 HTTP 422 | 5/5 HTTP 200 | 230 ms |
| 20-ply Ruy Lopez, `min_plies=0` | 5/5 HTTP 422 | 5/5 HTTP 200 | 34 ms |

Previously these requests entered the SQL-backed aggregate planner and hit its work budget. The ordinal projection holds only dates, ratings and outcomes, at up to 16 bytes per game and four million games per collection. Common rating, date, year and result predicates can scan the compact posting group with a 20 MiB encoded bound and cooperative cancellation. Player, event and other complex predicates still use the bounded SQL planner with the existing 250,000-unit work limit. An effectively unrestricted query with `min_plies=0` uses CPI. See [filtered Explorer semantics](../SEARCH_API.md#6-filtered-opening-explorer-statistics).

The [Masters canonical replay](../../benchmarks/search/HARDENING_MASTERS_ORACLE.json) processed all 3,284,429 games from CGB3 and checked each continuation's W/D/L counts against HTTP. It confirmed totals of 2,903,599 at the initial position with both ratings at least 2200, 183,790 at the Sicilian with both ratings at least 2400, and 486,306 at the Sicilian with either `from_year=2000` or `from_date=2000-01-01`. The Ruy Lopez no-op filter matched the legacy CPI response body.

In the four-user run, each request class was attempted 40 times with 125 ms think time. Autocomplete, metadata, position search, latest-board navigation, legacy Explorer and PGN all succeeded 40/40 with two workers, compared with 39, 35, 37, 36, 40 and 40 successes respectively with one worker. Two-worker p95 on the four successful discovery classes ranged from 8.9 to 14.0 ms. Broad rating and date filters each succeeded 24/40, with the remainder returning 429 and accepted p95 of 776 and 169 ms. One worker accepted 36/40 and 33/40 respectively in its separate run. This limited comparison supports two workers as the initial setting for protecting fast discovery while a filtered aggregate is active. It does not establish a throughput advantage for broad aggregates. The client abandoned 40 superseded board requests. Cooperative server cancellation is verified separately by the search runtime tests. In the deliberate four-client mixed workload, all 300 legacy requests succeeded and the two broad rating requests each succeeded 8/20, with 12/20 returning 429.

### Paired legacy HTTP and CPI

Three alternating runs per binary used separate server and client physical cores and the same CPI-only release snapshots. The comparison against main revision `575d988dad9e69468e637dee05090008d364bccb` records identical canonical artifact inventories, response bodies and same-snapshot ETags. Its executable SHA-256 is `56b690fdcc42507d49883a57a16f3cdcaf83d4e17d250b63efa424ed4ed0a8b2`.

| Regime | Sequential p50 | Sequential p95 | Concurrent p95 | Concurrent requests/sec | Timing gate |
| --- | ---: | ---: | ---: | ---: | --- |
| Warm | +0.9% | +1.2% | +0.7% | +1.9% | Passed |
| Eviction-advised | +7.9% | +1.4% | −3.8% | +2.9% | Sequential p50 exceeded the 5% tolerance |

The initial eviction-advised sequential median increased from 1.305 to 1.408 ms. The other metrics met their thresholds, including warm-pass CPI hit p95 improvements of 9.1% to 18.8% across the two runs and collections. The [warm run](../../benchmarks/search/HARDENING_PAIRED_WARM.json) and [initial eviction-advised run](../../benchmarks/search/HARDENING_PAIRED_EVICTION_ADVISED.json) retain the full samples and gate results.

A [five-pair confirmation](../../benchmarks/search/HARDENING_PAIRED_EVICTION_CONFIRMATION.json) on the same CPI-only roots did not reproduce the p50 miss. Sequential p50 fell 0.9%, sequential p95 rose 6.7% and crossed its 5% threshold, concurrent p95 fell 1.8%, and throughput rose 3.4%. The sequential p95 change within each pair ranged from −20.3% to +10.5%, with a median of −1.1%. The comparator still reports failure because it compares the two variants' medians. Both runs matched canonical inventories, bodies and ETags. The isolated result leaves a timing discrepancy to review on the owner host. All measurements use best-effort file eviction and retain the original thresholds.

A [second five-pair confirmation](../../benchmarks/search/HARDENING_PAIRED_EVICTION_QUIET.json) paused the QEMU guest throughout the native measurement. Sequential p50 and p95 improved by 2.3% and 1.0%. Concurrent p95 rose 12.2%, p99 rose 13.7%, and throughput fell 5.5%, failing the existing gate. Concurrent p95 changes within individual pairs ranged from −0.1% to +56.6%, with a median of +3.7%. The slowest repetition affected both PGN and Explorer requests. CPI hit and miss timings improved for both collections. Canonical inventories, response digests and same-snapshot ETags still matched. The failed concurrent gate requires a representative isolated run before acceptance.

A [five-pair control using the identical candidate executable in both arms](../../benchmarks/search/HARDENING_PAIRED_EVICTION_IDENTICAL.json) used the same CPI-only roots, CPU split and paused-guest setup. It passed the directional comparator. The second arm still measured 13.9% lower concurrent p95, 22.9% lower concurrent p99 and 8.9% higher throughput. The two arms used the same binary SHA-256. These differences establish the scale of variation in this environment. The three failed baseline comparisons remain part of the acceptance record.

A native host follow-up on 2026-11-26 compared the pinned baseline `575d988` and merged candidate `5e8dc18` using five alternating pairs and the published CPI-only roots. The [warm comparison](../../benchmarks/search/native-release-warm/comparison.json) passed all timing and identity checks. The [eviction-advised comparison](../../benchmarks/search/native-release-evict/comparison.json) preserved artifact identities, responses and ETags but failed sequential p99 (+25.35%), concurrent p50 (+6.78%), p95 (+11.40%), p99 (+12.48%) and throughput (−9.91%), among other metrics. An [identical-candidate control](../../benchmarks/search/native-release-evict-control/comparison.json) also failed concurrent p50 (+5.46%), p95 (+7.40%) and throughput (−5.75%), demonstrating substantial environmental variance. The control does not account for every observed failure, so native legacy performance acceptance remains inconclusive. Eviction was advisory rather than a guarantee of cold cache. Seven alternating pairs of [in-process CPI samples](../../benchmarks/search/NATIVE_RELEASE_CPI_PAIR.json) showed lower candidate medians across warm hit and miss percentiles.

### Two enriched collections under emulation

The [startup report](../../benchmarks/search/HARDENING_TWO_COLLECTION_QEMU.json) used both enriched snapshots in a pinned Debian runtime with the hardened native executable. Compose retained read-only mounts, a 512 MiB memory cap and two CPUs. The guest restarted before the observation and ran under QEMU TCG. The service missed the 600-second acceptance window and bound its socket at 1,006 seconds. It advertised both collections on `/ready`. At the first ready sample the service cgroup recorded 1,023 CPU seconds, a memory peak of 536,875,008 bytes and zero OOM events. At the ten-minute startup sample, file-backed memory accounted for about 498 MiB and anonymous memory for about 1 MiB. The host backing cache was uncontrolled and a separate image build ran in the guest. These data characterize the emulated run. The [native Docker acceptance commands](../DOCKER_DEPLOYMENT.md#two-collection-acceptance-on-a-local-docker-host) cover representative readiness and serving under the deployment cap.

Two partially overlapping four-user workloads each sent 120 requests per case. All 480 legacy Explorer and PGN requests succeeded, with successful p95 ranging from 58 to 79 ms. Masters autocomplete, metadata, position and latest-board requests each passed 120/120. The broad Masters rating query returned 22 deadlines and 98 busy responses. Elite's broad rating query served 36/120 and returned 84 busy responses. The Elite and serial Masters discovery-to-PGN probes passed. A further 240 legacy requests all passed while a same-snapshot reload was revalidating the data. Both collections remained ready at 606 seconds. The reload client reached its 900-second deadline without a response. Once the guest resumed after the native timing run, the server logged successful completion for both versions. The cgroup still recorded zero OOM events. The guest pause prevents a continuous reload-duration measurement from that completion timestamp.

On the [native host](../../benchmarks/search/HARDENING_NATIVE_RELOAD.json), revalidating both enriched snapshots during a four-user Masters workload took 14.84 seconds. The reload returned 200 and the collection identities and capabilities matched before and after. Both legacy classes and all four discovery classes passed 40/40. This run had no Docker memory cap, and its sampled process RSS includes file-backed mappings.

The [repository Docker image trial](../../benchmarks/search/HARDENING_REPOSITORY_IMAGE_QEMU.json) built the checked-in Dockerfile at `5b94bbe` and ran the image with default serving settings, the debug loopback overlay and both datasets mounted read-only. The guest page cache was evicted. The host backing cache was uncontrolled. A direct `/ready` poll succeeded 549 seconds after Compose startup. Loading Masters took 468 seconds in the service log, followed by 50 seconds for Elite. After both collections served a legacy Explorer probe, Docker reported healthy and the cgroup had used 550 CPU seconds. It recorded a 536,875,008-byte memory peak, zero OOM events and zero restarts.

The Elite discovery-to-PGN check passed. With four interactive users, all 360 legacy Explorer and PGN requests succeeded across the two collections. Elite autocomplete, metadata, position and latest-board requests each passed 120/120. The date, rating and player filters served 80, 94 and 85 of 120 requests respectively. The remaining requests in those cases returned HTTP 429. During SIGHUP reload verification, a second workload served all 360 legacy requests. Elite date, rating and player filters served 70, 93 and 65 of 120 requests, with the remaining requests returning HTTP 429. A probe four minutes into the reload returned HTTP 200 from readiness and both Explorer routes. The server finished revalidating both pinned versions after 536.41 seconds. These timings and admission rates describe the emulated guest at 512 MiB and two CPUs.

### Reproduce the hardening measurements

Use local immutable copies of the enriched snapshots. Enable Masters discovery and metadata only on the isolated loopback test server. Build from the pinned revisions and record the toolchain and executable hashes.

```bash
builds="$PWD/data/hardening/builds"
mkdir -p "$builds"
git worktree add --detach "$builds/main" 575d988dad9e69468e637dee05090008d364bccb
git worktree add --detach "$builds/pre-hardening" 6a29e6e929cac08a4ae27feac282a67e5201e1c5
git worktree add --detach "$builds/candidate" 3518a8e7f38b4497a5491dca926b54440da57848
cargo build --locked --release --manifest-path "$builds/main/Cargo.toml" \
  --target-dir "$builds/main/target" -p opening-explorer-cli --bin opening-explorer
cargo build --locked --release --manifest-path "$builds/pre-hardening/Cargo.toml" \
  --target-dir "$builds/pre-hardening/target" -p opening-explorer-cli --bin chess-database
cargo build --locked --release --manifest-path "$builds/candidate/Cargo.toml" \
  --target-dir "$builds/candidate/target" -p chess-database-cli --bin chess-database
MAIN_BIN="$builds/main/target/release/opening-explorer"
PRE_HARDENING_BIN="$builds/pre-hardening/target/release/chess-database"
CANDIDATE_BIN="$builds/candidate/target/release/chess-database"
rustc --version
sha256sum "$MAIN_BIN" "$PRE_HARDENING_BIN" "$CANDIDATE_BIN"
```

Set `MASTERS_ROOT` and `ELITE_ROOT` to the enriched release roots. For the search comparison run both workload shapes once against each server, using separate output filenames. The canonical replay helper runs without a server. The HTTP oracle comparison is for the hardened candidate.

```bash
cargo build --locked --release -p chess-database-cli --example audit_filtered_explorer
target/release/examples/audit_filtered_explorer --dataset "$MASTERS_ROOT" \
  > data/hardening/masters-expected.json
CHESS_DATABASE_MASTERS_DISCOVERY=true CHESS_DATABASE_MASTERS_GAME_METADATA=true \
  "$CANDIDATE_BIN" serve --masters-dataset "$MASTERS_ROOT" --elite-dataset "$ELITE_ROOT" \
  --host 127.0.0.1 --port 19192
```

```bash
python3 scripts/bench_search_api.py --url http://127.0.0.1:19192 \
  --collection masters-otb --workers 4 --repetitions 5 --output data/hardening/search.json
python3 scripts/verify_search_api.py --url http://127.0.0.1:19192 \
  --collection masters-otb --masters-oracle data/hardening/masters-expected.json
python3 scripts/bench_search_interactive.py --url http://127.0.0.1:19192 \
  --collection masters-otb --users 4 --rounds 10 --think-ms 125 \
  --output data/hardening/interactive.json
```

Repeat against `PRE_HARDENING_BIN` for the before/after workload and against `CANDIDATE_BIN` with `--search-concurrency 1` for the worker comparison. Stop the previous server before starting the next.

For the legacy pair, stop the search test server and choose separate physical CPUs for the server and client. Set `MASTERS_CPI_ROOT` and `ELITE_CPI_ROOT` to the unmodified Masters R3 and Elite R1 indexed release roots. The main baseline predates the enriched snapshot manifest. Repeat with `--evict` and a separate output directory.

```bash
taskset -c "$CLIENT_CPUS" python3 scripts/bench_legacy_pair.py \
  --baseline "$MAIN_BIN" --baseline-revision 575d988dad9e69468e637dee05090008d364bccb \
  --candidate "$CANDIDATE_BIN" --candidate-revision 3518a8e7f38b4497a5491dca926b54440da57848 \
  --masters "$MASTERS_CPI_ROOT" --elite "$ELITE_CPI_ROOT" --rounds 300 --pairs 3 \
  --server-cpus "$SERVER_CPUS" --output-dir data/hardening/paired-warm
```

The eviction-advised confirmations used `--pairs 5 --evict`. To measure timing variation with identical code, use the candidate executable and revision for both arguments. Use a separate output directory and retain the comparator result alongside the paired measurements.
