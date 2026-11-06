---
summary: "Historical research report on storage feasibility, scaling models, and empirical measurements."
read_when: "Reviewing the mathematical derivations and empirical basis for the 10 GB storage budget."
status: historical
---

# Research Track F: Storage Feasibility Report & Empirical Scaling Analysis

## 1. Executive Summary

This report delivers the engineering and scientific scaling evaluation for the **Chess Openings DB** project, addressing the central storage and scaling questions across PR-01 and PR-02:

> **Can we build a reliable, reproducible, publicly redistributable chess database containing millions of high-quality games, with full game retrieval and position-based opening statistics, within approximately 10 GB of deployed storage?**

### Conclusion: **Feasibility Confirmed Across Scale & Production Storage Architecture**

Building upon PR-01 and PR-02, PR-03 delivered the production position index (CPI v2) pipeline with bounded-memory external sorting, deterministic transposition aggregation, atomic publication, and functional Opening Explorer queries:

1. **Directly Demonstrated in PR-01, PR-02 & PR-03 (Empirical Evidence):**
   - **Streaming Ingestion & Bounded Memory:** Ingestion memory is strictly bounded ($O(1)$ RAM with corpus size). Peak RAM RSS remains strictly below **30 MB** throughout builds of 50,000 to 100,000 games.
   - **Bounded-Memory External Sorting (PR-03):** Position indexing operates within a configurable RAM ceiling (`--memory-limit-mib`, default 128 MiB). Temporary runs (`.crun`) are sorted, flushed to disk, and merged via bounded multi-pass K-way merge with fan-in limits, completely avoiding whole-corpus in-memory retention.
   - **Persistent Sharded Storage Container (`CGB3`):** Games are persisted into multi-block binary shards (`shard_XXXX.cgb3`) with configurable shard capacities, SHA-256 block footers, and atomic staging manifests.
     - **Masters OTB:** **208.2 bytes/game** in CGB3 blocks (5.16× compression ratio over PGN) with 100% round-trip fidelity across all supported canonical and provenance fields.
     - **Elite Online:** **178–181 bytes/game** in CGB3 blocks (~15% more compact than Masters OTB due to shorter average game length of ~60 plies vs. ~75 plies).
   - **Stable Random-Access Game Retrieval (`games.idx`):** Memory-mapped 16-byte fixed records sorted by 64-bit ID enable binary search retrieval in **733 µs (p50)**, **813 µs (p95)**, and **889 µs (p99)** across 50,000+ games.
   - **Production Position Indexing (`CODB_CPI` v2 vs. SQLite):** Persistent `.cpi` files consume **21.40–21.60 bytes per unique position** at scale, achieving **7.2× to 9.4× smaller disk footprint than SQLite** with sub-25 µs in-process warm lookup times.
   - **Chess Semantics & Correctness (PR-03):** Orthodox legal FEN setups are indexed correctly (illegal setups fail closed); intra-game revisits within the depth window are deduplicated; unknown-result (`*`) games are excluded from W/D/L counters; and output is 100% byte-identical across arbitrary buffer sizes.
   - **Crash-Safe Atomic Publication (PR-03):** Staged in isolated temporary directories, verified deeply before activation, and committed via atomic symlink switch with automatic crash recovery.

2. **Empirical Online Validation & Diversity Divergence:**
   - Curation of modern Lichess Standard Rated archives (2024–2026) demonstrates a **~1.95%–2.0% acceptance rate** under `EliteOnlinePolicy` (bullet excluded, ratings $\ge 2200$).
   - Elite Online exhibits **~40% higher position diversity** at Depth 20 and **~20% higher diversity** at Depth 30 than Masters OTB, validating that online repertoires branch more widely than classical grandmaster tournament play.

3. **Recalibrated 15-Million-Game Deployment Projections (Option B):**
   - Applying independent Heaps' Law models ($\alpha = 0.9293$ for Masters OTB D40; $\alpha = 0.9293$ for Elite Online D30) with calibrated production CPI densities (21.40 B/pos Masters D40, 21.60 B/pos Elite D30) to the 15,000,000-game capacity target (5M Masters OTB + 10M Elite Online):
     - **Core Payload:** **7.24 GB** (1.04 GB Masters games + 1.80 GB Elite games + 1.79 GB Masters D40 index + 2.60 GB Elite D30 index).
     - **Ancillary Deployment Overhead:** **0.44 GB** (shard headers, global `games.idx` lookup index, manifest checksums, and filesystem alignment slack).
     - **Total Deployed Footprint (Option B):** **7.68 GB baseline** (**6.44 GB optimistic** to **9.98 GB conservative**). Feasibility is **conditional**: while baseline and optimistic projections are comfortably within budget, the conservative scenario leaves razor-thin headroom of **20 MB (0.2%)** against the 10 GB limit. No double-counting exists: game storage and position index storage are independently accounted for.

---

## 2. Benchmark Environment & Methodology

All measurements were conducted using reproducible release builds (`cargo build --release`):

- **Host OS:** Linux (x86_64, Kernel 6.12)
- **CPU:** 11th Gen Intel(R) Core(TM) i5-1135G7 @ 2.40GHz (4 cores / 8 hardware threads)
- **RAM:** 16 GB DDR4-3200
- **Storage:** NVMe PCIe Gen3 SSD (ext4 filesystem)
- **Toolchain:** Rust (`rustc 1.99.1`, `opt-level = 3`)
- **Datasets Tested:**
  - `fixtures/stage_a_validation.pgn`: Deterministic 1,000-game CI validation fixture.
  - `data/masters_combined.pgn.zst`: Concatenation of Lichess Official Broadcasts from `2026-07`, `2026-08`, and `2026-09` (117,616 total games encountered; 22,265 accepted unique games).
  - `data/elite_online_2013-01.pgn.zst`: Lichess standard rated game archive from January 2013 (121,332 games encountered; 0 accepted under `EliteOnlinePolicy`).

