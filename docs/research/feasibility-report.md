# Research Track F: Storage Feasibility Report & Empirical Scaling Analysis

## 1. Executive Summary

This report delivers the foundational engineering evaluation for the **Chess Openings DB** project, addressing the central question of PR-01:

> **Can we build a reliable, reproducible, publicly redistributable chess database containing millions of high-quality games, with full game retrieval and position-based opening statistics, within approximately 10 GB of deployed storage?**

### Conclusion: **Preliminary Feasibility Supported — Full-Scale Validation Pending**

Empirical measurements across 117,616 real Lichess Official Broadcast games (`22,265` accepted after curation and prefix-verified deduplication) and 121,332 Lichess Standard Rated games support preliminary storage feasibility within the 10 GB target, subject to explicit scaling assumptions and corpus-coverage boundaries:

1. **Directly Demonstrated in PR-01 (Measured Evidence):**
   - **Lossless Supported-Field Storage (`CGB3`):** Blocks of 512 games achieve **205.5–211.3 bytes/game** (`208.2 B/game` at $N = 20{,}000$, a **5.16×** compression ratio over exported PGN) while preserving 100% round-trip fidelity across all supported canonical and provenance fields (`White`, `Black`, `WhiteElo`, `BlackElo`, `Date` with `UTCDate` precedence, `Event`, `Site`, `Round`, `Result`, `ECO`, `TimeControl`, `GameURL` as `source_id`, `BroadcastURL` as `broadcast_url`, `FEN`, and legal moves).
   - **On-Disk Position Index (`CODB_CPI` v2 vs. SQLite):** Measured directly at Depths **20, 30, 40, and 60** up to $N = 20{,}000$ accepted games. At $N = 20{,}000$, persistent `.cpi` files consume **20.26–21.57 bytes per unique position** (including the 64-byte header and 34-byte/block sparse index directory), which is **7.2× to 9.4× smaller on disk than equivalent SQLite tables**.
   - **Stratified Query Latency & Semantic Equivalence:** Across 2,000 deterministically sampled queries per run (frequent, medium, rare, transposition, and miss keys; verified for 100% result equivalence between CPI and SQLite), warm-cache on-disk `.cpi` lookups complete in **13.1–14.0 µs (p50)**, **21.5–23.4 µs (p95)**, and **23.7–29.0 µs (p99)**, while in-memory lookups complete in **0.82–1.14 µs (p50)** and **2.38–2.79 µs (p99)**.

2. **Modeled Full-Scale Projections (15 Million Games Capacity Target):**
   - Extrapolating from $N = 20{,}000$ to a **15,000,000-game capacity ceiling** (5M Masters OTB + 10M Elite Online) using our recommended **Option B** architecture (Depth 40 for Masters OTB, Depth 30 for Elite Online):
     - **Core Measured-Format Payloads (`.cgb` blocks + `.cpi` indexes):** **~6.52 GB** baseline (`1.04 GB` Masters games + `2.08 GB` Elite games + `1.72 GB` Masters D40 index + `1.67 GB` Elite D30 index).
     - **Estimated Ancillary Deployment Overheads (`.cgb` block directory framing, global `game_id` lookup index, provenance manifests, and filesystem alignment reserve):** **~0.44 GB**.
     - **Total Projected Deployed Footprint (Option B):** **~6.96 GB baseline** (ranging from **5.95 GB optimistic** to **8.82 GB conservative** under bounded sensitivity scenarios), remaining below the 10 GB ceiling.
   - **Boundary Condition (Depth 60 Rejected):** Indexing to Depth 60 (30 full moves) projects to **13.44 GB baseline** (**18.29 GB conservative**), exceeding the 10 GB budget.

3. **Explicit Evidence Gaps & Caveats for PR-02:**
   - **Out-of-Sample Extrapolation ($250\times$–$500\times$):** Heaps' Law parameters are fitted over nested prefixes from $N = 1{,}000$ to $N = 20{,}000$ accepted games. Projections to 5M–10M games are presented with bounded sensitivity scenarios rather than formal statistical confidence intervals.
   - **Elite Online Proxy Assumption:** In the tested early Lichess archive (`2013-01`, 121,332 games), `0` games satisfied `EliteOnlinePolicy` (121,330 failed the $\ge 2200$ rating threshold; 2 were excluded as $< 180\text{s}$ bullet). Consequently, Elite Online storage and position-growth projections currently use Masters OTB scaling parameters as an unvalidated proxy until modern multi-month Lichess archives are ingested in PR-02.
   - **Masters OTB Source Volume vs. Capacity Target:** The full Lichess Broadcast archive (~1.26M–1.5M raw games) yields **~240,000–285,000 accepted Masters OTB games** at the measured 18.9% acceptance rate (~300,000–500,000 games when combined with public-domain historical events). Thus, an initial legally clean Masters OTB release will consume **< 0.45 GB**, while the 5,000,000-game figure represents a long-term architectural capacity ceiling rather than immediate yield from Lichess Broadcasts alone.

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

