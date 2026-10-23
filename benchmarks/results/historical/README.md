# Historical Benchmark Artifacts

This directory archives early prototype benchmark artifacts that have been superseded by the current authoritative benchmark schema and methodology.

## Archived Artifacts

### `stage_b_masters_otb_07.json`
- **Generated:** October 22, 2026 (Initial prototype exploration).
- **Dataset:** `data/masters_otb_2026-07.pgn.zst` (July 2026 Lichess Broadcast archive).
- **Status:** **Superseded and Excluded from Current Projections**.
- **Reason for Archival:**
  1. **Legacy Format & Schema:** Generated under the `CODB-v1` format prior to `CGB3` dictionary interning.
  2. **Incomplete SQLite Measurement:** SQLite index size is reported as `0` bytes (in-memory test rather than persistent WAL on-disk measurement).
  3. **Lacks Fail-Closed Accounting:** Pre-dates the strict fail-closed accounting invariant reconciliation (`is_reconciled`, `retained_canonical_games == accepted_unique`, and `rejected_malformed` counters).
- **Authoritative Replacement:**
  - Single-month OTB benchmark: [`stage_b_masters_otb.json`](../stage_b_masters_otb.json) (August 2026, 48,940 games encountered, 9,923 accepted, complete fail-closed accounting, physical on-disk SQLite WAL comparison).
  - Multi-month progressive scaling series: [`masters_1k.json`](../masters_1k.json) through [`masters_20k.json`](../masters_20k.json) on `data/masters_combined.pgn.zst`.
