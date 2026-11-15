# Agent Handoff (chess-openings-db)

## Current objective

Finish PR #7 on `feat/gigabase-masters`: implement and document a source-qualified local Masters OTB evaluation from the user-supplied Lumbra's Gigabase archive, keep existing releases untouched, validate, push the branch, and create a dedicated Draft PR. Do not merge. The user authorized branch push and Draft PR creation.

## Current state

- Branch `feat/gigabase-masters` is based on `origin/main` commit `56d39184a8c68b3d4a5883a423cf79b2a325f4eb`. The PR search found no existing Gigabase PR. Changes are not yet committed or pushed.
- Archive `data/sources/lumbra/OTB_DATABASE.7z` is 2,570,013,439 bytes, with local SHA-256 `4e4209ccc1870e7fede557aae09b4dd40b721d4697db9b4c272ff0e4fbbdb500`. Its sole regular member, `LumbrasGigabase_OTB_Complete.pgn`, is 12,300,428,287 bytes and has local SHA-256 `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`. The publisher provides no archive checksum or exact game count; neither local hash is publisher-authenticated.
- Full streaming structural/header inventory: 13,450,696 records, zero recoverable syntax errors, clean EOF. It does not replay every record.
- The opt-in Masters policy is `corpus/gigabase_source_qualification.json`, ID `gigabase-otb/2026-11-r1`. It allows 11 reviewed source tags, requires both players Strong/Confirmed, uses publisher-supplied Elo with existing numeric thresholds, rejects explicit non-OTB modes and pre-1970 games, and relies on the publisher's archive-level OTB Complete assertion when `Mode` is absent. It does not independently certify event play mode, FIDE ratings, or per-game rights.
- Local evaluation accounting reconciles 13,450,696 encountered: 183,907 accepted unique, 11 confirmed duplicates, 1 superseded prior, 13,266,762 rejected by policy, and 15 quarantined stable game ID collisions. No policy-eligible move replay failed legality. Header/policy-rejected records were not all replayed. Retained games equal accepted unique.
- Deep-verified dataset in ignored `data/work/gigabase-eval/dataset-run2`, build ID `codb-masters-otb-1794715589069977685-7428-388d105284a591ff-cpi-d40`: 183,907 games, CPI v2 depth 40, 3,945,392 unique positions, and 169,136,912 manifest-reported artifact bytes. `source-attribution.jsonl` has 183,907 entries, 49,822,721 bytes, SHA-256 `3eecd9fb393396cfdbcbd3aa703b0cdcd5bf85b780689e45d85ff2e791ba94a1`; the CGB3, CPI v2, and `games.idx` binary layouts are unchanged.
- Independent deep-verified rebuild ID `codb-masters-otb-1794716076611874293-7628-d7c2bf1261c928a3-cpi-d40` matched manifest and report fields, including shard, lookup, CPI, and attribution hashes. See `benchmarks/gigabase/INDEPENDENT_REBUILD.json`.
- Docker image `codb-gigabase-eval:local` loaded the local Masters snapshot and existing Elite snapshot independently. Readiness, both collections, `e4,e5,Nf3` queries, and Masters PGN retrieval passed from a separate in-network client. Docker-in-Docker could not resolve workspace bind paths or reach the debug port from the host; the service used read-only named volumes. Details: `benchmarks/gigabase/DOCKER_API_CHECK.json`.
- Existing `data/releases/masters-otb` and `data/releases/elite-online` remain unchanged. Masters remains at 29,640 games / 29,892,368 deployed-tree bytes; Elite remains at 429,150 games / 210,308,033 bytes. No approved package or public dataset release was created. Publisher terms are CC BY-NC 4.0; heterogeneous upstream rights remain unresolved, so public Evalis use is not cleared.
- `./scripts/validate.sh` passed all eight gates in `codb-pr7-rust-work` using Rust 1.100.0. The workspace host does not have Cargo. A Clippy type-complexity error in `crates/cli/src/inventory.rs` was fixed with named aliases before the passing run.
- Final report and reproducibility evidence: `benchmarks/gigabase/FINAL_REPORT.md`, `REPRODUCIBILITY.json`, and `QUALITY_REPORT.json`. Canonical corpus, licensing, Docker, architecture, data contract, roadmap, README, and changelog docs have been updated.

## Next actions

1. Review `git diff --check`, the final status, and `MANIFEST.sha256` after updating this handoff.
2. Run `./scripts/validate.sh` once more after the final handoff and evidence edits.
3. Commit the reviewed changes in English, push `feat/gigabase-masters`, and open a Draft PR against `main` with the local-only rights limitations in the description.
4. Update this handoff with the commit and Draft PR URL, then validate and push that final documentation update. Do not merge.

## Evidence paths

- `benchmarks/gigabase/PLAN.md`, `INVENTORY.md`, `INVENTORY.json`, and `archive_inventory.json`
- `benchmarks/gigabase/FINAL_REPORT.md`, `REPRODUCIBILITY.json`, `INDEPENDENT_REBUILD.json`, `QUALITY_REPORT.json`, and `DOCKER_API_CHECK.json`
- `corpus/gigabase_source_qualification.json`
- `docs/CORPUS_BUILD.md`, `docs/CORPUS_AND_DEDUPLICATION.md`, `docs/LICENSING.md`, and `docs/DOCKER_DEPLOYMENT.md`
- The archive, extracted PGN, local build reports, and dataset are under ignored `data/`; never commit them.