| Dataset / Run | Completion Status | Total Encountered | Accepted Unique | Retained Canonical | Confirmed Dups | Superseded Prior | Rejected Policy | Rejected Variant/FEN | Rejected Illegal | Rejected Malformed | Invariants Verified |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Stage A Fixture** | `CompleteEof` | 1,000 | 1,000 | 1,000 | 0 | 0 | 0 | 0 | 0 | 0 | **true** |
| **Masters OTB 2026-08** | `CompleteEof` | 48,940 | 9,923 | 9,923 | 1 | 6 | 38,681 | 329 | 0 | 0 | **true** |
| **Masters OTB 2026-07** | `CompleteEof` | 40,038 | 6,545 | 6,545 | 2 | 0 | 32,920 | 571 | 0 | 0 | **true** |
| **Masters OTB 2026-09** | `CompleteEof` | 28,638 | 5,797 | 5,797 | 13 | 5 | 22,589 | 231 | 3 | 0 | **true** |
| **Combined Masters OTB** | `CompleteEof` | **117,616** | **22,265** | **22,265** | **16** | **11** | **94,190** | **1,131** | **3** | **0** | **true** |
| **Lichess Online 2013-01** | `CompleteEof` | 121,332 | 0 | 0 | 0 | 0 | 121,332 | 0 | 0 | 0 | **true** |

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

---

## 6. On-Disk Index Comparison: CPI vs. Persistent SQLite

We compare physical `.cpi` files (written via `ImmutablePositionIndex::write_to_file`, including the 64-byte header, Zstd-compressed position blocks, and 34-byte/block sparse index directory) against physical on-disk SQLite databases (`position_moves` table + `idx_pos` index + WAL file):

### 6.1 Physical Disk Footprint Comparison (at $N = 20{,}000$ Accepted Games)

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
| **CPI Persistent On-Disk (`mmap` + block zstd, warm OS cache)** | **14.010 µs** | **23.400 µs** | **28.950 µs** | **Verified (2000/2000)** |
| **SQLite Persistent On-Disk (WAL mode, warm page cache)** | **5.720 µs** | **12.990 µs** | **20.790 µs** | **Verified (2000/2000)** |

---

## 7. Complete Deployed Storage Inventory: Measured vs. Estimated Costs

To avoid conflating raw compressed payloads with a complete production deployment, Table 7.1 separates **measured/extrapolated format payloads** from **modeled ancillary deployment overheads** for the 15,000,000-game capacity target:

### 7.1 Component-Level Deployed Storage Inventory (15M Games, Option B)

| Component | Category | Basis / Formula | Baseline Size (GB) | Conservative Size (GB) |
| :--- | :--- | :--- | :--- | :--- |
| **1. Masters OTB `.cgb` Game Blocks (5M games)** | Measured format, extrapolated to 5M | $5\text{M} \times 208.16\text{ B/game}$ (+10% cons.) | **1.04 GB** | **1.15 GB** |
| **2. Elite Online `.cgb` Game Blocks (10M games)** | Proxy-extrapolated from Masters `CGB3` | $10\text{M} \times 208.16\text{ B/game}$ (+10% cons.) | **2.08 GB** | **2.29 GB** |
| **3. Masters OTB `.cpi` Index (Depth 40, 5M)** | Measured format + Heaps' Law fit (incl. 34 B/block sparse dir) | $83.75\text{M pos} \times 20.57\text{ B/pos}$ | **1.72 GB** | **2.50 GB** |
| **4. Elite Online `.cpi` Index (Depth 30, 10M)** | Direct D30 fit applied as proxy (incl. 34 B/block sparse dir) | $79.95\text{M pos} \times 20.88\text{ B/pos}$ | **1.67 GB** | **2.44 GB** |
| **Subtotal — Core Format Payloads** | | | **6.52 GB** | **8.38 GB** |
| **5. `.cgb` Container Framing & Block Offset Directory** | Measured struct (`64 B + 28 B/block`) | $29{,}297\text{ blocks} \times 28\text{ B}$ | **0.001 GB** | **0.001 GB** |
| **6. Global `game_id -> (block, idx)` Lookup Index** | Estimated PR-02 index (`12 B/game`) | $15\text{M} \times 12\text{ B}$ | **0.180 GB** | **0.180 GB** |
| **7. Provenance & SHA-256 Block Checksum Manifests** | Estimated PR-02 `provenance.json` | JSON + per-block SHA-256 hashes | **0.005 GB** | **0.005 GB** |
| **8. Filesystem Block Alignment & Operational Margin** | Estimated deployment reserve | ~4% alignment / swap / metadata slack | **0.254 GB** | **0.254 GB** |
| **Subtotal — Ancillary Deployment Overhead** | | | **0.44 GB** | **0.44 GB** |
| **Total Deployed Footprint (Option B)** | **Core Payloads + Ancillary Overhead** | | **6.96 GB** | **8.82 GB** |