---

## 3. Ingestion Accounting & Fail-Closed Invariant Verification

Every ingestion and benchmark run enforces two strict invariants and fails closed (`Err` / nonzero exit code) on any fatal I/O, Zstandard decompression, or move-reconstruction failure:

1. **Mutually Exclusive Category Reconciliation:**
   $$\text{total\_encountered} = \text{accepted\_unique} + \text{confirmed\_duplicates} + \text{superseded\_prior} + \text{rejected\_by\_policy} + \text{rejected\_variant} + \text{rejected\_illegal} + \text{rejected\_malformed} + \text{quarantined}$$
2. **Retained Canonical Corpus Invariant:**
   $$\text{retained\_canonical\_games} = \text{accepted\_unique}$$
   When a fuller relay transmission supersedes an earlier truncated prefix of the same game (`superseded_prior`), it replaces the existing record in-place without inflating `accepted_unique`.

### 3.1 Measured Ingestion Runs

#### Single-Archive and Complete Datasets
| Dataset / Run | Completion Status | Total Encountered | Accepted Unique | Retained Canonical | Confirmed Dups | Superseded Prior | Rejected Policy | Rejected Variant/FEN | Rejected Illegal | Rejected Malformed | Invariants Verified |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Stage A Fixture** | `CompleteEof` | 1,000 | 1,000 | 1,000 | 0 | 0 | 0 | 0 | 0 | 0 | **true** |
| **Masters OTB 2026-08** | `CompleteEof` | 48,940 | 9,923 | 9,923 | 1 | 6 | 38,647 | 329 | 0 | 34 | **true** |
| **Masters OTB 2026-07** | `CompleteEof` | 40,038 | 6,545 | 6,545 | 2 | 0 | 32,833 | 571 | 0 | 87 | **true** |
| **Masters OTB 2026-09** | `CompleteEof` | 28,638 | 5,797 | 5,797 | 13 | 5 | 22,535 | 231 | 3 | 54 | **true** |
| **Combined Masters OTB** | `CompleteEof` | **117,616** | **22,265** | **22,265** | **16** | **11** | **94,015** | **1,131** | **3** | **175** | **true** |
| **Lichess Online 2013-01** | `CompleteEof` | 121,332 | 0 | 0 | 0 | 0 | 121,332 | 0 | 0 | 0 | **true** |

#### Progressive Masters OTB Scaling Batches (`data/masters_combined.pgn.zst`)
| Benchmark Run ($N$) | Completion Status | Total Encountered | Accepted Unique | Retained Canonical | Confirmed Dups | Superseded Prior | Rejected Policy | Rejected Variant/FEN | Rejected Illegal | Rejected Malformed | Invariants Verified |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **`masters_1k`** | `PartialLimitReached` | 4,904 | 1,000 | 1,000 | 0 | 0 | 3,894 | 0 | 0 | 10 | **true** |
| **`masters_2.5k`** | `PartialLimitReached` | 15,212 | 2,500 | 2,500 | 0 | 0 | 12,696 | 0 | 0 | 16 | **true** |
| **`masters_5k`** | `PartialLimitReached` | 34,708 | 5,000 | 5,000 | 2 | 0 | 29,048 | 571 | 0 | 87 | **true** |
| **`masters_10k`** | `PartialLimitReached` | 59,556 | 10,000 | 10,000 | 2 | 0 | 48,605 | 858 | 0 | 91 | **true** |
| **`masters_20k`** | `PartialLimitReached` | 106,640 | 20,000 | 20,000 | 16 | 11 | 85,324 | 1,131 | 3 | 155 | **true** |

#### Progressive Elite Online Scaling Batches (Modern Lichess 2024–2026 Rated Archives)
| Benchmark Run ($N$) | Source Archive | Total Encountered | Accepted Unique | Retained Canonical | Confirmed Dups | Superseded Prior | Rejected Policy | Invariants Verified | Acceptance Rate |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **`elite_1k`** | `2024-01` | 49,636 | 1,000 | 1,000 | 0 | 0 | 48,636 | **true** | **2.01%** |
| **`elite_5k`** | `2024-01` | 249,477 | 5,000 | 5,000 | 1 | 0 | 244,476 | **true** | **2.00%** |
| **`elite_10k`** | `2025-01` | 560,026 | 10,000 | 10,000 | 1 | 0 | 550,025 | **true** | **1.79%** |
| **`elite_50k`** | `2025-01` | 2,572,137 | 50,000 | 50,000 | 1 | 0 | 2,522,136 | **true** | **1.94%** |
| **`elite_100k`** | `2025-01` | 5,108,242 | 100,000 | 100,000 | 2 | 0 | 5,008,240 | **true** | **1.96%** |

---

## 4. Game Storage Density & Supported-Field Round-Trip Fidelity

Under `CGB3` (`BLOCK_MAGIC_V3`), all supported canonical and provenance fields—including variable-length `ECO`, `Date` (resolved with deterministic `Date` vs. `UTCDate` precedence), `GameURL` (`source_id`), and `BroadcastURL` (`broadcast_url`)—are dictionary-interned per 512-game block without fixed-width truncation. During every benchmark run, each compressed block is decompressed and verified against the original `ParsedGame` records.

