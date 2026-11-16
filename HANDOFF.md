# Agent Handoff (chess-openings-db)

## Current objective

Complete the R3 local Gigabase Masters evaluation for PR #7. The user authorized pushing updates to `feat/gigabase-masters` and updating PR #7 while keeping it Draft, open, and unmerged. Do not change production dataset pointers or publish/deploy the evaluation.

## Current state

- Branch `feat/gigabase-masters` has the R3 policy, ingestion updates, audit runners, reports, and documentation in the working tree. The changes are not yet committed or pushed.
- PR [#7](https://github.com/Nerver-zip/chess-openings-db/pull/7) is open, Draft, and unmerged. Its current description still documents R2 and must be replaced with the final R3 result after validation.
- The R3 build ID is `codb-masters-otb-1794774904566910568-6701-5698b67c3952ca6b-cpi-d40`. The source archive is locally 2,570,013,439 bytes (SHA-256 `4e4209ccc1870e7fede557aae09b4dd40b721d4697db9b4c272ff0e4fbbdb500`); its PGN member is 12,300,428,287 bytes (SHA-256 `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`). The publisher supplied no checksum, so these are local hashes.
- Policy `gigabase-otb/2026-11-r3` has SHA-256 `93b9d0c7fd8eb35bb3bf36425a5e7b02f95fa80910942bbda615a453bd2ef3a9`. The streaming inventory found 13,450,696 records and clean EOF with no recoverable syntax errors. Build accounting reconciles: 3,284,429 accepted unique, 4,018 confirmed duplicates, 506 superseded, 10,153,901 policy rejects, 7,842 quarantined stable-ID collisions, and zero variant, illegal, or malformed rejects.
- CPI v2 indexes 3,284,429 games to 40 plies, with 51,909,604 positions and 55,864,091 position/move pairs. CPI size is 1,107,897,085 bytes (SHA-256 `284898e18f4a4aba0b56d383e8b1a163790a85e1081749f0ff9204fdab5a971c`). Indexing used a 128 MiB configured budget, 35 runs, one merge pass, and 2,132,859,200 bytes peak spill. Sampled anonymous RSS was 122,548,224 bytes; total process RSS was 1,027,588,096 bytes, mostly file-backed. The build sampled 67,895,296 bytes peak RSS and took 4,723.004 seconds.
- The selected-position CPI oracle replayed all 3,284,429 games and matched 18 lines / 17 unique positions with no mismatch (`benchmarks/gigabase/CPI_ORACLE_R3.json`). The source-to-canonical audit matched all 3,284,429 retained games and compared metadata, attribution, moves, starting position, final board, and the source-attribution sidecar with zero mismatches (`benchmarks/gigabase/SOURCE_CANONICAL_AUDIT.json`). It reuses the production parser, stable-ID logic, policy, and deduplicator; it is traceability evidence, not independent parser or deduplication validation.
- The source dedupe audit compares all 12,366 duplicate/supersession/collision rows with canonical games, with zero lookup failures. It found 9,256 metadata-drift records and 162 strict source-prefix cases; each shares the starting position, 161 has a different final board and is validated as a strict truncation, and one has the same final board. Its anonymized report contains no game IDs, player/event values, or raw PGN (`benchmarks/gigabase/SOURCE_DEDUPE_AUDIT.json`). The cross-release overlap audit found no exact stable-ID overlap, 16,488 identical move sequences, and 259 one-to-one player/date/event/round/move candidates; all are manual-review candidates and none was removed or merged (`benchmarks/gigabase/SOURCE_OVERLAP.json`).
- The corrected local package is 2,700,902,400 bytes with SHA-256 `297b7d2ab67951fd9655c35f6993da4d0fb5dd55ee763b62f9249c1804019247`. It excludes the source archive and PGN; checksum, extraction, deep verification, and offline query passed. The quality report build ID now matches the dataset. Isolated bootstrap returned 1,469,608 games for `e4`, and rollback restored the previous 29,640-game Masters snapshot.
- Real Docker API smoke used private networking, read-only named-volume dataset copies, no published host ports, and UID 10001. Masters and Elite position and stored-PGN responses matched CLI output; 96/96 concurrent requests passed. Restart/deep verification and SIGTERM shutdown passed. Sampled RSS was 208,265,216 bytes. Host bind-mount behavior remains unverified.
- Production release roots are unchanged: Masters remains at 29,640 games and Elite at 429,150 games. Rights remain unresolved: the publisher claims CC BY-NC 4.0, supplies no checksum, and describes heterogeneous upstream sources; no event-level OTB audit, independent publisher-Elo validation, or project-specific written terms are recorded. All accepted games lack `Mode`. No public distribution or service is authorized by the available evidence.
- Main R3 evidence is indexed in `benchmarks/gigabase/R3_REPRODUCIBILITY.json`; the final narrative is `benchmarks/gigabase/FINAL_REPORT.md`. The workspace host has no Cargo; validation used Rust 1.100.0 in `codb-pr7-rust-work`. After fixing strict Clippy findings, `./scripts/validate.sh` passed twice on 2026-11-16, including the recorded reproducibility status. The final run passed all eight gates; Rust workspace/example tests passed, Python ran 110 tests with 2 skipped, production CPI/corpus evidence and historical benchmark validators passed, documentation links passed, and `MANIFEST.sha256` passed.

## Next actions

1. Review the final diff, `git diff --check`, and `git status`; keep ignored source data and release packages out of the commit.
2. Replace the stale R2 PR description with a concise R3 summary and explicit local-only/rights limitations. Preserve Draft/open/unmerged state.
3. Commit the intended changes, push `feat/gigabase-masters`, and verify the PR remains Draft and unmerged.

## Evidence paths

- `benchmarks/gigabase/R3_REPRODUCIBILITY.json`, `FINAL_REPORT.md`, `INVENTORY.md`, `BUILD_REPORT_R3.json`, `INDEX_REPORT_R3.json`, `QUALITY_REPORT_R3.json`, `RELEASE_R3.json`, and `RESOURCE_METRICS_R3.json`
- `benchmarks/gigabase/CPI_ORACLE_R3.json`, `SOURCE_CANONICAL_AUDIT.json`, `SOURCE_DEDUPE_AUDIT.json`, `SOURCE_OVERLAP.json`, `DOCKER_API_R3.json`, `PUBLISHER_TERMS_REVIEW.json`, and `ARCHIVE_RECHECK.json`
- `docs/LICENSING.md`, `docs/CORPUS_BUILD.md`, `docs/CORPUS_AND_DEDUPLICATION.md`, and `docs/DOCKER_DEPLOYMENT.md`
- Source archive, extracted PGN, local packages, and deployment experiments remain under ignored `data/` and must not be committed.
