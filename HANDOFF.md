# Agent Handoff (opening-explorer-api)

## Current objective

Deliver the consolidation PR stacked on PR #7 (`feat/gigabase-masters`): `opening-explorer-api` identity, two prepared Masters OTB dataset products, release tooling, licensing/attribution, README and API documentation. Keep it Draft. Do not merge, do not publish release assets, the GitHub repository is already renamed. `GOAL.md` stays untracked.

## What changed

- **Identity.** CLI `opening-explorer`; crates `opening-explorer-*`; env vars `OPENING_EXPLORER_*` with deprecated `CODB_*` fallbacks; compose files `compose.yaml`, `compose.named-volume.yaml`, `compose.debug.yaml`; image `opening-explorer-api:local`. Unchanged by design: binary magics (`CGB3`, `CODB_CPI`, ...), game IDs, CPI keys, the `codb-` build-identifier prefix and existing release IDs, HTTP routes, historical evidence. Legacy-to-current table: `docs/MIGRATION_FROM_CODB.md`.
- **API fixes (backward compatible, tested).** Additive `cpi_depth_plies`; reload exempt from the 15 s timeout (HTTP reload previously 408'd on the 3.28 M-game snapshot); framework rejections use the error envelope (`invalid_query`, `not_found`, `method_not_allowed`, `request_timeout`); only 500s sanitized; clearer illegal-move/missing-collection messages.
- **Docs.** New: `API_REFERENCE`, `GETTING_STARTED`, `USE_CASES`, `DATASET_RELEASES`, `MIGRATION_FROM_CODB`, `examples/`, `fixtures/api/`. `DATA_CONTRACTS.md` section 6 now points at the API reference. Root README rewritten; `docs/README.md` regrouped by reader need. `scripts/test_feasibility_alignment.py` now requires the historical storage figures in `docs/BENCHMARKS.md` instead of the README (the stale-figure ban still covers the README).
- **Distribution.** `opening-explorer export-pgn`; `scripts/release_assets.py`; `scripts/release_pgn_verify.py`; `.github/workflows/publish-dataset.yml` (manual only; validates a reviewed draft release against `releases/masters-otb-r3.json`, never rebuilds); `releases/distribution_clearance.json` gate.

## Prepared assets (local only, git-ignored `data/releases-dist/masters-otb-r3/`)

| Asset | SHA-256 |
| :--- | :--- |
| `masters-otb-r3-indexed.tar.part-001` (2.0 GB) | `6e71be9b8614fb6cdc8ff12b924cc3b9102ca2a9489feb06ab75f7e69f81625a` |
| `masters-otb-r3-indexed.tar.part-002` (709 MB) | `861d539ab6a869f1d28b66bbc11b7ec9d586bd73037fcbbe5dd8f14470b7e8b7` |
| `masters-otb-r3-indexed.tar` (reconstructed) | `89ccb7931357cf21795e9eaa8dc82b0632ce40cb0ae9834927cbc5b637f0e706` |
| `masters-otb-r3-filtered.pgn.zst` (502 MB) | `4134599e1d6da75dbe00ce162e6c1cb90a0092e047bdfdfe5fa4662c36b306ff` |

Independent verification (`benchmarks/gigabase/PGN_EXPORT_VERIFICATION_R3.json`): 3,284,429 games; headers and sidecar tags checked on every game; every 16th game (205,277) fully replayed with python-chess; zero failures. Install and rollback evidence: `benchmarks/gigabase/INSTALL_VERIFICATION_R3.json` (an earlier racy rollback-install step is superseded by a clean sequential rerun, PASS). A whole-corpus reimport of the exported PGN through the project's ingest (`benchmarks/gigabase/PGN_REIMPORT_LEGALITY_R3.json`) encountered all 3,284,429 games with 0 illegal, 0 malformed and 0 variant rejections. That reimport uses shakmaty, not an independent parser; python-chess fully replayed the 205,277-game sample.

## Licensing gate (read before publishing)

Per the project decision, the publisher's CC BY-NC 4.0 is the basis for non-commercial distribution, with attribution and disclosed changes (`docs/LICENSING.md` section 6, `dataset/ATTRIBUTION.md`). Recorded conflicts, stated precisely: the snapshot's build-time `RELEASE/DATASET_LICENSE.txt` says local internal evaluation only; the publisher's B2B page prohibits end-user bulk exports and raw-PGN downloads to end users (scope for a non-commercial public dataset unstated); the publisher provides no archive checksum. Affected mechanisms: the filtered PGN and the indexed package (embeds full games). `releases/distribution_clearance.json` is now `cleared` (maintainer decision recorded 2026-11-16, resting on the maintainer's reading of the public CC BY-NC 4.0 grant, not on written permission from the publisher); the conflicts remain disclosed there. The tooling still refuses to publish unless the record is `cleared` with a reference and a matching release ID.

## Remaining operational steps

1. Merge PR #7, retarget this PR to `main`, run the final Ready-for-Review CI.
2. After merge: record clearance, tag `dataset-masters-otb-r3` on the merged commit, upload to a DRAFT release with `release_assets.py upload` (dry run first), then run the manual publish workflow.
3. Repository rename: GitHub already reports the repository as `Nerver-zip/opening-explorer-api` (the old URL redirects). The local `origin` was updated to the new URL. Confirm no CI or branch-protection settings still reference the old name after #7 merges.
4. The private deployment still runs as `codb-gigabase-r3-codb-1`; a redeploy under the new compose names yields `codb-gigabase-r3-opening-explorer-1` (stop the old container first; both share the `codb` network alias).
5. Open items from the API audit not implemented: per-collection retrieval switch; stable game IDs in the PGN export; `/ready` does not re-check after startup.

## Verification

Container run of `./scripts/validate.sh` (Rust 1.100.0): gates 1-3 passed (fmt, clippy, workspace tests); gate 4 failed on the README storage-figure test (fixed as described above) and its dependent `reproduce_benchmarks.sh --ci` test; `--ci` and the feasibility suite pass standalone after the fix. A final full run is recorded in the PR description. Not verified: host bind mounts, a live redeploy under the new names, `reload.sh` against the final binary, browser CORS.