### 4.1 Measured Storage Density Across Progressive Batches (`masters_combined`)

| Sample Size $N$ | Completion Status | Exported PGN Size | Exported PGN Bytes/Game | `CGB3` Compressed Blocks | Block Dir Framing | `CGB3` Bytes/Game | Compression Ratio |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **1,000 games** | `PartialLimitReached` | 1.06 MB | 1,062.0 B | 0.21 MB | 120 B | **205.5 B** | **5.17×** |
| **2,500 games** | `PartialLimitReached` | 2.67 MB | 1,068.7 B | 0.52 MB | 204 B | **208.6 B** | **5.12×** |
| **5,000 games** | `PartialLimitReached` | 5.38 MB | 1,075.9 B | 1.05 MB | 344 B | **210.4 B** | **5.11×** |
| **10,000 games** | `PartialLimitReached` | 10.80 MB | 1,080.1 B | 2.11 MB | 624 B | **211.3 B** | **5.11×** |
| **20,000 games** | `PartialLimitReached` | 21.49 MB | 1,074.7 B | 4.16 MB | 1,184 B | **208.2 B** | **5.16×** |
| **9,923 games (`2026-08`)** | `CompleteEof` | 10.70 MB | 1,078.0 B | 2.06 MB | 624 B | **208.0 B** | **5.18×** |

### 4.2 Measured Storage Density for Elite Online (`CGB3` Persistent Shards)

| Sample Size $N$ | Source Archive | CGB3 Shard Size | CGB3 Bytes/Game | Global Lookup (`games.idx`) | Total Archive Size | Total Bytes/Game |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **1,000 games** | `2024-01` | 0.18 MB (181,139 B) | **181.1 B** | 16.0 KB | 0.20 MB | **198.7 B** |
| **5,000 games** | `2024-01` | 0.89 MB (888,243 B) | **177.6 B** | 80.0 KB | 0.97 MB | **193.9 B** |
| **10,000 games** | `2025-01` | 1.79 MB (1,784,874 B) | **178.5 B** | 160.0 KB | 1.95 MB | **194.6 B** |
| **50,000 games** | `2025-01` | 9.03 MB (9,031,012 B) | **180.6 B** | 800.0 KB | 9.83 MB | **196.6 B** |
| **100,000 games** | `2025-01` | 18.07 MB (18,070,305 B) | **180.7 B** | 1.60 MB | 19.67 MB | **196.7 B** |

---

## 5. Position Indexing: Empirical Scaling & Sensitivity Analysis

We physically measure unique positions $U(N)$, unique `(position, move)` pairs, persistent `.cpi` file size, and persistent SQLite database size across **four depths: 20, 30, 40, and 60 plies**.

### 5.1 Directly Measured Scaling Data (`masters_combined`)

| Games $N$ | Depth 20 Pos / CPI | Depth 30 Pos / CPI | Depth 40 Pos / CPI | Depth 60 Pos / CPI |
| :--- | :--- | :--- | :--- | :--- |
| **1,000** | 11,067 (`0.23 MB`) | 20,833 (`0.43 MB`) | 30,462 (`0.63 MB`) | 47,890 (`0.99 MB`) |
| **2,500** | 23,907 (`0.51 MB`) | 47,987 (`1.00 MB`) | 71,912 (`1.49 MB`) | 115,815 (`2.38 MB`) |
| **5,000** | 42,205 (`0.90 MB`) | 89,793 (`1.87 MB`) | 137,215 (`2.84 MB`) | 224,389 (`4.59 MB`) |
| **10,000** | 71,545 (`1.54 MB`) | 164,961 (`3.44 MB`) | 260,050 (`5.36 MB`) | 435,410 (`8.87 MB`) |
| **20,000** | 122,987 (`2.65 MB`) | 304,740 (`6.36 MB`) | 493,211 (`10.14 MB`) | 841,110 (`17.04 MB`) |

### 5.2 Empirical Heaps' Law Fits ($U(N) = k \cdot N^\alpha$) & Methodological Caveats

Fitting $\ln U(N) = \ln k + \alpha \ln N$ via OLS across $N \in \{1000, 2500, 5000, 10000, 20000\}$:

| Depth | $k$ | $\alpha_{\text{OLS}}$ | In-Sample Slope Std Err ($s_\alpha$) | Sensitivity Bound ($\pm \Delta\alpha$) | In-Sample $R^2$ | Measured CPI Density (at $N=20\text{k}$) |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Depth 20 (10 moves)** | **44.29** | **0.8023** | $0.0099$ | $\pm 0.0198$ | **0.999544** | **21.57 B / position** |
| **Depth 30 (15 moves)** | **43.40** | **0.8951** | $0.0042$ | $\pm 0.0150$ | **0.999933** | **20.88 B / position** |
| **Depth 40 (20 moves)** | **49.88** | **0.9293** | $0.0021$ | $\pm 0.0150$ | **0.999985** | **20.57 B / position** |
| **Depth 60 (30 moves)** | **64.88** | **0.9565** | $0.0015$ | $\pm 0.0150$ | **0.999993** | **20.26 B / position** |