> [!NOTE]
> **Optional Unimplemented Index Extensions (Deferred to PR-02):**
> - **Representative `top_games` IDs per position:** If a single representative 64-bit game ID (`8 B`) is embedded per unique position across 163.7M combined positions, it adds **~1.31 GB** raw (~0.65 GB compressed), which still fits within the 6.96 GB baseline budget (~7.61 GB total).
> - **Multi-dimensional Rating $\times$ Era $\times$ Speed pre-materialized slices:** Explicitly rejected for static full-depth materialization, as multiplying index tables by 10–18× would exceed 10 GB. If slice filtering is required in PR-02, it will be limited to shallow opening books (Depth $\le 16$) or computed via compact bitsets.

### 7.2 Architecture Scenarios Across Sensitivity Regimes (15M Games Capacity Target)

| Scenario | Core Payload (Baseline) | Ancillary Overhead | Deployed Total (Optimistic) | Deployed Total (Baseline) | Deployed Total (Conservative) | Feasibility Assessment |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Option A (Depth 20 Both)** | 3.74 GB | 0.44 GB | 3.45 GB | **4.18 GB** | **4.85 GB** | **Feasible (Shallow Opening Book Only)** |
| **Option B (Recommended: D40 Masters + D30 Elite)** | **6.52 GB** | **0.44 GB** | **5.95 GB** | **6.96 GB** | **8.82 GB** | **Feasible Under All Tested Scenarios (< 10 GB)** |
| **Option C (Depth 40 Both)** | 8.13 GB | 0.44 GB | 7.21 GB | **8.57 GB** | **11.18 GB** | **Marginal (Exceeds 10 GB in Conservative Case)** |
| **Option D (Depth 60 Both)** | 13.00 GB | 0.44 GB | 11.12 GB | **13.44 GB** | **18.29 GB** | **Infeasible (Exceeds 10 GB in All Regimes)** |

---

## 8. Corpus-Volume Reality Check & PR-02 Validation Roadmap

1. **Masters OTB Initial Corpus Volume vs. 5M Capacity Ceiling:**
   - Across our 3-month Lichess Official Broadcast sample (`117,616` raw games), `22,265` games (`18.93%`) pass `MastersOtbPolicy` and deduplication.
   - Across the entire Lichess Broadcast archive (`~1.26M–1.5M` raw games from 2020–present), this acceptance rate yields **~240,000 to ~285,000 curated Masters OTB games** (~300,000 to ~500,000 when combined with public-domain historical tournaments).
   - At $N = 500{,}000$ games, the entire Masters OTB collection (games + Depth 40 `.cpi` index) will occupy **~0.31 GB** on disk. The 5,000,000-game Masters OTB figure in Section 7 is an **architectural capacity stress test**, ensuring our binary formats scale without redesign as future broadcasts and additional open-licensed OTB sources are added.
2. **Elite Online Empirical Validation (Priority 1 for PR-02):**
   - Because `data/elite_online_2013-01.pgn.zst` had `0` accepted games under `EliteOnlinePolicy`, PR-02 must ingest a modern Lichess rated monthly archive (e.g., 2024–2026) to directly measure Elite Online acceptance rate, bytes/game, and Heaps' Law exponent $\alpha_{\text{online}}$ at Depth 30.
3. **Cold-Cache vs. Warm-Cache Latency Characterization:**
   - PR-01 latency figures reflect warm OS page cache conditions on NVMe storage. PR-02 will add explicit cold-cache (`posix_fadvise` / `DONTNEED`) benchmarks at multi-gigabyte index sizes.
