# Lumbra's Gigabase OTB Local Evaluation

**Date:** 2026-11-15
**Status:** Verified local evaluation; not eligible for public or production release
**Scope:** PR #7 source qualification, local package installation, rollback, and restricted API evaluation

## Outcome

The r2 qualification policy retained **156,898 unique games** from 13,450,696 structurally parsed records. The resulting Masters dataset has a depth-40 CPI v2 index with 3,406,233 unique positions. A second independently executed build matched the deterministic artifacts and accounting, and both snapshots passed deep verification.

A checksum-covered package was created under ignored `data/work/gigabase-eval/`, extracted and verified, bootstrapped in an isolated root beside the prior Masters snapshot, and rolled back to that prior snapshot. Docker QA passed both with raw-PGN retrieval disabled and in a separate isolated run with retrieval enabled: a digest-pinned client's Masters and Elite PGN responses matched `codb get-game` byte for byte. The route-enabled run used read-only named-volume copies on a private Docker network with no published host ports. Host bind-mount behavior remains unverified. No approved catalog entry or public/production release was created, and the existing Masters and Elite release pointers were not changed.

The source screen cannot meet a one-million-game target without relaxing its reviewed gates: the full archive has only **943,493** records meeting the existing numeric rating threshold and both-player identity-confidence gates, before source allowlisting, date and play-mode checks, legality, termination, minimum length, and deduplication. The narrower r2 build retained 156,898. The additional records are not treated as quality-eligible Masters games merely to reach a target count.

## Before/after release metrics

| Measure | Previous Masters release | Gigabase r2 local candidate |
| --- | ---: | ---: |
| Accepted unique games | 29,640 | 156,898 |
| Records encountered | 227,489 | 13,450,696 |
| Unique indexed positions | 705,718 | 3,406,233 |
| CPI depth | 40 plies | 40 plies |
| Deployed/version directory bytes | 29,892,368 | 145,324,071 |
| Build wall time | 13.195 s | 74.874 s |
| CPI index wall time | 1.840 s | 9.623 s |
| Build / index peak process RSS | 23,015,424 / 47,996,928 bytes | 16,150,528 / 128,843,776 bytes |
| Docker position response matched CLI | Both collections matched in the baseline real smoke | Masters and Elite both matched in isolated QA |
| Stored-game retrieval | HTTP 200 in the baseline real smoke; exact CLI byte comparison was not recorded | HTTP 200 for both collections; exact PGN bytes matched `codb get-game` |
| License and OTB provenance | Lichess CC BY-SA 4.0 broadcasts with event-level OTB qualification | Publisher-claimed CC BY-NC 4.0; heterogeneous upstream rights unresolved; accepted OTB provenance relies on the publisher's archive assertion |

The prior wall times and RSS values are from the real-release corpus benchmark; r2 build and indexing values are from the recorded independent local rebuild. The new version directory includes release metadata. The Gigabase candidate is not the active Masters version. The route-enabled HTTP check used the same 429,150-game Elite snapshot and proves API retrieval behavior for the local candidate, not rights to serve it to users.

## Source and rights

