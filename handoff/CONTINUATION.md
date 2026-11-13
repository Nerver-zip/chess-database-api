# chess-openings-db continuation handoff

## Context

The prior implementation was on remote host `agents` in `/home/agent/workspace/chess-openings-db`, using Claude Code. Claude's quota expired after commit `e7667b4`; the continuation began from branch `feat/corpus-release-pipeline` and Draft PR #6. The original untracked `GOAL.md` is user input and must remain uncommitted.

## Work completed in the continuation

- Committed termination filtering and tighter SQLite memory settings in `5b97671`.
- Rebuilt both real datasets with the termination filter: 42,612 Masters and 429,150 Elite games. The measured ingestion peaks were 29,339,648 and 29,851,648 bytes.
- Verified the 13 local archives against their pinned publisher hashes; the official publisher checksum lists matched all 13 catalog entries.
- Deep-verified both releases, packaged and verified each local archive, and passed the 414-check HTTP integration run.
- Captured direct real-data HTTP position queries, compared them with `codb query`, and retrieved one actual PGN from each collection.
- Rebuilt each corpus independently from its saved manifest; `games.idx`, `positions.cpi`, and shard hashes matched byte for byte.
- Added the `2026.11-r2` policy/catalog descriptor and refreshed measured docs and evidence. A clean r2 Elite release build is currently running under `data/work/elite-online-baseline-20261113T232801Z/`.

## Before handoff is complete

1. Finish the Elite r2 build; rebuild Masters at a clean commit if its release metadata records a dirty tree.
2. Verify and package both r2 releases; rerun `http-check`, `bench-lookup`, and direct HTTP examples; copy reports into `benchmarks/corpus/`.
3. Refresh `HANDOFF.md`, the corpus report, Q-04, catalog checksums, and PR #6 body. Keep the PR Draft and do not upload data.
4. Run `./scripts/validate.sh`, commit the final evidence, push, and inspect CI.

## Limitations to keep visible

Masters OTB qualification is still a medium-confidence marker heuristic; a broadcast archive can contain unmarked online events, and roughly 69% of accepted games lack a physical venue. Elite archives cover 2015–2017, not current play. The PGN export has no provisional-rating marker. The release packages remain local-only. Current readiness recommendation: **NEEDS FURTHER WORK** until the Masters OTB uncertainty is resolved or explicitly accepted by maintainers.
