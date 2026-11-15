# Agent Handoff (chess-openings-db)

## Current objective

Complete PR #7 on `feat/gigabase-masters`: implement and document a source-qualified local Masters OTB evaluation from the user-supplied Lumbra's Gigabase archive, keep existing releases untouched, validate, push the branch, and maintain a dedicated Draft PR. Do not merge. The user authorized branch push and Draft PR work.

## Current state

- Branch `feat/gigabase-masters` is based on `origin/main` commit `56d39184a8c68b3d4a5883a423cf79b2a325f4eb`; the r2 implementation and evidence are pushed. Draft PR [#7](https://github.com/Nerver-zip/chess-openings-db/pull/7) is open against `main`, its description reflects r2, and it has not been merged.
- User-supplied archive `data/sources/lumbra/OTB_DATABASE.7z` is 2,570,013,439 bytes, local SHA-256 `4e4209ccc1870e7fede557aae09b4dd40b721d4697db9b4c272ff0e4fbbdb500`. Its sole regular member `LumbrasGigabase_OTB_Complete.pgn` is 12,300,428,287 bytes, local SHA-256 `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`. The publisher supplied no archive checksum or exact game count; these local hashes are not publisher-authenticated.
- Streaming structural/header inventory found 13,450,696 records, no recoverable syntax errors, and clean EOF. It does not replay every record for move legality.
- Current opt-in policy is `gigabase-otb/2026-11-r2`, SHA-256 `9da7691b2b440e66711750b31f12d849b2b32fe1e77bf8752157b475d80e2e94`. It allows 11 reviewed source tags, requires both identities Strong/Confirmed, applies the existing numeric Masters thresholds to publisher-supplied Elo, requires a complete valid calendar date, rejects pre-1970 dates and explicit non-OTB modes, and rejects online-event evidence in a Masters source. All accepted records lacked `Mode`, so OTB provenance relies on the publisher's archive-level OTB Complete assertion. There was no event-level OTB audit; publisher Elo was not independently certified as FIDE Elo.
- r2 accounting reconciles 13,450,696 encountered: 156,898 accepted unique, 2 confirmed duplicates, 0 superseded, 13,293,782 policy rejects, 14 quarantined stable-ID collisions, and 0 variant, illegal, or malformed rejects. Retained canonical games equal accepted unique. Policy-rejected records were not all replayed.
- Deep-verified candidate build ID is `codb-masters-otb-1794721639733091532-17393-ebd97b2287ae7375-cpi-d40`, with 156,898 games, CPI v2 depth 40, and 3,406,233 unique positions. Manifest-reported artifact bytes: 145,291,576. An independent rebuild deep-verified and matched deterministic content. Its build sampled 15,772 KiB peak RSS; indexing sampled 125,824 KiB against a 128 MiB budget with two runs, one merge pass, and 112,686,434 bytes peak temporary spill. See `benchmarks/gigabase/INDEPENDENT_REBUILD.json` and `REPRODUCIBILITY.json`.
- Local evaluation package v5 is under ignored `data/work/gigabase-eval/packages-r2-final5/`; it is 145,346,560 bytes, local SHA-256 `5e0b01011d4c1007fa5c2cca30f442cc94cf58c3fa31d91c92dd30c69733744f`. Checksum, extraction, listed-artifact hashes, deep verification, and offline query passed. The source archive and PGN are not package members. Package/release metadata says local-only and not release-eligible.
- Bootstrap was tested under isolated ignored `data/work/gigabase-eval/deploy/masters-otb-r2`. The final post-Docker rollback restored the original Masters build ID `codb-masters-otb-1794689403953239696-66953-9aa01bd5606eaf65-cpi-d40`; its `e4` query returned 14,138 games. Production release roots `data/releases/masters-otb` and `data/releases/elite-online` remain unchanged.
- Final real Docker API evidence is `benchmarks/gigabase/DOCKER_API_CHECK.json`. The API served r2 Masters beside the unchanged 429,150-game Elite collection; both `e4,e5,Nf3` responses matched the CLI (17,885 Masters and 30,390 Elite). With `CODB_ENABLE_GAME_RETRIEVAL=false`, both game routes returned 404. Ninety-six concurrent requests yielded 48 successful queries and 48 expected 404s; restart, deep dataset verification, and SIGTERM shutdown passed. The container used a read-only root filesystem and mounts, user `10001:10001`, no published ports, and 512 MiB memory limit. Final sampled RSS was 209,848 KiB. Docker-in-Docker required read-only named volumes and an in-network client because workspace bind paths and host debug port were unavailable.
- Existing production releases remain the separate Lichess-derived datasets: Masters has 29,640 games and Elite has 429,150. The publisher's CC BY-NC 4.0 statement does not resolve upstream rights. Do not publicly distribute or deploy this evaluation; no production catalog or release was changed.
- Local package, bootstrap, rollback, opening coverage, overlap, source qualification, and Docker evidence are documented in `benchmarks/gigabase/FINAL_REPORT.md` and supporting JSON. The README, changelog, canonical corpus/licensing/API/architecture documents, and policy tests now describe r2.
- Final `./scripts/validate.sh` passed all eight gates in Docker container `codb-pr7-rust-work` at `/workspace`, using Rust 1.100.0. This run included the latest README, CLI policy expectation, Docker evidence, and refreshed manifest. Cargo is unavailable on the workspace host. The result is recorded in `benchmarks/gigabase/REPRODUCIBILITY.json`.

## Next actions

1. Continue review on Draft PR #7; keep it unmerged.
2. Public redistribution and Evalis deployment remain blocked pending publisher terms and upstream-rights review.

## Evidence paths

- `benchmarks/gigabase/PLAN.md`, `INVENTORY.md`, `INVENTORY.json`, and `archive_inventory.json`
- `benchmarks/gigabase/FINAL_REPORT.md`, `REPRODUCIBILITY.json`, `INDEPENDENT_REBUILD.json`, `QUALITY_REPORT.json`, `DOCKER_API_CHECK.json`, `OPENING_USEFULNESS.json`, and `SOURCE_OVERLAP.json`
- `corpus/gigabase_source_qualification.json`
- `docs/CORPUS_BUILD.md`, `docs/CORPUS_AND_DEDUPLICATION.md`, `docs/LICENSING.md`, and `docs/DOCKER_DEPLOYMENT.md`
- Source archive, extracted PGN, candidate datasets, package, and deployment experiments are under ignored `data/`; never commit them.