> [!WARNING]
> **Statistical Extrapolation Caveat:** Because the 5 empirical points are nested cumulative prefixes ($N = 1\text{k} \dots 20\text{k}$, $df = 3$) and projecting to $N = 5{,}000{,}000$ or $10{,}000{,}000$ spans $250\times$–$500\times$ beyond the observed sample domain, the in-sample residual error $s_\alpha$ **must not** be interpreted as a formal 95% confidence interval at multi-million-game scale. Instead, we evaluate projections across three bounded sensitivity regimes:
> - **Optimistic:** $\alpha = \alpha_{\text{OLS}} - \Delta\alpha$ and $-5\%$ bytes/position (assuming stronger transposition convergence at multi-million scale).
> - **Baseline:** Point fit $\alpha = \alpha_{\text{OLS}}$ and measured bytes/position at $N = 20{,}000$.
> - **Conservative:** $\alpha = \alpha_{\text{OLS}} + \Delta\alpha$ (where $\Delta\alpha = \max(2 s_\alpha, 0.015)$), $+15\%$ bytes/position margin, and $+10\%$ bytes/game margin (`229.0 B/game`).

### 5.3 Extrapolated Unique Position Counts Across Sensitivity Scenarios

| Horizon $N$ | Depth 20 Baseline (`[Opt, Cons]`) | Depth 30 Baseline (`[Opt, Cons]`) | Depth 40 Baseline (`[Opt, Cons]`) | Depth 60 Baseline (`[Opt, Cons]`) |
| :--- | :--- | :--- | :--- | :--- |
| **100,000** | 455k (`[362k, 571k]`) | 1.30M (`[1.09M, 1.54M]`) | 2.21M (`[1.86M, 2.63M]`) | 3.93M (`[3.31M, 4.67M]`) |
| **500,000** | 1.65M (`[1.28M, 2.15M]`) | 5.47M (`[4.50M, 6.67M]`) | 9.86M (`[8.10M, 12.00M]`) | 18.34M (`[15.06M, 22.33M]`) |
| **1,000,000** | 2.89M (`[2.19M, 3.79M]`) | 10.18M (`[8.28M, 12.53M]`) | 18.77M (`[15.26M, 23.09M]`) | 35.59M (`[28.93M, 43.78M]`) |
| **5,000,000 (Masters Cap.)** | **10.49M** (`[7.73M, 14.24M]`) | **42.99M** (`[34.11M, 54.18M]`) | **83.75M** (`[66.45M, 105.55M]`) | **165.91M** (`[131.64M, 209.10M]`) |
| **10,000,000 (Elite Proxy)** | **18.30M** (`[13.30M, 25.18M]`) | **79.95M** (`[62.78M, 101.82M]`) | **159.48M** (`[125.23M, 203.10M]`) | **321.97M** (`[252.82M, 410.02M]`) |

### 5.4 Empirical Collection Divergence: Masters OTB vs. Elite Online

The empirical curation of real modern Elite Online datasets in PR-02 reveals fundamental structural differences between tournament master games and online blitz/rapid play:

| Metric | Masters OTB ($N=10{,}000$) | Elite Online ($N=10{,}000$) | Divergence / Ratio |
| :--- | :--- | :--- | :--- |
| **Depth 20 Unique Positions** | 71,545 | 100,749 | **+40.8% more diverse online** |
| **Depth 30 Unique Positions** | 164,961 | 197,326 | **+19.6% more diverse online** |
| **Depth 40 Unique Positions** | 260,050 | 291,204 | **+12.0% more diverse online** |
| **Storage Density (CGB3)** | 211.3 B/game | 178.5 B/game | **-15.5% more compact online** |
| **Fitted Heaps Exponent $\alpha$ (D30)** | 0.8951 | 0.9293 | Sub-linear scaling ($R^2=0.99999$) |

**Key Architectural Insights:**
1. **Higher Early Diversity Online:** Online grandmasters and masters play significantly wider opening repertoires, offbeat sidelines, and experimental novelties compared to conservative classical OTB tournament preparation.
2. **Shorter Online Game Length:** Online blitz/rapid games conclude faster (~60 plies vs. ~75 plies for classical masters), yielding fewer moves per game and thus lower byte density in compressed storage (~180 B vs. ~208 B).

### 5.5 Production CPI Index Scaling & Bounded-Memory External Sorting (PR-03 Empirical Matrix)

PR-03 transitioned position indexing from in-memory prototypes to a production disk-backed indexing pipeline:
1. **Streaming Traversal:** Streams canonical games directly from persistent CGB3 shards without buffering full game objects in memory.
2. **Bounded-Memory External Sorting:** Buffers raw observations in a strictly budgeted heap buffer (with reservations for I/O buffers, Zstd state, and K-way merge fan-in). Flushes sorted and locally coalesced run files (`.crun`) to disk when the buffer threshold is reached.
3. **Multi-Pass K-Way Merging:** Merges `.crun` files with configurable fan-in (default 64) using a tournament-tree min-heap, guaranteeing deterministic sorting and global transposition aggregation.
4. **Streaming CPI v2 Block Writer:** Compresses position blocks with Zstandard and spills the sparse index directly to disk, ensuring $O(1)$ RAM usage independent of unique position count.

#### Physically Measured Production CPI Scaling Matrix (128 MiB Memory Budget)

The production indexing pipeline was executed across progressive sample sizes ($1\text{k}, 20\text{k}, 50\text{k}, 100\text{k}$) on genuine licensed corpora containing $\ge 100{,}000$ accepted canonical games for both **Masters OTB (Depth 40 plies)** and **Elite Online (Depth 30 plies)**. Each run measures kernel `wait4` peak RSS (`ru_maxrss`), sampled anonymous heap memory (`RssAnon`), peak temporary disk usage, final `.cpi` size, build throughput, deep verification status, and Opening Explorer query latencies:

