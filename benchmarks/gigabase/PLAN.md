# PR #7 Engineering Plan and Execution Record

## Objective

Build a reproducible, source-qualified local Masters OTB evaluation from the supplied Lumbra's Gigabase archive. Keep it separate from approved production releases until checksum and rights gaps are resolved; preserve CGB3/CPI v2/API contracts and submit a dedicated Draft PR without merging.

## Baseline and source evidence

- PR #6 baseline: 29,640 Masters games, 705,718 positions at depth 40, and 29,892,368 deployed bytes; Elite remains 429,150 games at depth 30 and 210,308,033 deployed bytes.
- Input archive: `OTB_DATABASE.7z`, 2,570,013,439 bytes, local SHA-256 `4e4209ccc1870e7fede557aae09b4dd40b721d4697db9b4c272ff0e4fbbdb500`. It is untracked and has been moved unchanged to ignored `data/sources/lumbra/OTB_DATABASE.7z`.
- Archive member inventory: one regular member, `LumbrasGigabase_OTB_Complete.pgn`, declared size 12,300,428,287 bytes, timestamp 2026-11-11. Full streaming decompression completed and passed; member SHA-256 is `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`.
- The publisher's current download page lists “OTB Complete”, version 2026-11-11, described as all OTB PGN files except LichessBroadcastDB. It publishes no game count or archive checksum. The local hashes are not publisher-authenticated checksums.
- The publisher identifies the main database as CC BY-NC 4.0 and documents heterogeneous upstream source classes and `Source` tags. Its licensing page describes a free internal-development/closed-beta phase (up to 50 non-paying testers and six months), prohibits bulk raw-data exports, and separates commercial use. Do not claim commercial or public-production clearance from the local archive or CC license alone; report source-level uncertainty and keep the release isolated from the CC BY-SA Lichess release.

## Implementation sequence and status

1. **Complete.** Inventory the full PGN structure, headers, source tags, dates, ratings, player/event/site coverage, and results. Preserve the original archive under ignored `data/`.
2. **Complete for local evaluation.** Add a separate source, identity, and complete-date policy; reject explicit non-OTB modes, incomplete or invalid dates, and pre-1970 records; leave the existing Lichess qualification path unchanged. Missing `Mode` relies on the publisher's archive-level OTB assertion, not an event audit.
3. **Complete.** Add safe local `.7z` extraction with local hashes distinct from publisher checksums, regular-member checks, expansion limits, and offline tests.
4. **Complete.** Preserve per-game attribution in a bounded, integrity-checked sidecar without changing CGB3, CPI v2, or `games.idx` layouts.
5. **Complete.** Build twice, reconcile accounting, deeply verify both results, retain the previous releases, and record time, memory, index, output, overlap, and opening-coverage measurements.
6. **Complete with an environment limitation.** Create and verify a local-only package, bootstrap it beside the retained Masters snapshot in an isolated ignored root, and roll back to the prior version. Serve the evaluation and existing Elite snapshot in Docker with raw-PGN retrieval disabled, query them from a separate client, and attempt the normal host bind. Docker-in-Docker could not resolve workspace bind paths or expose its loopback port to the workspace host, so the service used read-only named volumes and an in-network client. The package is not approved for external redistribution or production use.
7. **In progress.** Documentation and new tests are complete. Re-run `./scripts/validate.sh`, update the handoff and manifest, then push the feature branch and update the Draft PR without merging.

## Current implementation risks

- The opt-in `GigabaseOtbPolicy` now reads `Source`, player identity evidence, `Mode`, and date tags before move replay. It relies on the publisher's archive-level OTB Complete assertion when Mode is absent; it does not independently verify event-level play mode.
- Accepted source fields are recorded in the optional `source-attribution.jsonl` sidecar, not in CGB3. Deep verification checks the sidecar against `games.idx`. This is attribution evidence, not proof of per-game licensing.
- Source allowlisting and identity evidence do not independently validate each game's upstream rights or the official status of publisher-supplied Elo. Public redistribution remains blocked pending publisher terms and upstream-rights review.
- The archive is 12.30 GB expanded. The completed local build and Docker evidence are summarized in [FINAL_REPORT.md](FINAL_REPORT.md). The publisher checksum and upstream-rights gaps still prevent an approved public release.

## Verification status

The feature branch `feat/gigabase-masters` starts at fetched `origin/main` commit `56d39184a8c68b3d4a5883a423cf79b2a325f4eb`. Archive identity, member inventory, complete decompression, extraction, and relocation hashes are verified locally. The full streaming header inventory and 156,898-game r2 source-qualified evaluation are complete. The production catalog and existing releases remain unchanged. The policy, runtime tag capture, integrity-checked sidecar, independent rebuild, local package/bootstrap/rollback, and restricted Docker API evaluation are documented in [FINAL_REPORT.md](FINAL_REPORT.md). The final validation run and branch push remain.

## Inventory findings and policy direction

The full streaming structural/header inventory is complete in [INVENTORY.md](INVENTORY.md) and [INVENTORY.json](INVENTORY.json). The parser encountered and structurally parsed 13,450,696 records, saw zero recoverable syntax errors, and reached clean EOF. The `Source` tag appears on every record. This confirms the preliminary line scan but does not validate moves.

The publisher overview currently claims “over 13 million” games without an exact count, consistent with the observed record count. The publisher says the OTB Complete file is OTB-only and documents separate online exports; the input nevertheless contains 1,405,063 explicit `Mode "Correspondence"` tags. The Gigabase-specific policy rejects those records. Missing `Mode` is not per-event independent OTB evidence, so any local build must label its provenance as publisher-asserted OTB and must not claim independent event verification.

The source-tag census shows 11,390,350 records (84.7%) in publisher categories for which reliable upstream license information is often unavailable. The conservative candidate screen intersecting the existing Masters rating thresholds with both players marked `Strong` or `Confirmed` produces 943,493 records. Restricting that intersection to the academic, named federation, and Lichess API source groups with a specific publisher description of rights/source status gives 185,547 preliminary candidates. These are not accepted games: they precede play-mode, rating, date, termination, minimum-ply, legality, deduplication, overlap, and license review.

The publisher documents `SourceQuality -1` as verified player corrections but does not define values `1` through `5`; do not infer an order. It says current Elo values reflect values at the game date, and also describes using historical rating files to resolve missing data. The export does not distinguish verified rating values from historical reference assignments per record, and the publisher acknowledges identity/FIDE-ID/Elo errors can occur. Keep these fields labelled as publisher-supplied Elo until records can be independently verified. Identity confidence remains separate from play-mode provenance.

The publisher describes the Gigabase as CC BY-NC 4.0 and its commercial page distinguishes limited internal development from commercial use. This is not independent clearance for a public user-facing Evalis deployment, especially given upstream source-rights uncertainty. A local build can support evaluation, but a public integration requires separate publisher terms and further upstream rights review. The existing production catalog and releases remain unchanged; CGB3, CPI v2, and `games.idx` layouts are unchanged. The source-policy end-to-end tests, full build, deterministic rebuild, deep verification, local package/bootstrap/rollback, and restricted Docker API evaluation passed. No approved public or production package was made. The 943,493 rating-and-identity candidates form an upper bound below one million before other gates; the narrower source-qualified run retained 156,898.
