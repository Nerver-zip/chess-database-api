# Agent Handoff (opening-explorer-api)

## Current state

PR #7 and PR #8 are merged. `opening-explorer-api` is the project identity and the GitHub repository name. The Masters OTB dataset is published as the GitHub release `dataset-masters-otb-r3` (2026-11-16, https://github.com/Nerver-zip/opening-explorer-api/releases/tag/dataset-masters-otb-r3): indexed package (2 parts), filtered PGN, notice, manifest, `SHA256SUMS`. GitHub's reported asset digests equal `releases/masters-otb-r3.json`. `GOAL.md` stays untracked.

## What changed

- **Identity.** CLI `opening-explorer`; crates `opening-explorer-*`; env vars `OPENING_EXPLORER_*` with deprecated `CODB_*` fallbacks; compose files `compose.yaml`, `compose.named-volume.yaml`, `compose.debug.yaml`; image `opening-explorer-api:local`. Unchanged by design: binary magics (`CGB3`, `CODB_CPI`, ...), game IDs, CPI keys, the `codb-` build-identifier prefix and existing release IDs, HTTP routes, historical evidence. Legacy-to-current table: `docs/MIGRATION_FROM_CODB.md`.
- **API fixes (backward compatible, tested).** Additive `cpi_depth_plies`; reload exempt from the 15 s timeout (HTTP reload previously 408'd on the 3.28 M-game snapshot); framework rejections use the error envelope (`invalid_query`, `not_found`, `method_not_allowed`, `request_timeout`); only 500s sanitized; clearer illegal-move/missing-collection messages.
- **Docs.** New: `API_REFERENCE`, `GETTING_STARTED`, `USE_CASES`, `DATASET_RELEASES`, `MIGRATION_FROM_CODB`, `examples/`, `fixtures/api/`. `DATA_CONTRACTS.md` section 6 now points at the API reference. Root README rewritten; `docs/README.md` regrouped by reader need. `scripts/test_feasibility_alignment.py` now requires the historical storage figures in `docs/BENCHMARKS.md` instead of the README (the stale-figure ban still covers the README).
- **Distribution.** `opening-explorer export-pgn`; `scripts/release_assets.py`; `scripts/release_pgn_verify.py`; `.github/workflows/publish-dataset.yml` (manual only; validates a reviewed draft release against `releases/masters-otb-r3.json`, never rebuilds); `releases/distribution_clearance.json` gate.

## Published assets

| Asset | SHA-256 |
| :--- | :--- |
| `masters-otb-r3-indexed.tar.part-001` (2.0 GB) | `6e71be9b8614fb6cdc8ff12b924cc3b9102ca2a9489feb06ab75f7e69f81625a` |
| `masters-otb-r3-indexed.tar.part-002` (709 MB) | `861d539ab6a869f1d28b66bbc11b7ec9d586bd73037fcbbe5dd8f14470b7e8b7` |
| `masters-otb-r3-indexed.tar` (reconstructed) | `89ccb7931357cf21795e9eaa8dc82b0632ce40cb0ae9834927cbc5b637f0e706` |
| `masters-otb-r3-filtered.pgn.zst` (502 MB) | `4134599e1d6da75dbe00ce162e6c1cb90a0092e047bdfdfe5fa4662c36b306ff` |

Local copies remain in git-ignored `data/releases-dist/masters-otb-r3/`. Verification: `benchmarks/gigabase/PGN_EXPORT_VERIFICATION_R3.json` (headers of all games, full python-chess replay of 205,277 sampled games), `PGN_REIMPORT_LEGALITY_R3.json` (all 3,284,429 games reimported through the project's ingest, 0 illegal; shakmaty, not independent), and `INSTALL_VERIFICATION_R3.json` (install, API queries, rollback).

## Licensing

Authoritative current policy: `docs/LICENSING.md` section 6, backed by two maintainer decision records: `releases/distribution_clearance.json` (non-commercial release distribution, unchanged) and `releases/public_deployment_decision.json` (public, non-commercial API deployment, including position statistics and single-game PGN retrieval; commercial use and bulk export are not covered, and no bulk-export endpoint exists). Both rest on the maintainer's reading of the publisher's public CC BY-NC 4.0 grant, not on separate written permission from the publisher. Other documents link to section 6 instead of restating it; `scripts/test_policy_decisions.py` guards the records and stops current docs from reasserting superseded restrictions.

Disclosed and unresolved (section 6.5): the publisher's B2B page prohibits end-user bulk exports and raw-PGN downloads (scope for a non-commercial public product unstated; the publisher has not confirmed the reading); upstream rights for individual games are unverified; no publisher checksum. The immutable R3 artifacts keep their build-time wording (`RELEASE/DATASET_LICENSE.txt` local evaluation only, `RELEASE.json` `public_release_eligible: false`, the NOTICE asset "not_cleared" at packaging); they predate the decisions and were not modified. Retrieval is enabled by default (`OPENING_EXPLORER_ENABLE_GAME_RETRIEVAL=true`); `.env.gigabase.example` now matches. The live R3 deployment still runs with retrieval disabled from its own copied `.env.gigabase`; changing it needs a redeploy (not done).

## Remaining operational steps

1. The existing R3 deployment (retrieval disabled) still runs as `codb-gigabase-r3-codb-1`; a redeploy under the new compose names yields `codb-gigabase-r3-opening-explorer-1` (stop the old container first; both share the `codb` network alias).
2. Open items from the API audit not implemented: per-collection retrieval switch; stable game IDs in the PGN export; `/ready` does not re-check after startup.
3. For a future dataset release (r4): render a fresh notice at build time (the r3 notice text predates the clearance), record a new `distribution_clearance.json` for that release ID, then follow `docs/DATASET_RELEASES.md` section 7.
4. Public-deployment guidance is in `docs/DOCKER_DEPLOYMENT.md` ("Operating a Public, Non-Commercial Instance"). Add a tag ruleset protecting `dataset-*` tags if not yet configured; the publish job uses an environment named `dataset-publication`.

## Verification

Final CI on the PR head passed (pull_request and manual dispatch); `./scripts/validate.sh` passed all eight gates in the Rust 1.100.0 container. Not verified: host bind mounts, a live redeploy under the new names, browser CORS.

## Elite Online R1 release preparation (2026-11-17)

The `elite-online` R1 baseline has been reconstructed from all seven pinned Lichess CC0 archives (8,522,353,902 bytes total) on branch `work/elite-online-r1-release`. The source recovery, immutable snapshot, PGN, package, and scratch install outputs are under git-ignored `data/`. Masters was not rebuilt or modified.

The published Elite snapshot is `codb-elite-online-1794887234132165603-8469-32d007e132ccc048-cpi-d30`: 429,150 accepted games from 46,536,762 encountered; accounting reconciles (45 duplicates, 16 superseded, 46,107,551 policy rejections, no other rejections). The CPI v2 index is depth 30 with 6,136,307 positions and 6,739,644 position/move pairs. `opening-explorer verify --deep`, source rehash, copied-host verification, release asset verification, and the CC0 manifest policy check all pass. The rebuilt `games.idx`, `positions.cpi`, and `shard_0000.cgb3` match the prior Elite baseline byte-for-byte.

The canonical filtered PGN contains 429,150 games (329,763,971 plain bytes), compressed to 69,584,283 bytes at Zstandard level 19 (SHA-256 `4498e4d8ee2b0fe9a5e813a94dd7a14d340beae027fc2f951f5ef483e4adefc5`). The indexed package is 210,370,560 bytes (SHA-256 `9aada688d146ac00eb91b741e200051124afb6945479e37b41d70a32a70cf76e`). Both products are recorded in `releases/elite-online-r1.json` and `data/releases-dist/elite-online-r1/`.

Independent verification with `scripts/release_pgn_verify.py --sample-every 1` passed for every game: 429,150 header checks and full python-chess replays, no parse/tag/header/sidecar/move mismatches, 429,150 unique IDs, and five opening probes matched CLI counts. The indexed package passed clean installation and deep verification; a bit-corrupted package was rejected while the scratch `current` pointer stayed on its seed, the valid package installed, and publication rolled back to the retained seed. Elite-only API smoke checks passed for health/readiness/collection metadata, position stats matching `opening-explorer query --json`, game retrieval, and invalid-move handling; the server shut down cleanly.

Full build, source, index, export, package, install/rollback, and API evidence is in [`benchmarks/elite-online/REBUILD_VERIFICATION_R1.json`](benchmarks/elite-online/REBUILD_VERIFICATION_R1.json). It records actual per-process HWM and disk/wall metrics: ingestion 30,744,576 B HWM; index 151,445,504 B HWM, 18.312 s, with a 128 MiB configured index buffer and 223,794,178 B peak temporary spill; deep verify 137,949,184 B HWM. The release documentation and `MANIFEST.sha256` have been refreshed for these measurements.

`./scripts/validate.sh` passed all eight gates twice in the Rust 1.100.0 container after installing its missing Clippy component: formatting, Clippy, Rust workspace tests, Python tests, CPI and corpus evidence, historical benchmarks, documentation links, and `MANIFEST.sha256`. Commit `59b2ab5` is pushed on `work/elite-online-r1-release`; [Draft PR #10](https://github.com/Nerver-zip/opening-explorer-api/pull/10) is open against `main`. No dataset tag or GitHub release has been created. Next: await review; create the official tag and publish the verified assets only after approval and merge.
