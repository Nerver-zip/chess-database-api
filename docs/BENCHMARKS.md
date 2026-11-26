---
summary: "Authoritative benchmark methodology, production CPI baselines, reproduction steps, and scaling models."
read_when: "Reproducing empirical scaling measurements, running benchmark suites, or evaluating Heaps' Law parameters."
status: current
---

# Benchmark Methodology & Reproduction Guide

## 1. Benchmark Architecture & Taxonomy

The benchmark ecosystem in `chess-database-api` is partitioned into the following domains:

```
benchmarks/
├── production_cpi/     # Authoritative active production CPI index baselines (ADR-0003)
├── corpus/             # Current Masters/Elite release and Docker evidence
└── results/            # Runtime benchmark outputs
    └── historical/     # Completed developmental stages & research baselines
```

- **Production CPI Benchmarks (`benchmarks/production_cpi/`):** The normative performance baseline for the Compact Position Index (CPI v2) built via bounded-memory external sorting. Evaluated across 16 parameter combinations.
- **Real Corpus Evidence (`benchmarks/corpus/`):** Machine-validated provenance, accounting, quality, build-performance, and HTTP-check records for the first real Masters OTB and Elite Online releases (section 2.3).
- **Historical Benchmarks (`benchmarks/results/historical/`):** Archived empirical artifacts from prior developmental phases: Stage A (deterministic fixture), Stage B (single-month baselines), PR-01 progressive scaling ($N \in \{1000, 2500, 5000, 10000, 20000\}$), PR-02 ($100{,}000$ game scale exploration), and derived Heaps' Law projection parameters (`projections_heaps.json`).

---

## 2. Production CPI Benchmark Suite

Defined in [ADR-0003](adr/0003-production-cpi-index.md) and validated by [`scripts/validate_production_cpi_benchmarks.py`](../scripts/validate_production_cpi_benchmarks.py).

### 2.1 Matrix Parameter Dimensions
- **Collections:** `masters-otb` and `elite-online`
- **Sample Sizes ($N$):** 1,000, 20,000, 50,000, and 100,000 games
- **Memory Limits ($M$):** 16 MiB, 32 MiB, and 128 MiB accumulation ceilings
- **Fan-Ins ($F$):** 2-way and 64-way external merge passes

### 2.2 Key Performance Invariants
1. **Bounded Heap Allocation:** Peak memory during external sorting strictly obeys configured ceiling ($M$).
2. **Sub-Millisecond Query Latency:** Opening Explorer lookup latency over memory-mapped CPI blocks achieves:
   - $p_{50} \le 1.0\text{ ms}$
   - $p_{95} \le 1.5\text{ ms}$
   - $p_{99} \le 2.0\text{ ms}$
3. **Deterministic Output:** Byte-identical `.cpi` binary outputs regardless of memory limit or merge fan-in settings.

---

## 2.3 Real Corpus Release Baseline (PR-A)

