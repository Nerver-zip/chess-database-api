# chess-openings-db continuation handoff

## Context

The prior implementation was on remote host `agents` in `/home/agent/workspace/chess-openings-db`, using Claude Code. Claude's quota expired after commit `e7667b4`; the continuation began from branch `feat/corpus-release-pipeline` and Draft PR #6. The original untracked `GOAL.md` is user input and must remain uncommitted.

## Work completed in the continuation

- Committed termination filtering and tighter SQLite memory settings in `5b97671`.
- Rebuilt both real datasets with the termination filter: 42,612 Masters and 429,150 Elite games. The measured ingestion peaks were 29,507,584 and 29,732,864 bytes.
- Verified the 13 local archives against their pinned publisher hashes; the official publisher checksum lists matched all 13 catalog entries.
- Deep-verified both releases, packaged and verified each local archive, and passed the 414-check HTTP integration run.
- Captured direct real-data HTTP position queries, compared them with `codb query`, and retrieved one actual PGN from each collection.
- Scanned all 42,612 accepted Masters PGNs for the source policy's online-event markers; zero matches. This still cannot rule out unmarked online events.
- Rebuilt each corpus independently from its saved manifest; `games.idx`, `positions.cpi`, and shard hashes matched byte for byte.
- Added the `2026.11-r2` policy/catalog descriptor and refreshed measured docs and evidence. Both r2 releases are now built from clean commit `7f4fa09`; their manifests record `git_tree_dirty: false`. Current reports and package hashes are in `benchmarks/corpus/`.

## Verification and next actions

- `./scripts/validate.sh`: PASS on this R2 worktree; all eight gates passed. The generated timing-only modification to `benchmarks/results/historical/stage_a_validation.json` was restored afterward. `MANIFEST.sha256` verifies.
- The final R2 snapshots, marker-audit evidence, and documentation are ready to commit. Preserve untracked `GOAL.md`.
- Push the branch, update Draft PR #6 with the current evidence and `NEEDS FURTHER WORK` readiness, then inspect CI. The GitHub CLI token could not read PR check rollups; the connected GitHub integration may provide status. Do not merge or upload corpus packages.

## Limitations to keep visible

Masters OTB qualification is still a medium-confidence marker heuristic; a broadcast archive can contain unmarked online events, and roughly 69% of accepted games lack a physical venue. Elite archives cover 2015–2017, not current play. The PGN export has no provisional-rating marker. The release packages remain local-only. Current readiness recommendation: **NEEDS FURTHER WORK** until the Masters OTB uncertainty is resolved or explicitly accepted by maintainers.
