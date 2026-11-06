# Historical Benchmark Artifacts

This directory archives completed milestone benchmark artifacts and historical research datasets. Current production baselines are maintained in [`benchmarks/production_cpi/`](../../production_cpi/).

## Catalog of Historical Artifacts

### 1. Prototype Exploration (Superseded)

#### `stage_b_masters_otb_07.json`
- **Generated:** October 22, 2026 (Initial prototype exploration).
- **Dataset:** `data/masters_otb_2026-07.pgn.zst` (July 2026 Lichess Broadcast archive).
- **Status:** **Superseded and Excluded from Authoritative Projections**.
- **Reason for Archival:**
  1. **Legacy Format & Schema:** Generated under the `CODB-v1` format prior to `CGB3` dictionary interning.
  2. **Incomplete SQLite Measurement:** SQLite index size reported as `0` bytes (in-memory test rather than persistent WAL on-disk measurement).
  3. **Lacks Fail-Closed Accounting:** Pre-dates strict fail-closed accounting reconciliation (`is_reconciled`, `retained_canonical_games == accepted_unique`, and `rejected_malformed` counters).
- **Authoritative Replacement:** [`stage_b_masters_otb.json`](stage_b_masters_otb.json).

---

### 2. Stage A Fixture Validation

#### `stage_a_validation.json`
- **Generated:** October 23, 2026.
- **Dataset:** `fixtures/stage_a_validation.pgn` (1,000 deterministic games).
- **Purpose:** Fast offline CI verification establishing initial CGB compression and ingestion correctness without external network dependencies.
- **Results:** 1,000 games encountered, 1,000 accepted unique, 361 Depth 40 positions, 100% verified accounting.

---

### 3. Stage B Single-Month Baselines

#### `stage_b_masters_otb.json`
- **Generated:** October 24, 2026.
- **Dataset:** `data/masters_otb_2026-08.pgn.zst` (August 2026 Lichess Broadcast archive).
- **Results:** 48,940 games encountered, 9,923 accepted unique, 256,960 Depth 40 positions, 43.4 MB SQLite index. Full fail-closed accounting reconciliation.

#### `stage_b_elite_online.json`
- **Generated:** October 24, 2026.
- **Dataset:** `data/elite_online_2013-01.pgn.zst` (January 2013 Lichess rated games).
- **Results:** 121,332 games encountered, zero games passing initial policy (establishing strict bullet/unrated rejection enforcers).

---

### 4. PR-01 Progressive Scaling Series (`masters_1k.json` to `masters_20k.json`)

Authoritative progressive scaling run on combined Masters OTB corpus (`data/masters_combined.pgn.zst`) used to fit Heaps' Law parameters:
- `masters_1k.json`: $N = 1,000$ games, 30,462 D40 positions, 6.78 MB SQLite index.
- `masters_2.5k.json`: $N = 2,500$ games, 71,912 D40 positions, 15.52 MB SQLite index.
- `masters_5k.json`: $N = 5,000$ games, 137,215 D40 positions, 23.95 MB SQLite index.
- `masters_10k.json`: $N = 10,000$ games, 260,050 D40 positions, 44.27 MB SQLite index.
- `masters_20k.json`: $N = 20,000$ games, 493,211 D40 positions, 78.60 MB SQLite index.

---

### 5. PR-02 100k Scale Series (`pr02_scale_*.json`)

Extended empirical scale evaluation testing $O(1)$ RAM streaming ingestion and persistent multi-shard packaging up to 100,000 games:
- **Elite Online Series:** `pr02_scale_elite_1k.json`, `pr02_scale_elite_5k.json`, `pr02_scale_elite_10k.json`, `pr02_scale_elite_50k.json`, `pr02_scale_elite_100k.json`.
- **Masters OTB Series:** `pr02_scale_masters_1k.json`, `pr02_scale_masters_2.5k.json`, `pr02_scale_masters_5k.json`, `pr02_scale_masters_10k.json`, `pr02_scale_masters_20k.json`.
- **Key Findings:** Validated constant memory ceiling ($< 30$ MB RSS) and ~178–181 bytes/game CGB3 block storage density.

---

### 6. Derived Heaps' Law Projections

#### `projections_heaps.json`
- **Generated:** October 26, 2026 (via `scripts/fit_heaps.py`).
- **Inputs:** PR-01 progressive scaling series and PR-02 100k measurements.
- **Model Parameters:** Masters OTB ($\alpha = 0.9293, k = 49.88$), Elite Online ($\alpha = 0.9293, k = 37.63$).
- **Deployment Projections (Option B - Masters D40 / Elite D30):**
  - **Baseline:** 7.68 GB
  - **Optimistic:** 6.44 GB
  - **Conservative:** 9.98 GB (20 MB / 0.2% headroom under 10 GB envelope).