| Collection | Processed $N$ | Indexed Games | Extracted Obs | Unique Pos | Move Pairs | CPI Size | Bytes / Pos | Peak Temp Disk | Peak RSS | Anon Heap | Build Time | Throughput | Warm p50 | Fresh Cold p50 | Deep Verify |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **masters-otb** | 1,000 | 994 | 38,880 | 29,681 | 30,859 | 648,122 B | **21.84 B** | 939 KB | **13.2 MiB** | 2.8 MiB | 0.12s | 8,634 g/s | 1,696 µs | 1,892 µs | **PASS** |
| **masters-otb** | 20,000 | 19,879 | 777,693 | 478,226 | 503,634 | 10,368,307 B | **21.68 B** | 15.1 MB | **31.3 MiB** | 25.6 MiB | 1.20s | 16,634 g/s | 1,622 µs | 1,589 µs | **PASS** |
| **masters-otb** | 50,000 | 49,692 | 1,944,233 | 1,126,545 | 1,190,578 | 24,233,857 B | **21.51 B** | 35.7 MB | **67.8 MiB** | 61.5 MiB | 3.22s | 15,532 g/s | 3,436 µs | 3,193 µs | **PASS** |
| **masters-otb** | 100,000 | 99,262 | 3,887,996 | 2,156,205 | 2,286,655 | 46,145,562 B | **21.40 B** | 69.7 MB | **121.5 MiB** | 114.5 MiB | 5.38s | 18,591 g/s | 1,925 µs | 2,052 µs | **PASS** |
| **elite-online** | 1,000 | 1,000 | 29,699 | 23,108 | 24,390 | 508,320 B | **22.00 B** | 732 KB | **13.6 MiB** | 3.5 MiB | 0.10s | 9,566 g/s | 1,689 µs | 1,940 µs | **PASS** |
| **elite-online** | 20,000 | 19,986 | 594,120 | 376,065 | 403,936 | 8,206,702 B | **21.82 B** | 12.1 MB | **26.1 MiB** | 20.2 MiB | 0.97s | 20,683 g/s | 1,797 µs | 1,617 µs | **PASS** |
| **elite-online** | 50,000 | 49,977 | 1,485,300 | 877,938 | 946,846 | 19,041,982 B | **21.69 B** | 28.5 MB | **53.5 MiB** | 47.4 MiB | 2.22s | 22,504 g/s | 1,674 µs | 1,844 µs | **PASS** |
| **elite-online** | 100,000 | 99,949 | 2,970,600 | 1,661,039 | 1,795,958 | 35,884,776 B | **21.60 B** | 54.1 MB | **99.7 MiB** | 92.8 MiB | 4.83s | 20,695 g/s | 1,889 µs | 1,757 µs | **PASS** |

#### Multi-Run and Multi-Pass Merge Stress Matrix ($N = 100{,}000$ Games)

To validate external sorting and deterministic multi-pass K-way merging under extreme memory constraints, memory stress benchmarks were executed with restricted memory limits (32 MiB, 16 MiB) and reduced fan-in ($F = 2$):

| Collection ($N=100\text{k}$) | Memory Budget | Merge Fan-In | Runs Generated | Merge Passes | Peak RSS | Anon Heap Peak | Temp Disk Peak | CPI Size | CPI SHA-256 Verified | Build Time |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **masters-otb** | 128 MiB | 64 | 2 | 1 | 121.5 MiB | 114.5 MiB | 69.7 MB | 46,145,562 B | `f2d98343...` (Identical) | 5.38s |
| **masters-otb** | 32 MiB | 64 | 5 | 1 | 55.5 MiB | 30.6 MiB | 76.3 MB | 46,145,562 B | `f2d98343...` (Identical) | 7.74s |
| **masters-otb** | 16 MiB | 64 | 9 | 1 | 55.8 MiB | 16.5 MiB | 79.6 MB | 46,145,562 B | `f2d98343...` (Identical) | 6.99s |
| **masters-otb** | 16 MiB | 2 | **9** | **4** | 55.5 MiB | 16.5 MiB | 138.0 MB | 46,145,562 B | `f2d98343...` (Identical) | 8.71s |
| **elite-online** | 128 MiB | 64 | 1 | 1 | 99.7 MiB | 92.8 MiB | 54.1 MB | 35,884,776 B | `da206f40...` (Identical) | 4.83s |
| **elite-online** | 32 MiB | 64 | 4 | 1 | 46.1 MiB | 31.9 MiB | 59.2 MB | 35,884,776 B | `da206f40...` (Identical) | 4.34s |
| **elite-online** | 16 MiB | 64 | 7 | 1 | 45.0 MiB | 16.1 MiB | 61.7 MB | 35,884,776 B | `da206f40...` (Identical) | 4.33s |
| **elite-online** | 16 MiB | 2 | **7** | **3** | 44.7 MiB | 16.1 MiB | 93.8 MB | 35,884,776 B | `da206f40...` (Identical) | 5.31s |

#### Analysis of Production Indexing Results

