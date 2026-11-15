# Lumbra's Gigabase OTB Local Evaluation

**Date:** 2026-11-15

**Result:** Local evaluation complete; not release-eligible

**Scope:** PR #7 source qualification and reproducible Masters OTB evaluation

## Outcome

The supplied archive produced a deeply verified, 40-ply Masters OTB evaluation dataset with **183,907 retained games**. A second independent build reproduced the deterministic dataset artifacts and accounting. The API loaded this snapshot beside the existing Elite Online snapshot, answered separate queries for both collections, and retrieved a Masters PGN by game ID.

This dataset remains a local evaluation. It was not added to `corpus/catalog.json`, packaged, or published. The publisher provides no checksum for this archive; the publisher's CC BY-NC 4.0 statement does not independently clear heterogeneous upstream records. Public redistribution and Evalis deployment remain blocked pending publisher terms and upstream-rights review.

The old Masters and Elite releases under `data/releases/` were not changed. The new dataset and source archive remain in git-ignored `data/` paths.

## Input and qualification

The user-supplied `OTB_DATABASE.7z` is 2,570,013,439 bytes, with local SHA-256 `4e4209ccc1870e7fede557aae09b4dd40b721d4697db9b4c272ff0e4fbbdb500`. Its sole regular member, `LumbrasGigabase_OTB_Complete.pgn`, expands to 12,300,428,287 bytes and has local SHA-256 `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`. Neither local hash is publisher-authenticated. The publisher states “over 13 million” games but gives no exact count or checksum for this archive.

The streaming inventory parsed 13,450,696 PGN records, found zero recoverable structural syntax errors, and reached clean EOF. This header inventory did not replay every record. During curation, retained games passed the existing legal-move ingestion path; records rejected before move replay were not all tested for legality.

The opt-in policy is [`corpus/gigabase_source_qualification.json`](../../corpus/gigabase_source_qualification.json), policy ID `gigabase-otb/2026-11-r1`, SHA-256 `5d3577f4fc15b240e41c20c818a611db51f8929f00f1bdbbe41a77b5d1d33e39`. It:

- allows only the 11 reviewed source tags listed below;
- requires both player identity evidence values to be `Strong` or `Confirmed`;
- applies the existing Masters numeric Elo thresholds to publisher-supplied values, without independently certifying them as official FIDE Elo;
- rejects explicit non-OTB modes and games dated before 1970; and
- relies on the publisher's archive-level OTB Complete assertion when `Mode` is absent. This is not an event-level OTB audit.

Retained games by `Source` tag:

| Source tag | Games |
| --- | ---: |
| LichessBroadcastAPI | 120,753 |
| AnaSchaigorodsky | 24,051 |
| GreekBase | 19,748 |
| Britbase | 10,479 |
| Federscacchi | 4,305 |
| DanBase | 3,989 |
| ChessScotland | 495 |
| SchachverbandRuhrgebiet | 48 |
| NiedersächsischerSchachverband | 20 |
| CanadianChessFederation | 18 |
| NiederrheinischerSchachverband | 1 |
| **Total** | **183,907** |

These source tags and identity signals support the technical filter; they do not clear per-game upstream rights.

## Accounting

| Category | Records |
| --- | ---: |
| Total encountered | 13,450,696 |
| Accepted unique | 183,907 |
| Confirmed duplicates | 11 |
| Superseded prior | 1 |
| Rejected by policy | 13,266,762 |
| Rejected variant | 0 |
| Rejected illegal after move replay | 0 |
| Rejected malformed | 0 |
| Quarantined stable game ID collisions | 15 |
| **Reconciled total** | **13,450,696** |

The policy rejection reasons are: 11,462,752 unqualified source records, 1,280,905 with untrusted player identity, 521,056 below the rating threshold, 1,385 with online-event evidence in a Masters source, 401 rejected under the historical-rating rule, 213 missing a rating, and 50 with non-OTB play mode. The 15 stable game ID collisions were quarantined and counted separately. All accounting categories reconcile, and retained canonical games equal accepted unique games.

## Dataset and determinism

