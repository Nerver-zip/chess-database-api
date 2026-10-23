# Benchmark Reproduction & Research Methodology Guide

This document establishes the exact, step-by-step reproduction procedure for all empirical benchmarks, ingestion accounting figures, position-growth scaling measurements, and derived storage projections published in PR-01.

---

## 1. Environment & Prerequisites

Reproduction is supported on any modern Linux development machine (x86_64 or aarch64):

- **Toolchain:** Rust (`stable`, 1.80+ or 1.99+) with `cargo`
- **Compression Tool:** `zstd` CLI (v1.5+)
- **Analysis:** Python 3.8+ (standard library only; no external pip dependencies)
- **C Compiler:** `gcc` or `clang` (required for bundled `zstd-sys` and `rusqlite`)

---

## 2. Dataset Sources & Integrity Verification

### 2.1 Official Sources

All external datasets are sourced from the official [Lichess Open Database](https://database.lichess.org/):

| Dataset Identifier | Canonical File Path | Source URL | Expected Uncompressed Games | SHA-256 Checksum |
| :--- | :--- | :--- | ---: | :--- |
| **Stage A Fixture** | `fixtures/stage_a_validation.pgn` | *Committed in Git repository* | 1,000 | `fbfb01e3eeefc54e7d4db5c510b642646d47cfbf426c5188bfb92784ee9b35bc` |
| **Broadcast 2026-07** | `data/masters_otb_2026-07.pgn.zst` | `https://database.lichess.org/broadcast/lichess_db_broadcast_2026-07.pgn.zst` | 40,038 | `714d0eb99f99fca8d791142038b6c59b5ca6a51b3339bd3891a92f4bdffcbf0c` |
| **Broadcast 2026-08** | `data/masters_otb_2026-08.pgn.zst` | `https://database.lichess.org/broadcast/lichess_db_broadcast_2026-08.pgn.zst` | 48,940 | `e227c35c3207ebade754849c1825982e2717ae82c013013d896778e27be77724` |
| **Broadcast 2026-09** | `data/masters_otb_2026-09.pgn.zst` | `https://database.lichess.org/broadcast/lichess_db_broadcast_2026-09.pgn.zst` | 28,638 | `27a39d035767a30901b89602f33f2a7b27d4ab5025842071165fa079f8fff1df` |
| **Rated 2013-01** | `data/elite_online_2013-01.pgn.zst` | `https://database.lichess.org/standard/lichess_db_standard_rated_2013-01.pgn.zst` | 121,332 | `aa40b3671fa3cf1072eb182892cd90b0e1e003a4a5943492f64b77e7f3fd1635` |

### 2.2 Verifying Checksums

Verify source files in `data/`:
```bash
sha256sum data/*.pgn.zst
```

---

## 3. Deterministic Combined Corpus Construction

The progressive scaling study evaluates game samples up to $N = 20{,}000$ accepted games against the multi-month combined archive `data/masters_combined.pgn.zst`.

To preserve deterministic deduplication and reproducible position-growth measurements, the archive **must** be assembled by sequential chronological concatenation:
1. `2026-07` (July 2026)
2. `2026-08` (August 2026)
3. `2026-09` (September 2026)

Execute:
```bash
zstd -dc \
  data/masters_otb_2026-07.pgn.zst \
  data/masters_otb_2026-08.pgn.zst \
  data/masters_otb_2026-09.pgn.zst \
  | zstd -3 -o data/masters_combined.pgn.zst
```

**Verification:**
- Total uncompressed games encountered: **117,616**
- Uncompressed stream SHA-256: `9cc0f844cf8c788e76ad13f8c634beb19dc73f55018a124c2df00aa7ecc716e7`
- Compressed archive SHA-256: `1302f9191e4cdd12577aa88357cfc6bb895775aca5114b7e016ae43995ace2e8`

---

## 4. Benchmark Execution Procedure

### 4.1 Automated One-Command Reproduction

Run the automated research harness:
```bash
./scripts/reproduce_benchmarks.sh --all
```

For CI environments where external archives are not present, execute the lightweight test:
```bash
./scripts/reproduce_benchmarks.sh --ci
```

### 4.2 Manual Step-by-Step Execution

#### Step 1: Compile Release Binary
```bash
cargo build --release -p codb-cli
BIN="./target/release/codb"
```

#### Step 2: Stage A Deterministic Fixture Benchmark
```bash
"$BIN" bench \
  --input fixtures/stage_a_validation.pgn \
  --collection masters-otb \
  --output-dir benchmarks/results \
  --run-name stage_a_validation
```

#### Step 3: Stage B Single-Archive Ingestion Benchmarks
```bash
# August 2026 Masters OTB
"$BIN" bench \
  --input data/masters_otb_2026-08.pgn.zst \
  --collection masters-otb \
  --output-dir benchmarks/results \
  --run-name stage_b_masters_otb

# January 2013 Elite Online
"$BIN" bench \
  --input data/elite_online_2013-01.pgn.zst \
  --collection elite-online \
  --output-dir benchmarks/results \
  --run-name stage_b_elite_online
```

#### Step 4: Progressive Masters OTB Scaling Series
```bash
for N in 1000 2500 5000 10000 20000; do
  "$BIN" bench \
    --input data/masters_combined.pgn.zst \
    --collection masters-otb \
    --max-games "$N" \
    --output-dir benchmarks/results \
    --run-name "masters_${N/000/k}"
done
```
*(Note: for $N = 2500$, the file name generated is `masters_2.5k.json`).*

#### Step 5: Empirical Heaps' Law Fitting & Storage Projections
```bash
python3 scripts/fit_heaps.py
```
This reads `benchmarks/results/masters_{1k,2.5k,5k,10k,20k}.json`, computes OLS log-linear regressions for Depths 20, 30, 40, and 60, derives sensitivity bounds, and generates `benchmarks/results/projections_heaps.json`.

#### Step 6: Automated Schema & Accounting Invariant Validation
```bash
python3 scripts/validate_benchmarks.py
```
Enforces that every artifact strictly complies with the schema, accounting reconciliation invariants, monotonicity rules, and latency percentile constraints.

---

## 5. Deterministic vs. Hardware-Dependent Metrics

When reproducing benchmarks on different hardware environments:

### 5.1 Exact Deterministic Metrics
The following metrics **must reproduce bit-for-bit identically**:
- `total_encountered`
- `accepted_unique` and `retained_canonical_games`
- `confirmed_duplicates`, `superseded_prior`, `rejected_by_policy`, `rejected_variant`, `rejected_illegal`, `rejected_malformed`
- `unique_positions` at Depths 20, 30, 40, and 60
- `unique_position_move_pairs`
- `cpi_block_count` and `cpi_sparse_directory_bytes`
- Deterministic query sampling identities (`SEED = 0xC0DB_2026_0001_CAFE`)
- `semantic_equivalence_verified == true`

### 5.2 Expected Physical Size Minor Variances
- `immutable_index_size_bytes` (`.cpi`) and `codb_block_compressed_bytes` (`CGB3`): Fixed across platforms with identical Zstd compression level (Level 3).
- `sqlite_index_size_bytes`: SQLite page allocation, B-tree balance, and WAL checkpoint state can vary across SQLite versions by $\pm 1\%$.

### 5.3 Hardware-Dependent Metrics
The following metrics naturally vary depending on CPU frequency, core count, RAM speed, and storage media:
- `import_duration_secs`, `import_throughput_games_sec`, `import_throughput_mb_sec`
- `index_build_duration_secs`
- `query_latency_us` (`p50`, `p95`, `p99`)