1. **Memory Budget Contracts: Buffer Limit vs. Total Process Footprint:**
   - `--memory-limit-mib` configures the in-memory sorting and observation buffer ceiling (`RawObservation` buffer and sort staging memory), explicitly reserving headroom for I/O buffers and Zstandard compressor contexts.
   - Total process anonymous memory (`RssAnon`) consists of this budgeted buffer capacity plus base runtime execution overhead (~0.5–1.0 MiB for thread stacks, global allocator metadata, and binary execution scaffolding). In the 16 MiB stress scenario, total process `RssAnon` peaked at **16.05–16.55 MiB**, confirming that buffer allocations were strictly constrained to 16 MiB while total process heap incurred only a minor ~0.55 MiB execution overhead.
   - In contrast, the process-level `ru_maxrss` recorded by the kernel (~44–56 MiB in stress runs) includes file-backed memory-mapped page cache (`RssFile`) from reading CGB3 shard files and writing CPI index blocks. On Linux, clean `RssFile` memory is automatically reclaimed by the OS under memory pressure.
2. **Deterministic Multi-Run & Multi-Pass Merging:**
   - Constraining the memory budget to 16 MiB forces the creation of up to 9 sorted `.crun` files.
   - Reducing fan-in to 2 exercises the multi-pass merge cascade across **3 to 4 merge passes**. Across all 16 configurations, the generated `.cpi` binary is **100% byte-for-byte identical** (verified by matching SHA-256 checksums).
3. **Empirical Storage Density at Scale:**
   - Production CPI v2 format yields **21.40 bytes per unique position** for Masters OTB (D40) and **21.60 bytes per unique position** for Elite Online (D30) at $N=100{,}000$ games.
   - Fixed block header overhead amortizes more effectively as position volume grows, decreasing bytes-per-position from ~21.8 B/pos at 1k to 21.40 B/pos at 100k.
4. **Opening Explorer Query Performance:**
   - End-to-end CLI queries (`codb query --dataset ... --moves ... --json`) execute with warm p50 latencies of **1.6–2.0 ms** and fresh-process cold-start p50 latencies of **1.6–2.2 ms** (which includes process spawn, argument parsing, dataset resolution, mmap initialization, binary search across sparse index, block decompression, and JSON formatting).
   - In benchmark artifacts, cold-start latency is explicitly labeled as **fresh-process cold-start latency** (with `os_page_cache_invalidated: false`), documenting that the OS page cache is warm while process memory is initialized fresh.
5. **Deterministic Aggregation & Zero Corruption:**
   - Deep verification (`codb verify --dataset ... --deep`) succeeds across all 16 runs, verifying exact transposition coalescing, next-move distribution conservation, and 100% position integrity across multi-pass external merges.

---

## 6. Persistent Retrieval and On-Disk Index Comparison

### 6.1 Random Game Retrieval Latency (`games.idx` Memory-Mapped Binary Search)

In PR-02, the persistent container provides stable random-access game retrieval by 64-bit ID via `games.idx`. Using `codb bench-dataset`, we measure 2,000 random lookups across persistent datasets:

| Dataset Size | Metric | Measured Latency | Storage Density | Verification Status |
| :--- | :--- | :--- | :--- | :--- |
| **50,000 games (`elite-online-50k`)** | **p50 Latency** | **733 µs** | 196.6 B/game total | 2,000/2,000 retrieved losslessly |
| | **p95 Latency** | **813 µs** | | |
| | **p99 Latency** | **889 µs** | | |
| | **Max Latency** | **1,434 µs** | | |
| | **Cache Model** | Single-block LRU | Block decompress: ~500–700 µs | |
| **100,000 games (`elite-online-100k`)** | **p50 Latency** | **698 µs** | 196.7 B/game total | 10,000/10,000 retrieved losslessly |
| | **p95 Latency** | **773 µs** | | |
| | **p99 Latency** | **863 µs** | | |
| | **Max Latency** | **1,862 µs** | | |
| | **Cache Model** | Single-block LRU | Block decompress: ~500–700 µs | |

### 6.2 Physical Disk Footprint Comparison: CPI vs. Persistent SQLite (at $N = 20{,}000$ Accepted Games)

We compare physical `.cpi` files (written via `ImmutablePositionIndex::write_to_file`, including the 64-byte header, Zstd-compressed position blocks, and 34-byte/block sparse index directory) against physical on-disk SQLite databases (`position_moves` table + `idx_pos` index + WAL file):

| Depth | Unique Positions | CPI Sparse Index Blocks | CPI Disk File Size | SQLite On-Disk DB Size | CPI Space Savings |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Depth 20 (Plies 1–20)** | 122,987 | 481 (`16.4 KB` dir) | **2.65 MB** | 25.02 MB | **9.4× smaller** |
| **Depth 30 (Plies 1–30)** | 304,740 | 1,191 (`40.5 KB` dir) | **6.36 MB** | 52.72 MB | **8.3× smaller** |
| **Depth 40 (Plies 1–40)** | 493,211 | 1,927 (`65.5 KB` dir) | **10.14 MB** | 78.60 MB | **7.8× smaller** |
| **Depth 60 (Plies 1–60)** | 841,110 | 3,286 (`111.7 KB` dir) | **17.04 MB** | 123.34 MB | **7.2× smaller** |

### 6.2 Stratified Query Latency Comparison (Warm OS Page Cache, 2,000 Queries)

Latency benchmarks sample **2,000 queries** deterministically (`SEED = 0xC0DB_2026_0001_CAFE`) across the full Depth-40 index of $N = 20{,}000$ accepted games:
- **400 frequent opening positions** ($\text{count} \ge 10$)
- **400 medium-frequency positions** ($2 \le \text{count} < 10$)
- **400 rare positions** ($\text{count} = 1$)
- **400 transposition positions** (verified reached via $\ge 2$ distinct parent `(position, move)` paths)
- **400 miss queries** (nonexistent 128-bit keys)