| Artifact | Result |
| --- | ---: |
| Dataset build ID | `codb-masters-otb-1794715589069977685-7428-388d105284a591ff-cpi-d40` |
| Retained games | 183,907 |
| CGB3 shard | 32,287,746 bytes; SHA-256 `713321e93357d929b07d5405cf69baa3b4a03f62d7785331ccb4e6bc3371402c` |
| `games.idx` | 2,942,576 bytes; SHA-256 `d530ac939720d270864b9fb0ec475db3a27cb3dcf354a2f3a9a0684decf36b88` |
| `positions.cpi` | 84,083,869 bytes; SHA-256 `01a38072ce7e07875373b9d6ce4a467082f2442e208df37ea62c55076b7c6ce8` |
| CPI depth and unique positions | 40 plies; 3,945,392 positions |
| Source attribution sidecar | 49,822,721 bytes; SHA-256 `3eecd9fb393396cfdbcbd3aa703b0cdcd5bf85b780689e45d85ff2e791ba94a1`; 183,907 entries |
| Manifest-reported artifact bytes (excludes `manifest.json`) | 169,136,912 |
| Immutable version directory bytes (includes `manifest.json`) | 169,139,860 |

The sidecar stores per-game source and identity attribution outside the CGB3 shard. Deep verification checks its checksum and one-to-one game-ID correspondence with `games.idx`; CGB3, CPI v2, and `games.idx` layouts remain unchanged.

The first completed build took 143.686 seconds and its measured peak process RSS was 17,456 KiB. CPI indexing took 12.722 seconds, measured 126,064 KiB peak process RSS under the configured 128 MiB limit, and used 130,961,894 bytes of peak temporary spill space. It processed all 183,907 retained games, emitted 3,945,392 unique positions, and generated two external-sort runs with one merge pass. These are separate measurements: spill-space size is not RSS.

An independent build, ID `codb-masters-otb-1794716076611874293-7628-d7c2bf1261c928a3-cpi-d40`, was deeply verified and matched deterministic fields, including accounting and rejection reasons, attribution, lookup index, CGB3 shard hash, CPI hash and statistics, game count, and snapshot bytes. The explicit field comparison is [`INDEPENDENT_REBUILD.json`](INDEPENDENT_REBUILD.json); build IDs and timestamps differ as expected. The aggregate quality report is [`QUALITY_REPORT.json`](QUALITY_REPORT.json).

The existing releases still point to their previous immutable snapshots: Masters has 29,640 games and 29,892,368 deployed-tree bytes; Elite has 429,150 games and 210,308,033 deployed-tree bytes. No release pointer was switched.

## Docker API evaluation

The local image `codb-gigabase-eval:local` (99,263,544 bytes) ran with a 512 MiB memory limit, two CPU limit, read-only root filesystem, and read-only dataset mounts. The service became ready about 5.2 seconds after container start. Its recorded process `VmHWM` was 220,396 KiB. Startup logs confirmed it loaded the Gigabase Masters snapshot and the existing Elite snapshot independently.

A separate client container on the Compose network passed readiness, collection listing, Masters and Elite position queries for `e4,e5,Nf3`, and Masters PGN retrieval (930-byte response). Masters returned 21,164 games (7,163 white wins, 8,887 draws, 5,114 black wins); Elite returned 30,390 games (14,325 white wins, 2,404 draws, 13,661 black wins). Ten warm sequential query samples per collection had median times of 0.812 ms for Masters and 0.839 ms for Elite. These are small local samples, not a load test. Full responses and timing samples are in [`DOCKER_API_CHECK.json`](DOCKER_API_CHECK.json).

The environment uses Docker-in-Docker. The daemon could not resolve the workspace path for the normal host bind mount, and the debug port was unreachable from the workspace host. The same service configuration was exercised with Docker-managed volumes populated from the verified local snapshots; both dataset mounts were read-only, and the in-network client reached the API. This limitation is specific to the test daemon.

## Reproduction and limits

The commands, source evidence, hashes, accounting, resource measurements, independent rebuild comparison, baseline IDs, and Docker results are summarized in [`REPRODUCIBILITY.json`](REPRODUCIBILITY.json). The safe local archive extraction steps are documented in [`docs/CORPUS_BUILD.md`](../../docs/CORPUS_BUILD.md).

The build report records `checksum_verified: true` for locally checked input bytes. The source inventory separately records that a publisher checksum is unavailable; do not interpret this flag or the local SHA-256 values as publisher authentication. The source license remains `Unknown` in the dataset metadata because rights at the record level have not been cleared. No corpus package, public release, or Evalis deployment was created.

## Verification

`./scripts/validate.sh` passed all eight gates in the pinned Rust validation container on 2026-11-15: formatting, strict Clippy, workspace tests, Python regression suites, production CPI and corpus-evidence validators, historical benchmark validation, documentation links/frontmatter, and `MANIFEST.sha256`. The workspace host has no Cargo installation; validation used the repository's Rust 1.100.0 container.
