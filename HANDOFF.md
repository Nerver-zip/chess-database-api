# Agent Handoff (chess-openings-db)

## Current objective

Finish the authorized private Gigabase R3 Masters promotion on PR #7 (`feat/gigabase-masters`). Keep the PR Draft, open, and unmerged. Do not rebuild the source corpus or publish the dataset publicly.

## Active release and preserved data

- `data/releases/masters-otb/current` now points to `codb-masters-otb-1794774904566910568-6701-5698b67c3952ca6b-cpi-d40`: 3,284,429 accepted games, CPI depth 40, 51,909,604 positions, and 55,864,091 position/move pairs.
- The previous 29,640-game Masters version `codb-masters-otb-1794689403953239696-66953-9aa01bd5606eaf65-cpi-d40` remains in `data/releases/masters-otb/versions/` and was used in a successful publisher-based rollback test.
- Elite remains at `codb-elite-online-1794670050929819232-27177-f042ce0f4dfe2697-cpi-d30` with 429,150 games and CPI depth 30.
- The original R3 candidate remains at `data/work/gigabase-eval/docker-r3-smoke/releases/masters-otb/versions/codb-masters-otb-1794774904566910568-6701-5698b67c3952ca6b-cpi-d40`. Its 18 release artifacts were individually SHA-256 checked. The existing local package checksum passed (`297b7d2ab67951fd9655c35f6993da4d0fb5dd55ee763b62f9249c1804019247`). No corpus rebuild occurred.
- The separate repeat-build shards in the accidentally removed `codb-pr7-rust-work` container are unavailable. This does not affect the complete baseline snapshot, its verified package, or this promotion.

## Deployment evidence

- The private API runs as container `codb-gigabase-r3-codb-1` from the named volume `codb-gigabase-r3-runtime`, on dedicated network `codb-gigabase-r3-net`. The Docker daemon could not access checkout bind paths; bind-mount support is not claimed as tested.
- Compose mounts `/datasets` read-only; no host port is published. `CODB_ENABLE_GAME_RETRIEVAL=false`; a known game route returned 404.
- Startup, restart, readiness, 96/96 concurrent requests, API/CLI query equality, and W/D/L reconciliation passed for both collections. Masters and Elite position-query counts are recorded in `benchmarks/gigabase/OPERATIONAL_DEPLOYMENT_R3.json`.
- The old Masters version was reactivated through `codb publish`, deep verification passed, the API was restarted against it, then R3 was republished through `codb publish`, deep verification passed, and the API restarted on R3. All 18 R3 artifact hashes were also verified in the runtime volume.
- The publisher claims CC BY-NC 4.0 but supplied no publisher checksum; upstream rights remain unresolved. Private operation is authorized for this deployment; public redistribution and public service remain unauthorized. All accepted games lack `Mode` and rely on the publisher's archive-level OTB assertion.

## Audit discrepancy disposition

The three `python-chess` sample games were legal and matched canonical moves, starting positions, and final boards. Exact persisted metadata differences and source-versus-sidecar attribution values are recorded in `benchmarks/gigabase/FINAL_REPORT.md`; they do not change position statistics. The source-occurrence deduplication dispositions are not linked in retained audit evidence and remain unavailable; no replay or game removal was done.

## Implementation in progress

- `scripts/promote_gigabase_r3.py` checks the pinned R3 `RELEASE.json` hash, all 18 artifact hashes, release identity/accounting, source restrictions, and deep CODB verification; it stages a copy and invokes the existing atomic `codb publish` command.
- `scripts/corpus.py` blocks routine Masters build/bootstrap replacement of active Gigabase R3 unless the explicit `--allow-replacing-local-evaluation-release` option is supplied.
- `compose.codb.named-volume.yaml` and `.env.codb.gigabase.example` provide the restricted named-volume mode with raw-PGN retrieval disabled. `.gitignore` permits the tracked example settings file.
- `.github/workflows/ci.yml` keeps `main` pushes and required PR events, skips Draft PR jobs, and includes `workflow_dispatch` for the final gate without changing test coverage.
- README, corpus, deduplication, licensing, Docker, benchmark, and audit evidence now describe the current private R3 deployment and retain historical baselines as history.

## Verification and next actions

Completed: all 18 host artifact checks; local package checksum; promoter check-only and publish; host and volume deep verification; Compose configuration parse; targeted bootstrap guard tests and runtime rejection checks; API startup/restart, query/WDL, concurrency, retrieval, rollback, and reactivation checks. `./scripts/validate.sh` passed all eight gates in the Rust 1.100.0 validation container, including Rust formatting, Clippy, workspace tests, Python suites, evidence validators, documentation links, and manifest verification. `git diff --check` passed. No full source corpus rebuild occurred.

The implementation is committed on PR #7; the exact discrepancy values have now been documented from the saved source PGNs, candidate CODB records, and attribution sidecar without a deduplication replay. `./scripts/validate.sh` passed after this documentation correction, and `./scripts/reproduce_benchmarks.sh --ci` passed on the implementation. The last hosted full CI run passed before this documentation-only update; after pushing the final docs commit, run the final manual GitHub CI gate once. Keep PR #7 Draft and unmerged. Do not publish publicly or rebuild the corpus.

## Evidence paths

- `benchmarks/gigabase/OPERATIONAL_DEPLOYMENT_R3.json`
- `benchmarks/gigabase/R3_REPRODUCIBILITY.json`
- `benchmarks/gigabase/INDEPENDENT_PGN_AUDIT_R3.json`
- `benchmarks/gigabase/FINAL_REPORT.md`
- `docs/CORPUS_BUILD.md`, `docs/DOCKER_DEPLOYMENT.md`, `docs/LICENSING.md`
