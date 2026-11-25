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