The supplied `OTB_DATABASE.7z` is 2,570,013,439 bytes with local SHA-256 `4e4209ccc1870e7fede557aae09b4dd40b721d4697db9b4c272ff0e4fbbdb500`. Its sole regular member, `LumbrasGigabase_OTB_Complete.pgn`, is 12,300,428,287 bytes with local SHA-256 `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`. The [publisher download page](https://lumbrasgigabase.com/en/download-area-en/) lists OTB Complete version 2026-11-11 but provides no checksum or exact game count. Both hashes are local computations, not publisher authentication.

The publisher describes the main database as CC BY-NC 4.0 and documents heterogeneous upstream source categories on its [sources page](https://lumbrasgigabase.com/en/sources/). Its [B2B terms](https://lumbrasgigabase.com/en/commercial-licenses-en/) describe restricted internal development/closed-beta terms and separate commercial terms. These publisher statements do not independently establish rights for every upstream record. Source-level clearance remains unresolved, so the package is restricted to local internal evaluation. The restricted service run disabled raw-PGN retrieval; a separate private-network QA run enabled individual retrieval only to validate the API path and byte-match it with the CLI.

The inventory streamed all 13,450,696 PGN records, parsed every record boundary and header, encountered no recoverable structural syntax errors, and reached clean EOF. That scan did not replay every record. A second full extraction using `py7zr 1.1.4` matched the locally pinned archive and PGN member sizes and SHA-256 values. Packed-stream CRC metadata was unavailable, and the publisher supplied neither a checksum nor an exact game count. See [`ARCHIVE_RECHECK.json`](ARCHIVE_RECHECK.json); its local hashes do not authenticate the publisher's archive.

## Qualification policy r2

Policy ID: `gigabase-otb/2026-11-r2`
Policy SHA-256: `9da7691b2b440e66711750b31f12d849b2b32fe1e77bf8752157b475d80e2e94`

The opt-in Gigabase policy is separate from the existing Lichess Masters policy. It allows 11 reviewed `Source` tags, requires both player identity values to be Strong or Confirmed, applies the existing Masters numeric thresholds to publisher-supplied Elo, requires a complete valid calendar date, rejects dates before 1970, rejects explicit non-OTB modes, and rejects games with online-event evidence in a Masters source. When `Mode` is absent, OTB provenance still relies on the publisher's archive-level OTB Complete assertion. No independent event audit was performed. Publisher-supplied Elo is not independently certified as official FIDE Elo.

Accepted-game attribution by source tag is recorded outside CGB3. Counts are per source and may sum to more than the unique game count when a canonical game retained multiple attributions.

| Source tag | Attributed accepted games |
| --- | ---: |
| LichessBroadcastAPI | 105,652 |
| AnaSchaigorodsky | 16,933 |
| GreekBase | 16,647 |
| Britbase | 10,127 |
| Federscacchi | 3,643 |
| DanBase | 3,431 |
| ChessScotland | 380 |
| SchachverbandRuhrgebiet | 48 |
| NiedersächsischerSchachverband | 20 |
| CanadianChessFederation | 18 |
| NiederrheinischerSchachverband | 1 |

All 156,898 retained records had missing `Mode`, so their OTB provenance is publisher-asserted rather than event-verified. The date gate rejected 52,625 records reaching that policy gate because their date was absent, partial, or not a valid calendar date. A synthetic policy end-to-end fixture covers accepted missing-Mode records, explicit correspondence mode, unapproved sources, identity confidence, partial dates, and stable-ID collision accounting. The source inventory observed no malformed record example; rejected records were not all replayed for move legality.

## Fail-closed accounting

| Category | Records |
| --- | ---: |
| Total encountered | 13,450,696 |
| Accepted unique | 156,898 |
| Confirmed duplicates | 2 |
| Superseded prior | 0 |
| Rejected by policy | 13,293,782 |
| Rejected variant | 0 |
| Rejected illegal after move replay | 0 |
| Rejected malformed | 0 |
| Quarantined stable game ID collisions | 14 |
| **Reconciled total** | **13,450,696** |

Policy rejections were: 11,462,752 unqualified sources; 1,280,905 with untrusted player identity; 495,614 below rating threshold; 52,625 with unverified dates; 1,361 with online-event evidence in a Masters source; 267 with unverified historical ratings; 208 missing a rating; and 50 with explicit non-OTB mode. Rejection-reason counts reconcile to `rejected_by_policy`; retained games equal accepted unique.

Retained candidates passed the existing legal-move ingestion path. Header- or policy-rejected records were not all replayed for legality. The streaming inventory's zero structural syntax errors is not a substitute for move replay.

## Dataset, resource use, and determinism

| Artifact | Result |
| --- | ---: |
| Dataset build ID | `codb-masters-otb-1794721639733091532-17393-ebd97b2287ae7375-cpi-d40` |
| Games | 156,898 |
| CGB3 shard | 27,616,251 bytes; SHA-256 `4d12fdb06c02471841223f8dbeb25d2bb396260bf5a092c67e600159c28209a2` |
| `games.idx` | 2,510,432 bytes; SHA-256 `e2000c8edf7821eb75f70074de64cc84e4c6648e8013ec992715025d72ce7be6` |
| `positions.cpi` | 72,656,173 bytes; SHA-256 `fe221a2360785809505efa479aaafaefa1438bbe94b1a93c3842e233cf03996d` |
| CPI depth and unique positions | 40 plies; 3,406,233 positions |
| Source-attribution sidecar | 42,508,720 bytes; SHA-256 `61af71afba5cd51ab62ae0ecd00f67f37f0e8c592121f03a408ef86fd67ab28b`; 156,898 entries |
| Manifest-reported artifact bytes | 145,291,576 |

The independently rebuilt indexed snapshot was `codb-masters-otb-1794722373850654316-20882-6502a96286fd50cb-cpi-d40`. Both snapshots passed deep verification and matched accounting, policy reasons, source attribution, `games.idx`, CGB3, CPI statistics and hash, game count, and physical bytes. Build IDs and timestamps differ as expected. The CGB3, CPI v2, and `games.idx` binary layouts are unchanged. See [`INDEPENDENT_REBUILD.json`](INDEPENDENT_REBUILD.json).

The second build took 74.874 seconds with a sampled process `VmHWM` of 15,772 KiB. Its 40-ply index took 9.623 seconds; sampled process `VmHWM` was 125,824 KiB against the 128 MiB configured buffer limit. External sorting produced two runs and one merge pass, with 112,686,434 bytes of peak temporary spill space. RSS and spill-space measurements are separate. The resource sample and command are recorded in [`INDEPENDENT_REBUILD.json`](INDEPENDENT_REBUILD.json).

The final package is `data/work/gigabase-eval/packages-r2-final5/masters-otb-codb-masters-otb-1794721639733091532-17393-ebd97b2287ae7375-cpi-d40-local-evaluation.tar`, 145,346,560 bytes, local SHA-256 `5e0b01011d4c1007fa5c2cca30f442cc94cf58c3fa31d91c92dd30c69733744f`. Its detached checksum was verified after extraction, artifact re-hashing, deep verification, and an offline query. The source archive and PGN are not package members. The CGB3 database contains the accepted game records; the restricted Docker run disabled the stored-game route, while the separate internal QA run exercised it. The package and release metadata both say `local_evaluation_only`, `release_eligible: false`, and `public_release_eligible: false`.

## Source-to-canonical audit

A separate bounded-memory scan streamed the full source PGN and matched all **156,898** retained candidate games to source records. It compared all 15 persisted metadata fields, the primary source attribution, compact move sequence, starting position, and final legal board. Every retained candidate matched; there were zero missing candidates and zero mismatches in those checks. The report also contains 31 anonymized representative records across seven strata dimensions: source tag, year band, minimum rating band, game length, identity tier, result, and play-mode evidence. No game IDs, player names, event names, site strings, or raw PGN are included. See [`SOURCE_CANONICAL_AUDIT.json`](SOURCE_CANONICAL_AUDIT.json) and the reproducible runner [`audit_source_canonical.rs`](../../crates/cli/examples/audit_source_canonical.rs).

The source scan encountered 13,450,696 records, of which 156,914 passed the full Masters policy. That count matches aggregate build accounting: 156,898 accepted unique, 2 confirmed duplicates, 0 superseded, and 14 quarantined. The stable-ID join found 156,913 policy-pass records and all retained candidates; it also observed 15 repeated source hits (2 with canonical sidecar attribution and 13 without) and one policy-pass record without a candidate ID. Repeated stable-ID records may be confirmed duplicates or quarantined collisions; this audit reconciles aggregate accounting but does not independently reproduce each repeated record's deduplication verdict.

This audit uses the repository's existing Shakmaty PGN visitor and core stable-ID function, so it verifies source-to-storage traceability and serialization rather than independent parser correctness. The source SHA-256 was supplied from a separate local hash check and was not recomputed during the scan. Source-level rights, the publisher's archive-wide OTB assertion, and publisher-supplied Elo remain unresolved.

## Previous-release overlap and opening coverage

An aggregate audit compared the prior Masters release with new Gigabase records attributed to `LichessBroadcastAPI`. It wrote no names, event strings, raw PGN, or game IDs. Exact stable-ID overlap was zero. There were 16,173 identical full move sequences, 5,716 normalized same-player/date/move candidates, and 242 same-player/date/event/round/move candidates. The 242 are direct manual-review candidates, not automatically classified cross-release duplicates; no cross-release deletion or merge occurred. See [`SOURCE_OVERLAP.json`](SOURCE_OVERLAP.json).

The query counts below compare the same indexed positions before and after. They measure the number of records available at each opening position, not their quality or rights status.

| Opening | Moves | Existing Masters | Gigabase evaluation | Count ratio |
| --- | --- | ---: | ---: | ---: |
| Sicilian Defense | `e4,c5` | 6,043 | 30,550 | 5.055x |
| Open Game | `e4,e5` | 4,187 | 19,294 | 4.608x |
| Ruy Lopez | `e4,e5,Nf3,Nc6,Bb5` | 1,786 | 8,619 | 4.826x |
| Italian Game | `e4,e5,Nf3,Nc6,Bc4` | 1,147 | 4,382 | 3.820x |
| French Defense | `e4,e6,d4,d5` | 1,327 | 6,799 | 5.124x |
| Caro-Kann Defense | `e4,c6,d4,d5` | 1,116 | 5,252 | 4.706x |
| Queen's Gambit Declined | `d4,d5,c4,e6` | 1,172 | 5,646 | 4.817x |
| King's Indian Defense | `d4,Nf6,c4,g6` | 901 | 5,955 | 6.609x |
| English Opening | `c4,e5` | 503 | 2,600 | 5.169x |

At depth 40, unique positions increased from 705,718 to 3,406,233, while the accepted game count increased from 29,640 to 156,898. In-process warm lookup median was 5.219 microseconds for the old index and 5.478 microseconds for the new one; the respective p95 values were 10.868 and 11.049 microseconds. The lookup benchmark excludes HTTP, JSON, and SAN derivation. Full samples are in [`OPENING_USEFULNESS.json`](OPENING_USEFULNESS.json).

Eight less-common opening positions also gained coverage. These counts are corpus coverage, not a quality or licensing assessment.

| Opening | Moves | Existing Masters | Gigabase evaluation | Count ratio |
| --- | --- | ---: | ---: | ---: |
| King's Gambit Accepted | `e4,e5,f4,exf4,Nf3` | 6 | 76 | 12.667x |
| Budapest Gambit | `d4,Nf6,c4,e5,dxe5,Ng4` | 12 | 93 | 7.750x |
| Scandinavian Defense | `e4,d5,exd5,Qxd5,Nc3,Qa5` | 45 | 361 | 8.022x |
| Alekhine Defense | `e4,Nf6,e5,Nd5,d4,d6` | 59 | 546 | 9.254x |
| Dutch Defense | `d4,f5,c4,Nf6,g3` | 4 | 57 | 14.250x |
| Nimzo-Indian Defense | `d4,Nf6,c4,e6,Nc3,Bb4` | 1,011 | 4,740 | 4.688x |
| Benko Gambit | `d4,Nf6,c4,c5,d5,b5` | 77 | 521 | 6.766x |
| Trompowsky Attack | `d4,Nf6,Bg5,e6,e3` | 2 | 29 | 14.500x |

The transposition check queried `e4,e5,Nf3,Nc6,Bb5` and `e4,e5,Bb5,Nc6,Nf3`. Both sequences returned the same `PositionKey` and counts in each dataset: 1,786 for the previous Masters release and 8,619 for the Gigabase candidate. This confirms the two tested move orders converge in CPI; it is not an independent position-index oracle.

An independent selected-position oracle now streams all 156,898 canonical CGB3 games, replays each game through the published 40-ply CPI depth, and aggregates first-visit next-move W/D/L counts for 18 opening lines covering 17 distinct positions. The opening targets include common and less-common positions plus the Ruy Lopez transposition. All 18 line checks matched their direct CPI v2 lookups exactly, including candidate moves and W/D/L counts; the scan also replayed every indexed move without finding an illegal canonical move. The oracle excludes unknown results, though this candidate contains none. See [`CPI_ORACLE.json`](CPI_ORACLE.json) and the reproducible runner [`audit_cpi_oracle.rs`](../../crates/cli/examples/audit_cpi_oracle.rs).

This validates selected CPI aggregates from the stored canonical games. It shares the repository's `compute_position_key` helper with CPI. Source-to-canonical mapping is covered separately by the audit above, which uses the same Shakmaty parser and stable-ID function as ingestion.

The median number of known-result game observations per indexed position is 1 in both datasets. The mean is 1.631 in the previous index and 1.805 in the Gigabase candidate. Positions with at least 10 observations increased from 5,999 to 31,143; positions with at least 100 increased from 409 to 2,609. These figures come from CPI v2 move-result counts. Full distributions and method are in `OPENING_USEFULNESS.json`.

The quality profile is broader in years but heavily weighted toward recent games. The prior Masters corpus contains only 2026 records. The Gigabase candidate spans 1970–2026; decade counts are 751 (1970s), 2,472 (1980s), 4,461 (1990s), 29,664 (2000s), 8,753 (2010s), and 110,797 (2020s), so 70.6% is from 2020–2026. Estimated distinct player/event counts are 6,487 / 17,954 in the previous release and 20,310 / 9,997 in the candidate. The estimates use hashed names and are not event-quality checks.

The minimum-player-rating bins are `2000–2099` 1,549; `2100–2199` 2,637; `2200–2299` 9,248; `2300–2399` 8,045; `2400–2499` 4,646; `2500–2599` 2,074; `2600–2699` 1,034; and `2700–2799` 407 for the prior release. Gigabase has 6,668; 11,998; 51,823; 38,562; 25,860; 13,224; 6,177; and 2,565 in those bins, plus 21 in `2800–2899`. Gigabase Elo remains publisher-supplied and is not independently confirmed as official FIDE Elo. Missing raw time control increases from 11,351 to 153,788; unknown-result games decrease from 252 to zero. Machine-readable detail is in [`QUALITY_REPORT.json`](QUALITY_REPORT.json).

## Package, bootstrap, and rollback

The package was generated and verified only below ignored `data/work/gigabase-eval/`. Its checksum sidecar was verified after extraction, every listed artifact was re-hashed, the dataset passed `codb verify --deep`, and an offline `e4` query succeeded. The package omits the original `.7z` and `.pgn` inputs and excludes internal locks and prior pre-index versions.

Bootstrap retained the previous Masters immutable version and activated the evaluation version under an isolated deployment root. The offline `e4` query returned 70,676 games on the evaluation version. Rollback atomically restored the original Masters build ID `codb-masters-otb-1794689403953239696-66953-9aa01bd5606eaf65-cpi-d40`; its `e4` query returned 14,138 games. These writes remained under ignored `data/work/gigabase-eval/`; `data/releases/masters-otb` and `data/releases/elite-online` were not changed.

## Docker API evaluation

The restricted local Compose run loaded the 156,898-game evaluation snapshot beside the unchanged 429,150-game Elite snapshot. An independent, digest-pinned client confirmed readiness, collection listing, and `e4,e5,Nf3` position queries that matched the CLI for both collections. Masters returned 17,885 games at that line; Elite returned 30,390. The stored-game route returned 404 for both collections because `CODB_ENABLE_GAME_RETRIEVAL=false`. There were 96 concurrent requests: 48 successful position queries and 48 expected game-route 404 responses. Restart, deep mounted-dataset verification, and graceful SIGTERM shutdown passed. See [`DOCKER_API_CHECK.json`](DOCKER_API_CHECK.json).

A separate internal QA run enabled game retrieval only inside a private Docker network. The API returned PGN for one Masters game and one Elite game; each body matched the corresponding `codb get-game` output byte for byte (930 bytes for the Masters sample and 908 bytes for Elite). Position queries matched the CLI for both collections, all 96 concurrent mixed position/game requests succeeded, and startup/restart reached healthy in 6.725/6.541 seconds. Peak sampled process RSS was 213,652 KiB under the 512 MiB container limit. The container used read-only named-volume copies, a read-only root filesystem, UID `10001:10001`, and no published host ports. This is internal QA evidence, not an end-user deployment or rights clearance. See [`DOCKER_API_RETRIEVAL_QA.json`](DOCKER_API_RETRIEVAL_QA.json).

Both Docker runs used a read-only root filesystem, user `10001:10001`, read-only dataset mounts, no published host ports, a 512 MiB memory limit, and a two-CPU limit. Docker-in-Docker could not resolve workspace bind paths or expose the debug port to the workspace host, so both real runs used read-only Docker-managed volumes and an in-network client. Neither run proves host bind-mount behavior. See [`DOCKER_API_CHECK.json`](DOCKER_API_CHECK.json), [`DOCKER_API_RETRIEVAL_QA.json`](DOCKER_API_RETRIEVAL_QA.json), and the exact host-side verification procedure in [`docs/DOCKER_DEPLOYMENT.md`](../../docs/DOCKER_DEPLOYMENT.md).

## Verification and remaining limits

`python3 -m unittest scripts/test_gigabase_eval.py scripts/test_docker_deployment.py` passed, including a Docker fixture run with the game route disabled. The final `./scripts/validate.sh` result is recorded in [`REPRODUCIBILITY.json`](REPRODUCIBILITY.json).

The publisher supplied no archive checksum or exact game count. The publisher's license statement does not clear heterogeneous upstream records; no independent event-level OTB audit was performed; publisher Elo was not independently certified as official FIDE Elo; and header/policy-rejected records were not all replayed. The committed source inventory remains a header-level inventory and has no `Termination` distribution; the source-to-canonical report provides anonymized examples but does not provide an independent parser or a source-derived golden dataset for metadata-drift deduplication. The 242 same-player/date/event/round/move overlap candidates remain unresolved. The selected-position oracle is derived from canonical CGB3 records, and the separate source audit uses the repository parser and stable-ID function. The Docker daemon could not test host bind mounts. The evaluation package is not approved for public distribution, commercial use, or production service. The existing approved Masters and Elite releases remain the separate Lichess-derived datasets.

## Readiness conclusion

The full supplied PGN contains **13,450,696 observed records**. The r2 screen retained **156,898 legal, unique games** under the documented source, identity, date, rating, and play-mode gates. The remaining records are accounted for: 13,293,782 policy rejections, 2 confirmed duplicates, and 14 stable-ID collision quarantines. Source qualification is the largest rejection reason (11,462,752), followed by identity confidence (1,280,905), below-threshold ratings (495,614), and date validation (52,625); the other rejection counts are listed above. Only 943,493 records met the numeric rating and identity gates before the remaining filters, so one million is not attainable under those gates.

The 156,898 count is an r2 policy-screen count, **not an independently event-verified OTB count**. Every retained record lacks `Mode`, all rely on the publisher's archive-wide OTB claim, and publisher Elo has not been independently verified as contemporaneous FIDE Elo. No event-level OTB audit or upstream-rights clearance was completed. The candidate adds measured coverage and passed local package, rollback, API, and retrieval QA, but it is not ready to power the public or production Evalis Opening Explorer. The existing 29,640-game event-qualified Masters release and 429,150-game Elite release remain active and unchanged.
