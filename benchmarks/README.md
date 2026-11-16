# Benchmarks Directory (`benchmarks/`)

This directory houses the empirical performance benchmarks, scaling measurements, and regression baselines for `opening-explorer-api`.

## Structure Overview

```
benchmarks/
├── README.md               # Directory overview and methodology guide (this file)
├── production_cpi/         # Authoritative active production CPI index baselines (ADR-0003)
├── corpus/                 # Current real Masters/Elite, package, and Docker evidence
├── gigabase/               # Local-only Lumbra Gigabase source evaluation evidence
└── results/                # Output destination for runtime benchmark executions
    └── historical/         # Preserved benchmark artifacts from completed research milestones
```

---

## 1. Production CPI Baselines (`benchmarks/production_cpi/`)

The `benchmarks/production_cpi/` directory constitutes the **active, authoritative baseline** for the Compact Position Index (CPI v2) introduced in [ADR-0003](../docs/adr/0003-production-cpi-index.md).

These artifacts document reproducible measurements on the final bounded-memory external sorting pipeline across 16 matrix combinations:

- **Collections:** `masters-otb` and `elite-online`
- **Sample Sizes ($N$):** 1,000, 20,000, 50,000, and 100,000 curated games
- **Memory Limits ($M$):** 16 MiB, 32 MiB, and 128 MiB in-memory accumulation buffers
- **Merge Fan-Ins ($F$):** 2-way and 64-way external K-way merge runs
- **Validation Script:** [`scripts/validate_production_cpi_benchmarks.py`](../scripts/validate_production_cpi_benchmarks.py)

Each production CPI artifact verifies:
1. Complete fail-closed accounting reconciliation (`is_reconciled = true`, `retained_canonical_games == accepted_unique`).
2. Exact memory bound enforcement: peak heap allocation during external sort remains strictly below configured memory limit ($M$).
3. CPI byte density and compression ratios against raw unindexed data.
4. Sub-millisecond random access query latency percentiles ($p_{50}$, $p_{95}$, $p_{99}$) for Opening Explorer lookups.

---

## 1.1 Real Corpus Evidence (`benchmarks/corpus/`)

Small JSON records (release manifest, build report, quality report, per-step metrics, index report, dataset license notice, event audit, package checksums, HTTP/CLI check, and real Docker smoke report) produced by the corpus and Docker acceptance workflows on the real Lichess-derived releases. They are validated by [`scripts/validate_corpus_evidence.py`](../scripts/validate_corpus_evidence.py). Binary datasets are never committed; see [docs/CORPUS_BUILD.md](../docs/CORPUS_BUILD.md).

## 1.2 Gigabase R3 Private Deployment and Evaluation Evidence (`benchmarks/gigabase/`)

This directory records the full-source r3 policy impact, build, CPI measurements, source traceability, package, parser sample, and operational deployment evidence for the user-supplied Lumbra's Gigabase archive. R3 is active in an authorized private Masters deployment; it remains outside the approved source catalog and is not cleared for public distribution or service. The publisher provides no archive checksum and upstream rights remain unresolved. Start with the [final report](gigabase/FINAL_REPORT.md), [R3 reproducibility record](gigabase/R3_REPRODUCIBILITY.json), [operational deployment report](gigabase/OPERATIONAL_DEPLOYMENT_R3.json), [independent full rebuild](gigabase/INDEPENDENT_REBUILD_R3.json), [independent parser sample](gigabase/INDEPENDENT_PGN_AUDIT_R3.json), [R3 header policy impact](gigabase/POLICY_IMPACT.json), [selected-position CPI oracle](gigabase/CPI_ORACLE_R3.json), and [source-to-canonical audit](gigabase/SOURCE_CANONICAL_AUDIT.json). The CPI oracle replayed the full canonical set through the 40-ply index depth and matched 18 target lines across 17 positions. A separate full rebuild matched all ten content artifacts. The targeted external parser sample verified 426 selected records; direct follow-up confirmed matching moves and boards for the three sampled metadata/attribution discrepancies. The durable report records differing field counts but is anonymized and does not contain exact per-row source and Opening Explorer values or link rows to deduplication dispositions; no dispositions are inferred. The observed descriptive metadata/provenance variations do not affect Opening Explorer position statistics. Quota misses and limits are documented in the report and [local runbook](gigabase/INDEPENDENT_PGN_AUDIT_RUNBOOK.md). The R3 package checksum, atomic promotion, 18 artifact checks, API/CLI and W/D/L comparisons, read-only named-volume startup/restart, 96/96 concurrent requests, disabled PGN route, rollback, and R3 reactivation are recorded in the operational deployment report. The daemon could not access host bind paths; bind mounts are not claimed as tested. The earlier 156,898-game r2 evidence is historical. The original archive and generated dataset remain under ignored `data/` paths.