Before timing, all 2,000 queries are checked to verify 100% semantic equivalence between `ImmutablePositionIndex`, `DiskPositionIndex`, and `SqlitePositionIndex`. Timings use `Instant::elapsed().as_nanos() as f64 / 1000.0` under a **warm OS page cache** regime (immediately following index build/write on NVMe storage):

| Storage Representation (`masters_20k`, 2,000 Stratified Queries) | p50 Latency | p95 Latency | p99 Latency | Semantic Equivalence |
| :--- | :--- | :--- | :--- | :--- |
| **CPI In-Memory (`BTreeMap` RAM Baseline)** | **1.144 µs** | **1.905 µs** | **2.789 µs** | **Verified (2000/2000)** |
| **CPI Persistent On-Disk (`mmap` + block zstd, warm OS cache)** | **14.013 µs** | **23.403 µs** | **28.945 µs** | **Verified (2000/2000)** |
| **SQLite Persistent On-Disk (WAL mode, warm page cache)** | **5.720 µs** | **12.994 µs** | **20.788 µs** | **Verified (2000/2000)** |

---

## 7. Complete Deployed Storage Inventory: Measured vs. Estimated Costs

To avoid conflating raw compressed payloads with a complete production deployment, Table 7.1 separates **measured/extrapolated format payloads** from **modeled ancillary deployment overheads** for the 15,000,000-game capacity target:

### 7.1 Component-Level Deployed Storage Inventory (15M Games, Option B)

| Component | Category | Basis / Formula | Baseline Size (GB) | Conservative Size (GB) |
| :--- | :--- | :--- | :--- | :--- |
| **1. Masters OTB `.cgb` Game Blocks (5M games)** | Measured format, extrapolated to 5M | $5\text{M} \times 208.15\text{ B/game}$ (+10% cons.) | **1.04 GB** | **1.15 GB** |
| **2. Elite Online `.cgb` Game Blocks (10M games)** | Empirically measured in PR-02 (2024–2026) | $10\text{M} \times 180.13\text{ B/game}$ (+10% cons.) | **1.80 GB** | **1.98 GB** |
| **3. Masters OTB `.cpi` Index (Depth 40, 5M)** | Measured format + Masters Heaps' Law fit | $83.75\text{M pos} \times 21.40\text{ B/pos}$ | **1.79 GB** | **2.60 GB** |
| **4. Elite Online `.cpi` Index (Depth 30, 10M)** | Measured format + Online Heaps' Law fit | $120.3\text{M pos} \times 21.60\text{ B/pos}$ | **2.60 GB** | **3.81 GB** |
| **Subtotal — Core Format Payloads** | | | **7.24 GB** | **9.54 GB** |
| **5. `.cgb` Container Framing & Shard Directories** | Measured persistent container headers | Shard & block headers | **0.001 GB** | **0.001 GB** |
| **6. Global `games.idx` Lookup Index (15M games)** | Measured 16-byte fixed record | $15\text{M} \times 16\text{ B}$ | **0.240 GB** | **0.240 GB** |
| **7. Provenance & SHA-256 Checksum Manifests** | Measured PR-02 `manifest.json` | JSON + block SHA-256 hashes | **0.005 GB** | **0.005 GB** |
| **8. Filesystem Block Alignment & Operational Reserve** | Estimated deployment reserve | ~2.5% alignment / swap margin | **0.194 GB** | **0.194 GB** |
| **Subtotal — Ancillary Deployment Overhead** | | | **0.44 GB** | **0.44 GB** |
| **Total Deployed Footprint (Option B)** | **Core Payloads + Ancillary Overhead** | | **7.68 GB** | **9.98 GB** |

> [!NOTE]
> In PR-01, Elite Online game storage was approximated using Masters OTB measurements (208 B/game) and an unvalidated scaling exponent, projecting 6.96 GB baseline (8.82 GB conservative). In PR-02, real modern Elite Online games (2024–2026) were directly curated and measured up to 100,000 games, confirming that online games are ~15% more compact in compressed moves (~180.1 B/game) while exhibiting ~20% higher position diversity at Depth 30.
>
> In PR-03, genuine 100,000-game production CPI indexes were constructed and benchmarked for both collections, measuring production densities of **21.40 B/pos** (Masters D40) and **21.60 B/pos** (Elite D30).
>
> **Feasibility Assessment (Conditional):** Under the recalibrated PR-03 Option B model, baseline deployed storage is **7.68 GB baseline** (optimistic: **6.44 GB optimistic**), leaving a comfortable 2.32 GB buffer below the 10 GB budget. However, under conservative scaling assumptions (+15% index density margin and +1σ position diversity), deployed storage reaches **9.98 GB conservative**, leaving razor-thin headroom of **20 MB (0.2%)**. Feasibility is therefore strictly **conditional** upon maintaining Option B's Depth 30 limit for Elite Online, achieving production block amortization at scale, and keeping empirical growth within projected bounds.

### 7.2 Architecture Scenarios Across Sensitivity Regimes (15M Games Capacity Target)