`benchmarks/corpus/` holds the committed, machine-validated evidence of the first real dual-collection build (see [CORPUS_BUILD.md §9](CORPUS_BUILD.md#9-measured-results-this-delivery) for the full tables):

```
benchmarks/corpus/
├── http_check.json                 # `corpus.sh http-check`: checks, HTTP/CLI cases, latency, concurrency
├── docker_real_check.json           # Compose integration against both real releases
├── masters-otb/masters_event_qualification.json # Event evidence and OTB subset decision
├── package_checksums.json           # Verified package SHA-256 and Path A bootstrap result
├── masters-otb/{RELEASE.json, build_report.json, quality_report.json, metrics.json, index_report.json, DATASET_LICENSE.txt}
└── elite-online/{...same files...}
```

`python3 scripts/validate_corpus_evidence.py` checks exact accounting, rejection-reason totals, hashes, source checksums and licenses, build steps, and HTTP evidence. This historical baseline used 29,640 event-qualified Masters games (depth 40, 705,718 positions) and 429,150 Elite games (depth 30, 6,136,307 positions; seven archives from 2015-09 to 2017-03). It passed 414/414 HTTP checks, 262 HTTP-vs-CLI position comparisons, and a Compose smoke with 96/96 container-to-container requests. HTTP position-query p50 was 167.9 µs for Masters and 190.6 µs for Elite. Warm in-process CPI hit p50 was 4.95 and 5.60 µs, respectively. Four-client-process throughput reached 10,671 and 8,127 requests/s on a shared four-core host. The container sampled peak was 159,224 KiB under its 512 MiB cap. See [corpus build performance](CORPUS_BUILD.md#93-query-and-api-performance).

The historical Masters snapshot remains a rollback version. Use the [current corpus search and legacy measurements](#6-search-and-interactive-serving) for Masters R3 (3,284,429 games) and Elite R1. The fixture-based `http_api_benchmark.json` remains the lightweight CI baseline.

---

## 3. Dataset Sources and Checksums

All empirical datasets are sourced from the official [Lichess Open Database](https://database.lichess.org/):

| Dataset Identifier | Canonical File Path | Source URL | Expected Uncompressed Games | SHA-256 Checksum |
| :--- | :--- | :--- | ---: | :--- |
| **Stage A Fixture** | `fixtures/stage_a_validation.pgn` | *Committed in Git repository* | 1,000 | `fbfb01e3eeefc54e7d4db5c510b642646d47cfbf426c5188bfb92784ee9b35bc` |
| **Broadcast 2026-07** | `data/masters_otb_2026-07.pgn.zst` | `https://database.lichess.org/broadcast/lichess_db_broadcast_2026-07.pgn.zst` | 40,038 | `714d0eb99f99fca8d791142038b6c59b5ca6a51b3339bd3891a92f4bdffcbf0c` |
| **Broadcast 2026-08** | `data/masters_otb_2026-08.pgn.zst` | `https://database.lichess.org/broadcast/lichess_db_broadcast_2026-08.pgn.zst` | 48,940 | `e227c35c3207ebade754849c1825982e2717ae82c013013d896778e27be77724` |
| **Broadcast 2026-09** | `data/masters_otb_2026-09.pgn.zst` | `https://database.lichess.org/broadcast/lichess_db_broadcast_2026-09.pgn.zst` | 28,638 | `27a39d035767a30901b89602f33f2a7b27d4ab5025842071165fa079f8fff1df` |
| **Rated 2013-01** | `data/elite_online_2013-01.pgn.zst` | `https://database.lichess.org/standard/lichess_db_standard_rated_2013-01.pgn.zst` | 121,332 | `aa40b3671fa3cf1072eb182892cd90b0e1e003a4a5943492f64b77e7f3fd1635` |

### 3.1 Combined Masters OTB Assembly
To assemble the chronological multi-month scaling corpus:
```bash
zstd -dc \
  data/masters_otb_2026-07.pgn.zst \
  data/masters_otb_2026-08.pgn.zst \
  data/masters_otb_2026-09.pgn.zst \
  | zstd -3 -o data/masters_combined.pgn.zst
```
- **Uncompressed Stream SHA-256:** `9cc0f844cf8c788e76ad13f8c634beb19dc73f55018a124c2df00aa7ecc716e7`
- **Compressed Archive SHA-256:** `1302f9191e4cdd12577aa88357cfc6bb895775aca5114b7e016ae43995ace2e8`

---

## 4. Benchmark Reproduction Procedures

### 4.1 Lightweight CI Mode (`--ci`)
Executes the deterministic 1,000-game Stage A validation fixture and asserts schema and consistency invariants without external downloads. The generated result is validated in temporary storage, so the archived Stage A measurements stay intact.
```bash
./scripts/reproduce_benchmarks.sh --ci
```

### 4.2 Full Empirical Reproduction Mode (`--all`)
Executes the complete empirical experiment against local datasets in `data/`:
```bash
./scripts/reproduce_benchmarks.sh --all
```
Fails closed with exit code 1 if datasets are missing or fail cryptographic checksum verification.

---

## 5. Mathematical Heaps' Law Scaling & Feasibility Projections

Position diversity growth is modeled using Heaps' Law:

$$V(N) = k \cdot N^\alpha$$

Where $V(N)$ is unique positions observed across $N$ curated games, $k$ is the corpus-specific proportionality constant, and $\alpha$ is the sub-linear scaling exponent.

### 5.1 Calibrated Model Parameters
Fitted via ordinary least squares (OLS) log-linear regression in [`scripts/fit_heaps.py`](../scripts/fit_heaps.py):
- **Masters OTB (Depth 40):** $\alpha = 0.9293$, $k = 49.88$, $R^2 = 0.99998$.
- **Elite Online (Depth 30):** $\alpha = 0.9293$, $k = 37.63$, $R^2 = 0.99999$.

### 5.2 Option B Deployment Projections (15 Million Games)
Slicing **Masters OTB (5M games) to Depth 40** and **Elite Online (10M games) to Depth 30**:
- **Historical baseline:** **7.68 GB** was the original projection using prototype storage-density measurements. The later PR-A extrapolation used the 29,640-game Masters and 429,150-game Elite snapshots and estimated about 7.76 GB. Its predicted unique-position counts were 1.6% below the Masters measurement and 4.9% below the Elite measurement at that time. See [storage feasibility against the earlier projection](CORPUS_BUILD.md#96-storage-feasibility-against-the-earlier-projection).
- **Optimistic Scenario:** **6.44 GB** total deployed footprint.
- **Conservative Scenario:** **9.98 GB** total deployed footprint (leaves **20 MB / 0.2%** headroom within the 10 GB envelope).

Storage feasibility is **conditional** upon enforcing Depth 30 slicing for Elite Online; extending Elite Online to Depth 40 pushes conservative projections to 12.26 GB, breaching the 10 GB budget.


## 6. Search and interactive serving

The [search evidence and reproduction commands](research/search-index-experiments.md) record compact and full coverage, build resources, exact canonical and CPI reconciliation, companion verification, and paired legacy HTTP and CPI measurements. Use the same immutable datasets, affinity and request mix for before/after trials. Separate interactive users with think time and superseded navigation from deliberate overload. Record successful latency alongside 429, 408 and 422 counts. Include the tested source revision and release-binary SHA-256. Treat QEMU timing and container memory observations as separate from native serving measurements.

The [hardening comparison](research/search-index-experiments.md#final-hardening-on-both-production-snapshots) records the three previously rejected Masters requests, full canonical replay for rating and year filters, the single- and two-worker workloads, and the warm and eviction-advised legacy pairs.