The completed r3 source-to-canonical scan matched all 3,284,429 retained games, reported zero content mismatches, and reconciled all 3,287,619 expected attribution pairs against the sidecar. The [source-dedupe audit](gigabase/SOURCE_DEDUPE_AUDIT.json) compares all 12,366 duplicate, supersession, and collision outcomes with canonical retrieval, emits 50 anonymized examples, and reconciles 162 strict source move-prefix cases by starting and final board. These traceability checks reuse the repository's parser, stable-ID computation, and deduplicator, so they do not constitute independent parser or deduplication implementations.

Historical r2 independent-parser evidence includes the original [144-game byte-window audit](gigabase/INDEPENDENT_PGN_AUDIT.json) and a supplemental [300,000-window targeted sample](gigabase/INDEPENDENT_PGN_AUDIT_TARGETED.json), documented in the [local runbook](gigabase/INDEPENDENT_PGN_AUDIT_RUNBOOK.md). The supplemental sample compared 227 unique candidate games and reported exact matches across the compared fields; four of ten supported source-tag strata missed the ten-game quota. Its measured peak RSS was 339,388 KiB, with no fixed memory ceiling claim. The earlier mmap run recorded 1,386,212 KiB and also missed planned quotas. Neither historical sample validates r3. All parser samples use a local-only external GPL-3.0-or-later `python-chess` package; neither checker nor wheel is part of Opening Explorer or the PR. The machine reports carry the exact hashes and the runbook explains the licensing boundary.

---

## 2. Runtime Results (`benchmarks/results/`)

`benchmarks/results/` serves as the runtime destination directory for automated benchmark runs generated by:
- `opening-explorer bench`: Generates detailed execution reports for ingestion, CGB3 compression, and SQLite comparative baseline measurements.
- `scripts/reproduce_benchmarks.sh`: Automated test harness executing deterministic and empirical reproduction pipelines.

Runtime outputs in this directory can be verified against the schema validator [`scripts/validate_benchmarks.py`](../scripts/validate_benchmarks.py).

---

## 3. Historical Artifacts (`benchmarks/results/historical/`)

`benchmarks/results/historical/` archives empirical results from prior engineering milestones:

- **Stage A Validation (`stage_a_validation.json`):** 1,000-game deterministic fixture benchmark demonstrating initial in-memory parsing and CGB baseline compression.
- **Stage B Baselines (`stage_b_masters_otb.json`, `stage_b_elite_online.json`):** Single-month real-world PGN archives establishing bounded streaming ingestion and CGB3 persistent storage.
- **PR-01 Progressive Scaling Series (`masters_1k.json` to `masters_20k.json`):** Empirical progressive scaling series ($N \in \{1000, 2500, 5000, 10000, 20000\}$) used to fit Heaps' Law parameters ($\alpha, k$).
- **PR-02 Scale Exploration (`pr02_scale_*.json`):** Scale evaluation up to 100,000 games testing $O(1)$ RAM scaling and multi-shard boundaries.
- **Fitted Projections (`projections_heaps.json`):** Derived mathematical model predicting deployed storage for 15 million games under Option B (Masters D40 / Elite D30).
- **Superseded Prototypes (`stage_b_masters_otb_07.json`):** Early Opening Explorer-v1 prototype artifacts preserved for provenance.

For detailed provenance, rationale, and schema changes for each archived file, consult [`benchmarks/results/historical/README.md`](results/historical/README.md).

---

## Validation & Reproduction

To validate benchmark integrity across all suites:

```bash
# Validate production CPI baseline artifacts
python3 scripts/validate_production_cpi_benchmarks.py

# Validate historical benchmark schemas, accounting invariants, and projections
python3 scripts/validate_benchmarks.py

# Run benchmark validation regression test suites
python3 -m unittest scripts/test_validate_benchmarks.py
python3 -m unittest scripts/test_reproduce_runner.py
python3 -m unittest scripts/test_feasibility_alignment.py
```