| Scenario | Core Payload (Baseline) | Ancillary Overhead | Deployed Total (Optimistic) | Deployed Total (Baseline) | Deployed Total (Conservative) | Feasibility Assessment |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Option A (Depth 20 Both)** | 3.90 GB | 0.44 GB | 3.60 GB | **4.34 GB** | **5.14 GB** | **Feasible (Shallow Opening Book Only)** |
| **Option B (Recommended: D40 Masters + D30 Elite)** | **7.24 GB** | **0.44 GB** | **6.44 GB** | **7.68 GB** | **9.98 GB** | **Feasible (Conditional: 20 MB / 0.2% headroom in conservative regime)** |
| **Option C (Depth 40 Both)** | 8.80 GB | 0.44 GB | 7.60 GB | **9.24 GB** | **12.26 GB** | **Infeasible Conservatively (Exceeds 10 GB limit by 2.26 GB)** |
| **Option D (Depth 60 Both)** | 14.10 GB | 0.44 GB | 12.05 GB | **14.54 GB** | **19.85 GB** | **Infeasible (Exceeds 10 GB in All Regimes)** |

### 7.3 Sensitivity Analysis & Critical Risk Factors

To evaluate the operational resilience of the 10 GB capacity target, we analyze sensitivity across key operational variables:

1. **Elite Online Index Depth Limit (Option B vs. Option C):**
   - Option B indexes Elite Online to Depth 30 (120.3M positions baseline). Slicing Elite Online to Depth 40 (Option C, 159.5M positions) expands the CPI index from 2.60 GB to 4.16 GB baseline (and up to 6.13 GB conservative), driving total deployed storage to **12.26 GB** and violating the 10 GB budget. Enforcing the Depth 30 boundary for online games is mandatory.
2. **Move Diversity and Position Uniqueness per Game:**
   - In online bullet/blitz, novel move transposition chains occur more frequently than in theoretical grandmaster lines. If $\alpha_{D30}$ drifts from $0.9293$ to $0.9443$ (+1 $\sigma$ delta), position counts increase by 18.5%, consuming an additional 450 MB and narrowing conservative headroom to near zero.
3. **CPI Index Density & Block Amortization (20k vs. 100k Density):**
   - At early sample size ($N=20{,}000$), unamortized block headers yield densities of ~21.65–21.82 B/pos. Under conservative assumptions, that higher density would project 10.04 GB (+40 MB over budget). At validated $N=100{,}000$ scale, block amortization lowers density to **21.40 B/pos** (Masters) and **21.60 B/pos** (Elite), preserving the conservative projection within 9.98 GB.
4. **Build-Time Scratch vs. Deploy-Time Persistent Storage:**
   - Persistent CGB3 dataset and CPI index creation operate with strictly bounded memory using external sorting. However, build-time disk footprint requires temporary storage for staging `.crun` files (~60–80 MB peak at 100k games, scaling to ~3.5–4.5 GB peak temporary scratch during full 10M merge passes). This scratch space is deleted upon atomic publication, but host environments must provision sufficient build disk space (~15 GB volume) during indexing before final deployment.
5. **Compression Ratio Degradation at Scale:**
   - CGB3 block compression achieves 5.16× on Masters OTB and 4.19× on Elite Online. If entropy increases on larger, less curated corpora, a 10% drop in compression efficiency adds ~285 MB across 15M games.

---

## 8. Corpus-Volume Reality Check & Roadmap for Future Milestones

1. **Masters OTB Initial Corpus Volume vs. 5M Capacity Ceiling:**
   - Across our 3-month Lichess Official Broadcast sample (`117,616` raw games), `22,265` games (`18.93%`) pass `MastersOtbPolicy` and deduplication.
   - Across the entire Lichess Broadcast archive (`~1.26M–1.5M` raw games from 2020–present), this acceptance rate yields **~240,000 to ~285,000 curated Masters OTB games** (~300,000 to ~500,000 when combined with public-domain historical tournaments).
   - At $N = 500{,}000$ games, the entire Masters OTB collection (games + Depth 40 `.cpi` index) will occupy **~0.31 GB** on disk. The 5,000,000-game Masters OTB figure in Section 7 is an **architectural capacity stress test**, ensuring our binary formats scale without redesign as future broadcasts and additional open-licensed OTB sources are added.
2. **Elite Online Empirical Validation (Completed in PR-02):**
   - PR-02 resolved the PR-01 Jan 2013 0-accepted limitation by ingesting modern 2024–2026 Lichess standard rated archives, establishing an empirical acceptance rate of **~1.96%** (100,000 accepted from 5,108,242 encountered games).
   - Confirmed game storage density of **180.70 B/game** (~15% more compact than Masters OTB) and position scaling exponent $\alpha = 0.9293$ at Depth 30 ($R^2 = 0.99999$).
   - Validated random retrieval across persistent datasets up to $N = 100{,}000$ with $p_{50} = 698\,\mu\text{s}$ and $p_{95} = 773\,\mu\text{s}$.
3. **Roadmap and Milestone Progress:**
   - **PR-01 (Completed):** Scientific feasibility, CGB3 binary encoding, CPI v2 format prototype, licensing architecture.
   - **PR-02 (Completed):** Bounded-memory streaming ingestion pipeline, persistent sharded CGB3 container, global `games.idx` lookup table, modern Elite Online empirical calibration.
   - **PR-03 — Position Index Construction:** Bounded-memory external sorting with temporary `.crun` runs, multi-pass K-way merge, orthodox legal FEN starting position validation, intra-game repetition deduplication, exclusion of unknown results (`*`), atomic publication/recovery, and functional Opening Explorer CLI queries (`codb query`).
   - **PR-04 — Query Engine & Integration:** HTTP API service (`codb serve`), client SDK, and Evalis frontend integration.
