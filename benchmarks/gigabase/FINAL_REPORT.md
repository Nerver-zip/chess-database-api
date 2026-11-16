# Lumbra's Gigabase OTB Local Evaluation

**Date:** 2026-11-16
**Status:** Current r3 local evaluation; not eligible for public or production release
**Scope:** Full-source r3 policy analysis, production build, source traceability, local package, isolated bootstrap/rollback, and internal Docker API evaluation

## Current r3 evaluation

Policy `gigabase-otb/2026-11-r3` (SHA-256 `93b9d0c7fd8eb35bb3bf36425a5e7b02f95fa80910942bbda615a453bd2ef3a9`) trusts the publisher's OTB Complete archive assertion unless a record contains concrete contradictory metadata. It accepts any non-placeholder `Source` tag, preserves source and publisher identity-evidence labels, retains valid partial or unknown date components, and applies symmetric rating/title strength rules. Missing `Mode` remains publisher-asserted OTB; explicit non-OTB modes and contradictory event/site evidence are rejected. Publisher-supplied Elo and GM labels are not independently verified. This qualification policy applies only to the opt-in Gigabase evaluation and does not change the approved Masters or Elite policy.

### Full-source analysis and build

The locally pinned PGN is 12,300,428,287 bytes with SHA-256 `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`. A full streaming header scan parsed all 13,450,696 records, found no recoverable syntax errors, and reached clean EOF. It counted 3,296,795 r3 header candidates; the production build accounted for every source record and retained 3,284,429 unique legal games.

The policy-impact scan found 3,650,416 records meeting an r3 strength rule before source, identity, play-mode, date, and other header gates. The final header candidates break down by strength rule as 3,206,053 original numeric-rule records, 65,629 records meeting the 2400/1800 rule, 23,725 meeting a GM-versus-rated-opponent rule, and 1,388 with two exact GM labels. Candidate dates include 2,630,197 complete dates, 663,864 valid partial dates, and 2,734 unknown dates; 10,081 candidates have dates before 1970. These are publisher-supplied Elo and title labels, not independent FIDE or historical-title validation. See [POLICY_IMPACT.json](POLICY_IMPACT.json) for overlapping rejection counts and the full source-tag breakdown.

| Accounting category | Records |
| --- | ---: |
| Total encountered | 13,450,696 |
| Accepted unique | 3,284,429 |
| Confirmed duplicates | 4,018 |
| Superseded prior records | 506 |
| Rejected by policy | 10,153,901 |
| Rejected variant / illegal / malformed | 0 / 0 / 0 |
| Quarantined stable-ID collisions | 7,842 |
| **Reconciled total** | **13,450,696** |

Retained games equal accepted unique games. The 10,153,901 records rejected by header or policy gates were not all replayed for legality. Candidate build ID: `codb-masters-otb-1794774904566910568-6701-5698b67c3952ca6b-cpi-d40`. The build passed deep verification. No independent full rebuild of r3 has been completed.

### CPI and resource measurements

The depth-40 CPI v2 index contains 51,909,604 unique positions, 55,864,091 unique position/move pairs, and 127,444,117 raw observations. `positions.cpi` is 1,107,897,085 bytes (SHA-256 `284898e18f4a4aba0b56d383e8b1a163790a85e1081749f0ff9204fdab5a971c`). The source-attribution sidecar is 1,001,755,579 bytes (SHA-256 `7d11330a704746306d53afcb39bcc74ccf95c7a11cf55346374dd05188b45cbc`).

The build took 4,723.004 seconds and sampled 67,895,296 bytes peak RSS. Indexing took 204.8 seconds under `--memory-limit-mib 128`; sampled peak anonymous RSS was 122,548,224 bytes, below 128 MiB. Total sampled process RSS reached 1,027,588,096 bytes because memory-mapped CPI pages were file-backed; peak workspace allocation was 5,913,501,696 bytes. The indexer emitted 35 runs, used one merge pass, and peaked at 2,132,859,200 bytes of spill. These values are detailed in `archive_inventory.json` and its `local_evaluation` record.

### Selected-position CPI oracle

The oracle replayed all 3,284,429 canonical games through the 40-ply index depth and independently aggregated next-move and WDL counts for 18 opening lines representing 17 positions. All direct CPI v2 lookups matched, with zero mismatches. The check starts from canonical CGB3 records, shares the production position-key function, and checks only the listed positions; it does not replace the source-to-canonical audit or a complete independent CPI rebuild. Machine report: [CPI_ORACLE_R3.json](CPI_ORACLE_R3.json), SHA-256 `182c1a19914ee9dea7dbbcd67f0c2d5279c847a32eef28a91aa3cd60e8b6d431`.

### Full-source traceability and deduplication audit

The corrected source-to-canonical scan matched all **3,284,429** retained games to source records. It compared persisted metadata, primary source attribution, compact moves, starting positions, and final legal boards, with zero mismatches in every comparison. Replay through the production deduplicator classified the 3,296,795 policy-pass records as 3,127,272 unique games, 157,157 distinct-game transpositions, 4,018 confirmed duplicates, 506 superseded records, and 7,842 stable-ID collision quarantines; these dispositions match build accounting. The expected and observed attribution sidecars each contain 3,287,619 pairs across 3,284,429 games, with no missing or unexpected pairs. The report contains 64 anonymized examples across its permitted strata; strata with fewer than 50 games are suppressed. See [SOURCE_CANONICAL_AUDIT.json](SOURCE_CANONICAL_AUDIT.json).

A separate source-derived audit compared all **12,366** confirmed-duplicate, supersession, and collision records with canonical retrieval. All lookups succeeded, and the report emits 50 anonymized examples without player/event values, game IDs, or raw PGN. It found 162 strict source move-prefix cases; all share the starting position and are confirmed duplicates. Of these, 161 end at a different final position and satisfy the strict truncation comparison; one ends at the same final position and is reported separately. The prefix classification reconciles, and there are no reverse-prefix cases. Metadata differences are recorded by field and boolean comparison only. See [SOURCE_DEDUPE_AUDIT.json](SOURCE_DEDUPE_AUDIT.json).

Both audits reuse the repository's Shakmaty visitor, stable-ID calculation, qualification policy, and production deduplicator. They verify source-to-storage traceability and sidecar accounting, not an independent parser or deduplication implementation. The separate selected-position CPI oracle checks the canonical index and shares the production position-key function.

### Prior Masters overlap

The r3 evaluation has zero exact stable-ID overlap with the active 29,640-game Masters snapshot. It contains 16,488 LichessBroadcastAPI-attributed games with identical full move sequences, 5,829 same-player/date/move records, and 259 same-player/date/event/round/move candidates. Each of the 259 signatures maps one-to-one to a distinct prior game, with no ambiguity or prior-record reuse. All agree on date, round, result, and ECO; player names differ in 5 White and 11 Black fields, event strings in 84, sites in 43, White Elo in 80, and Black Elo in 79. All new rows lack a source ID present in the corresponding prior rows, and all 259 stable-ID preimages differ. These remain manual-review candidates rather than confirmed duplicates; no cross-release deletion or merge occurred. See [SOURCE_OVERLAP.json](SOURCE_OVERLAP.json).

### Package and service status

The final local-evaluation package is 2,700,902,400 bytes with SHA-256 `297b7d2ab67951fd9655c35f6993da4d0fb5dd55ee763b62f9249c1804019247`. Its checksum, extraction, artifact hashes, deep verification, and offline query passed; the source archive and PGN are not package members. The packaged quality report was regenerated from the final indexed snapshot, matches its dataset build ID, and retains the same distributions. Bootstrap of this package in an isolated root activated the r3 build and returned 1,469,608 games for `e4`; rollback restored the previous 29,640-game Masters snapshot. Production Masters and Elite pointers remain unchanged.

The internal real-data Docker API smoke passed with both Masters and Elite mounted as read-only named-volume copies on a private network, no host ports, a read-only root filesystem, and UID `10001:10001`. Position responses matched the CLI for both collections, stored PGNs matched `codb get-game` byte-for-byte, all 96 concurrent requests succeeded, four invalid-dataset starts were rejected, restart and post-restart deep verification passed, and SIGTERM shutdown was graceful. Startup/restart to healthy took 46.162/45.655 seconds; sampled peak RSS was 208,265,216 bytes. This does not test host bind mounts or make the service user-accessible.

The candidate remains a local evaluation and is not approved for public or production use. The publisher supplied no archive checksum, the archive states CC BY-NC 4.0, heterogeneous upstream rights remain unresolved, and no project-specific written terms or upstream-rights clearance are recorded.

Machine evidence: [r3 reproducibility record](R3_REPRODUCIBILITY.json), [release manifest](RELEASE_R3.json), [build report](BUILD_REPORT_R3.json), [index report](INDEX_REPORT_R3.json), [quality report](QUALITY_REPORT_R3.json), [resource metrics](RESOURCE_METRICS_R3.json), [Docker API report](DOCKER_API_R3.json), [policy impact](POLICY_IMPACT.json), [selected-position CPI oracle](CPI_ORACLE_R3.json), [source-to-canonical audit](SOURCE_CANONICAL_AUDIT.json), [source dedupe audit](SOURCE_DEDUPE_AUDIT.json), [cross-release overlap](SOURCE_OVERLAP.json), and [archive inventory](archive_inventory.json). The historical r2 evaluation and its package/API evidence are preserved in the following sections and are not current r3 measurements.

## Outcome (historical r2)

The r2 qualification policy retained **156,898 unique games** from 13,450,696 structurally parsed records. The resulting Masters dataset has a depth-40 CPI v2 index with 3,406,233 unique positions. A second independently executed build matched the deterministic artifacts and accounting, and both snapshots passed deep verification.

A checksum-covered package was created under ignored `data/work/gigabase-eval/`, extracted and verified, bootstrapped in an isolated root beside the prior Masters snapshot, and rolled back to that prior snapshot. Docker QA passed both with raw-PGN retrieval disabled and in a separate isolated run with retrieval enabled: a digest-pinned client's Masters and Elite PGN responses matched `codb get-game` byte for byte. The route-enabled run used read-only named-volume copies on a private Docker network with no published host ports. Host bind-mount behavior remains unverified. No approved catalog entry or public/production release was created, and the existing Masters and Elite release pointers were not changed.

The source screen cannot meet a one-million-game target without relaxing its reviewed gates: the full archive has only **943,493** records meeting the existing numeric rating threshold and both-player identity-confidence gates, before source allowlisting, date and play-mode checks, legality, termination, minimum length, and deduplication. The narrower r2 build retained 156,898. The additional records are not treated as quality-eligible Masters games merely to reach a target count.

## Before/after release metrics (historical r2)

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

## Source and rights (historical r2)

The supplied `OTB_DATABASE.7z` is 2,570,013,439 bytes with local SHA-256 `4e4209ccc1870e7fede557aae09b4dd40b721d4697db9b4c272ff0e4fbbdb500`. Its sole regular member, `LumbrasGigabase_OTB_Complete.pgn`, is 12,300,428,287 bytes with local SHA-256 `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`. The [publisher download page](https://lumbrasgigabase.com/en/download-area-en/) lists OTB Complete version 2026-11-11 but provides no checksum or exact game count. Both hashes are local computations, not publisher authentication.

The publisher's [download page](https://lumbrasgigabase.com/en/download-area-en/) lists OTB Complete version 2026-11-11 as one PGN containing its OTB PGN files except LichessBroadcastDB, and displays CC BY-NC 4.0 terms; no archive checksum was visible on the reviewed page. The [sources page](https://lumbrasgigabase.com/en/sources/) lists heterogeneous inputs and says reliable or binding license information is unavailable for some abandoned independent collections and historical archives. The publisher's [B2B page](https://lumbrasgigabase.com/en/commercial-licenses-en/) describes free internal development and a closed beta limited to 50 unpaid testers for up to six months, permits internal derived metadata, and prohibits end-user bulk exports and direct raw-PGN downloads. It says commercial license fees apply when a project goes live and becomes accessible to users; its form also includes a non-commercial use option. The page does not specify whether a public non-commercial product or per-game PGN endpoint is permitted under those conditions. No Evalis project registration or written permission is recorded. The official [CC BY-NC 4.0 deed](https://creativecommons.org/licenses/by-nc/4.0/) requires attribution and excludes commercial use; it also notes additional permissions may be needed. These publisher statements and the CC license claim do not establish rights in every upstream record. Therefore the package remains local-evaluation-only. The restricted service run disabled raw-PGN retrieval; a separate private-network QA run enabled retrieval only for internal API correctness checks. Machine-readable review: [`PUBLISHER_TERMS_REVIEW.json`](PUBLISHER_TERMS_REVIEW.json).

The inventory streamed all 13,450,696 PGN records, parsed every record boundary and header, encountered no recoverable structural syntax errors, and reached clean EOF. That scan did not replay every record. A second full extraction using `py7zr 1.1.4` matched the locally pinned archive and PGN member sizes and SHA-256 values. Packed-stream CRC metadata was unavailable, and the publisher supplied neither a checksum nor an exact game count. See [`ARCHIVE_RECHECK.json`](ARCHIVE_RECHECK.json); its local hashes do not authenticate the publisher's archive.

## Qualification policy r2 (historical r2)

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

## Fail-closed accounting (historical r2)

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

## Dataset, resource use, and determinism (historical r2)

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

### Provenance-corrected rebuild and package (historical r2)

A follow-up full build used a generated one-source manifest pinned to the registered PGN member SHA-256 `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`. The manifest labels the license `Publisher claim: CC BY-NC 4.0; upstream rights unresolved`; the publisher checksum remains null. Its SHA-256 is `4c56144c8318abd4d8e73e64488c0e23d9b6d5558d39413d5cc8977190b2732f`. The build report and persistent dataset manifest both matched the registered source filename, URL, size, local hash, and license label. `checksum_verified: true` here records CODB's match to the local manifest hash and does not authenticate the publisher's archive.

The indexed build is `codb-masters-otb-1794734897423894473-41647-72708103e81b880b-cpi-d40`. It accounted for 13,450,696 records and retained 156,898 games with the same reconciled categories above. Build time was 71.153 seconds; the depth-40 index took 9.560 seconds under the 128 MiB memory limit, with two external runs, one merge pass, and 112,686,434 bytes peak temporary spill. Deep verification passed. The CGB3 shard, `games.idx`, CPI v2, and source-attribution sidecar were byte-identical to the original r2 package's corresponding artifacts; their SHA-256 values remain `4d12fdb06c02471841223f8dbeb25d2bb396260bf5a092c67e600159c28209a2`, `e2000c8edf7821eb75f70074de64cc84e4c6648e8013ec992715025d72ce7be6`, `fe221a2360785809505efa479aaafaefa1438bbe94b1a93c3842e233cf03996d`, and `61af71afba5cd51ab62ae0ecd00f67f37f0e8c592121f03a408ef86fd67ab28b`, respectively.

The supplemental package is `data/work/gigabase-eval/packages-provenance-r1/masters-otb-codb-masters-otb-1794734897423894473-41647-72708103e81b880b-cpi-d40-local-evaluation.tar`, 145,346,560 bytes, local SHA-256 `2902a8c9668457c58fb19bceefa081636ba1d4aab193dd645cb8592766db28f8`. Its checksum sidecar, extraction, deep verification, and offline query passed; the archive and PGN are not package members. The package was bootstrapped under ignored `data/work/gigabase-eval/docker-provenance-r1/deploy/masters-otb` beside the retained previous Masters snapshot. Its offline `e4` query returned 70,676 games.

The provenance-corrected package then passed the real `scripts/docker_smoke.py --real` flow with raw-PGN retrieval enabled for internal QA. A separate in-network client matched Masters and Elite position results and retrieved PGNs byte-for-byte equal to `codb get-game`; **96/96** concurrent mixed requests passed. Restart, post-restart deep verification, and graceful shutdown passed. Startup/restart to healthy took 6.653/6.597 seconds, sampled peak RSS was 212,968 KiB, both dataset mounts were read-only named volumes, the root filesystem was read-only, and no host ports were published. The smoke cleaned up its temporary containers, volumes, and network. After the smoke, the isolated deployment root rolled back to the previous 29,640-game Masters snapshot. A direct read-only bind probe failed because the Docker daemon could not find the checkout path; full Compose host bind-mount behavior remains unverified. This was internal QA, did not make the dataset user-accessible, and does not clear source rights for end-user service. See [`DOCKER_API_PROVENANCE_RETRIEVAL_QA.json`](DOCKER_API_PROVENANCE_RETRIEVAL_QA.json). The package remains local-evaluation-only and ineligible for public or production use.

## Source-to-canonical audit (historical r2)

A separate bounded-memory scan streamed the full source PGN and matched all **156,898** retained r2 candidate games to source records. It compared all 15 persisted metadata fields, the primary source attribution, compact move sequence, starting position, and final legal board. Every retained candidate matched; there were zero missing candidates and zero mismatches in those checks. The report contains 27 anonymized examples across seven single-dimension strata: source tag, year band, minimum rating band, game length, identity tier, result, and play-mode evidence. Four strata with fewer than 50 games are suppressed. Each example includes one stratum label and comparison booleans only; no game IDs, player names, event names, site strings, or raw PGN are included. See [`SOURCE_CANONICAL_AUDIT_R2.json`](SOURCE_CANONICAL_AUDIT_R2.json) and the reproducible runner [`audit_source_canonical.rs`](../../crates/cli/examples/audit_source_canonical.rs).

The source scan encountered 13,450,696 records, of which 156,914 passed the full Masters policy. Replay through the repository's disk-backed production deduplicator produced 156,665 unique IDs, 233 distinct-game transpositions, 2 confirmed duplicates, 0 superseded records, and 14 quarantined stable-ID collisions. These outcomes match build accounting. The 15 repeated source hits for candidate IDs resolve to 1 confirmed duplicate and 14 quarantined collisions; one additional confirmed duplicate has no candidate-ID hit. The earlier sidecar-membership counts (2 repeated records attributed to canonical games and 13 without attribution) describe membership only, not deduplication verdicts. The audit built the expected full attribution-pair set and compared it in both directions with the candidate sidecar: 156,900 expected and observed pairs across 156,898 games, with zero missing or unexpected pairs.

A follow-up source-derived deduplication audit ran against the provenance-corrected r2 build and reconciled the same 13,450,696 records and 156,914 policy-pass rows. It compared all 16 duplicate/collision records to their canonical CGB3 games: 2 confirmed duplicates and 14 stable-ID collision quarantines. Fifteen comparisons had identical move sequences. The remaining confirmed duplicate was a strict source-prefix truncation: its start position matched and its final position differed, with the source moves verified as a prefix of the longer canonical record. All 16 canonical lookups succeeded; 13 quarantined source attributions were absent from the canonical sidecar. The report contains only field labels, disposition labels, and booleans, with no IDs, player/event/site values, or PGN. This uses the production parser, policy, stable-ID function, and disk-backed deduplicator; it adds actual source-row traceability evidence but is not an independent implementation. See [`SOURCE_DEDUPE_AUDIT_R2.json`](SOURCE_DEDUPE_AUDIT_R2.json) and [`audit_dedupe_source.rs`](../../crates/cli/examples/audit_dedupe_source.rs).

This replay uses the production Deduplicator and is not an independent deduplication algorithm. The audit also uses the repository's existing Shakmaty PGN visitor and core stable-ID function, so it verifies source-to-storage traceability and serialization rather than independent parser correctness. The source SHA-256 was supplied from a separate local hash check and was not recomputed during the scan. Source-level rights, the publisher's archive-wide OTB assertion, and publisher-supplied Elo remain unresolved.

The source audit's aggregate outcomes anchor six offline synthetic cases in [`DEDUPE_GOLDEN.json`](DEDUPE_GOLDEN.json): normalized metadata drift, stable-ID collisions, identical moves with distinct metadata, prefix supersession, and divergent games. The fixture includes no source-row metadata, identifiers, or moves; its source link is limited to aggregate duplicate and collision counts. A Rust integration test runs the cases through the disk-backed production deduplicator. This protects the current behavior but is not independent deduplication validation. The separate source-derived audit now supplies anonymized examples for actual metadata drift and the observed prefix/truncation case.

### Independent PGN parser spot checks (historical r2)

The original audit used `python-chess` 1.11.2 on 40,000 systematic byte offsets and compared 144 unique stable-ID hits with `codb get-game`. It found no parser errors or illegal sampled mainlines, move mismatches, start-position mismatches, final-board mismatches, retrieval failures, or unreconciled source rows. It covered year, rating, length, source, and identity dimensions but missed planned quotas. Its mmap-based process peak was 1,386,212 KiB. It used an earlier candidate build, and the sampled report suppresses per-field metadata differences and small cells. See [`INDEPENDENT_PGN_AUDIT.json`](INDEPENDENT_PGN_AUDIT.json).

A supplemental deterministic seek/read sample used 300,000 uniform byte windows against the provenance-corrected candidate. It examined 300,000 record windows, consumed 434,132,364 logical source bytes, and independently parsed 4,109 header-prefiltered candidate-ID hits. It selected 227 unique candidate games by per-stratum minimum hash. All 227 were legal and matched persisted metadata, compact moves, starting position, final board, and source attribution exactly; parser errors, illegal mainlines, missing candidate joins, retrieval failures, and comparison mismatches were zero. Ten source-tag strata had enough candidate games to be quota-supported: six met ten games and four did not (CanadianChessFederation 0/10, ChessScotland 5/10, NiedersächsischerSchachverband 0/10, SchachverbandRuhrgebiet 0/10). The allowed NiederrheinischerSchachverband tag had one candidate and did not meet the ten-game support threshold. All supported year, rating, game-length, and identity strata met quota. The audit took 100.238 seconds and measured a 339,388 KiB process peak; it used no mmap, capped sampled lines at 4 MiB and records at 64 MiB, and held candidate membership in an in-memory SQLite database without a fixed RSS ceiling. See [`INDEPENDENT_PGN_AUDIT_TARGETED.json`](INDEPENDENT_PGN_AUDIT_TARGETED.json) and the [local runbook](INDEPENDENT_PGN_AUDIT_RUNBOOK.md).

Neither report is a full independent parse of the 12.3 GB PGN. The source hash in the targeted report was copied from the local inventory and was not recomputed. The external parser runner and GPL-3.0-or-later wheel are local-only and absent from the PR; their exact local hashes and license are recorded in the reports. Stable-ID joining was independently reimplemented, while candidate games were retrieved through the production CLI rather than an independent CGB3 decoder. These samples do not establish event-level OTB play, official FIDE Elo provenance, or source rights.

## Previous-release overlap and opening coverage (historical r2)

An aggregate r2 audit compared the prior Masters release with new Gigabase records attributed to `LichessBroadcastAPI`. It wrote no names, event strings, raw PGN, or game IDs. Exact stable-ID overlap was zero. There were 16,173 identical full move sequences, 5,716 normalized same-player/date/move candidates, and 242 same-player/date/event/round/move candidates. The 242 signatures each matched one distinct prior game, with no repeated or ambiguous pairings. All 242 agree on date, round, result, and ECO; raw player names differ in 5 White and 11 Black fields, event strings in 83 cases, site strings in 43, White Elo in 78, and Black Elo in 77. The new rows lack a persisted source ID in all 242 cases, while the corresponding prior rows have one; all stable-ID preimages therefore differ. These are strong manual-review candidates, not confirmed duplicates: same moves and metadata do not establish shared source identity, and the prior release has no comparable attribution sidecar. No cross-release deletion or merge occurred. See [`SOURCE_OVERLAP_R2.json`](SOURCE_OVERLAP_R2.json).

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

An independent selected-position r2 oracle streamed all 156,898 canonical CGB3 games, replayed each game through the published 40-ply CPI depth, and aggregated first-visit next-move W/D/L counts for 18 opening lines covering 17 distinct positions. The opening targets include common and less-common positions plus the Ruy Lopez transposition. All 18 line checks matched their direct CPI v2 lookups exactly, including candidate moves and W/D/L counts; the scan also replayed every indexed move without finding an illegal canonical move. The oracle excludes unknown results, though this candidate contains none. See [`CPI_ORACLE_R2.json`](CPI_ORACLE_R2.json) and the reproducible runner [`audit_cpi_oracle.rs`](../../crates/cli/examples/audit_cpi_oracle.rs).

This validates selected CPI aggregates from the stored canonical games. It shares the repository's `compute_position_key` helper with CPI. Source-to-canonical mapping is covered separately by the audit above, which uses the same Shakmaty parser and stable-ID function as ingestion.

The median number of known-result game observations per indexed position is 1 in both datasets. The mean is 1.631 in the previous index and 1.805 in the Gigabase candidate. Positions with at least 10 observations increased from 5,999 to 31,143; positions with at least 100 increased from 409 to 2,609. These figures come from CPI v2 move-result counts. Full distributions and method are in `OPENING_USEFULNESS.json`.

The quality profile is broader in years but heavily weighted toward recent games. The prior Masters corpus contains only 2026 records. The Gigabase candidate spans 1970–2026; decade counts are 751 (1970s), 2,472 (1980s), 4,461 (1990s), 29,664 (2000s), 8,753 (2010s), and 110,797 (2020s), so 70.6% is from 2020–2026. Estimated distinct player/event counts are 6,487 / 17,954 in the previous release and 20,310 / 9,997 in the candidate. The estimates use hashed names and are not event-quality checks.

The minimum-player-rating bins are `2000–2099` 1,549; `2100–2199` 2,637; `2200–2299` 9,248; `2300–2399` 8,045; `2400–2499` 4,646; `2500–2599` 2,074; `2600–2699` 1,034; and `2700–2799` 407 for the prior release. Gigabase has 6,668; 11,998; 51,823; 38,562; 25,860; 13,224; 6,177; and 2,565 in those bins, plus 21 in `2800–2899`. Gigabase Elo remains publisher-supplied and is not independently confirmed as official FIDE Elo. Missing raw time control increases from 11,351 to 153,788; unknown-result games decrease from 252 to zero. Machine-readable detail is in [`QUALITY_REPORT.json`](QUALITY_REPORT.json).

## Package, bootstrap, and rollback (historical r2)

The package was generated and verified only below ignored `data/work/gigabase-eval/`. Its checksum sidecar was verified after extraction, every listed artifact was re-hashed, the dataset passed `codb verify --deep`, and an offline `e4` query succeeded. The package omits the original `.7z` and `.pgn` inputs and excludes internal locks and prior pre-index versions.

Bootstrap retained the previous Masters immutable version and activated the evaluation version under an isolated deployment root. The offline `e4` query returned 70,676 games on the evaluation version. Rollback atomically restored the original Masters build ID `codb-masters-otb-1794689403953239696-66953-9aa01bd5606eaf65-cpi-d40`; its `e4` query returned 14,138 games. These writes remained under ignored `data/work/gigabase-eval/`; `data/releases/masters-otb` and `data/releases/elite-online` were not changed.

## Docker API evaluation (historical r2)

The restricted local Compose run loaded the 156,898-game evaluation snapshot beside the unchanged 429,150-game Elite snapshot. An independent, digest-pinned client confirmed readiness, collection listing, and `e4,e5,Nf3` position queries that matched the CLI for both collections. Masters returned 17,885 games at that line; Elite returned 30,390. The stored-game route returned 404 for both collections because `CODB_ENABLE_GAME_RETRIEVAL=false`. There were 96 concurrent requests: 48 successful position queries and 48 expected game-route 404 responses. Restart, deep mounted-dataset verification, and graceful SIGTERM shutdown passed. See [`DOCKER_API_CHECK.json`](DOCKER_API_CHECK.json).

A separate internal QA run enabled game retrieval only inside a private Docker network. The API returned PGN for one Masters game and one Elite game; each body matched the corresponding `codb get-game` output byte for byte (930 bytes for the Masters sample and 908 bytes for Elite). Position queries matched the CLI for both collections, all 96 concurrent mixed position/game requests succeeded, and startup/restart reached healthy in 6.725/6.541 seconds. Peak sampled process RSS was 213,652 KiB under the 512 MiB container limit. The container used read-only named-volume copies, a read-only root filesystem, UID `10001:10001`, and no published host ports. This is internal QA evidence, not an end-user deployment or rights clearance. See [`DOCKER_API_RETRIEVAL_QA.json`](DOCKER_API_RETRIEVAL_QA.json).

Both Docker runs used a read-only root filesystem, user `10001:10001`, read-only dataset mounts, no published host ports, a 512 MiB memory limit, and a two-CPU limit. Docker-in-Docker could not resolve workspace bind paths or expose the debug port to the workspace host, so both real runs used read-only Docker-managed volumes and an in-network client. Neither run proves host bind-mount behavior. See [`DOCKER_API_CHECK.json`](DOCKER_API_CHECK.json), [`DOCKER_API_RETRIEVAL_QA.json`](DOCKER_API_RETRIEVAL_QA.json), and the exact host-side verification procedure in [`docs/DOCKER_DEPLOYMENT.md`](../../docs/DOCKER_DEPLOYMENT.md).

## Verification and remaining limits (historical r2)

`python3 -m unittest scripts/test_gigabase_eval.py scripts/test_docker_deployment.py` passed, including a Docker fixture run with the game route disabled. The final `./scripts/validate.sh` result is recorded in [`REPRODUCIBILITY.json`](REPRODUCIBILITY.json).

The publisher supplied no archive checksum or exact game count. The publisher's license statement does not clear heterogeneous upstream records; no independent event-level OTB audit was performed; publisher Elo was not independently certified as official FIDE Elo; and header/policy-rejected records were not all replayed. The committed source inventory remains a header-level inventory and has no `Termination` distribution. The source-to-canonical and source-dedupe audits use the repository parser, stable-ID function, and production deduplicator. The independent-parser samples remain external; the 300,000-window sample misses four supported source-tag quotas and is not a full-source parse. The six synthetic deduplication golden cases remain aggregate-linked regression coverage; the separate source-dedupe report supplies 16 actual anonymized duplicate/collision comparisons. The 242 same-player/date/event/round/move overlap candidates remain unresolved. The selected-position oracle is derived from canonical CGB3 records. The Docker daemon could not test host bind mounts. The evaluation package is not approved for public distribution, commercial use, or production service. The existing approved Masters and Elite releases remain the separate Lichess-derived datasets.

## Readiness conclusion (historical r2)

The full supplied PGN contains **13,450,696 observed records**. The r2 screen retained **156,898 legal, unique games** under the documented source, identity, date, rating, and play-mode gates. The remaining records are accounted for: 13,293,782 policy rejections, 2 confirmed duplicates, and 14 stable-ID collision quarantines. Source qualification is the largest rejection reason (11,462,752), followed by identity confidence (1,280,905), below-threshold ratings (495,614), and date validation (52,625); the other rejection counts are listed above. Only 943,493 records met the numeric rating and identity gates before the remaining filters, so one million is not attainable under those gates.

The 156,898 count is an r2 policy-screen count, **not an independently event-verified OTB count**. Every retained record lacks `Mode`, all rely on the publisher's archive-wide OTB claim, and publisher Elo has not been independently verified as contemporaneous FIDE Elo. No event-level OTB audit or upstream-rights clearance was completed. The candidate adds measured coverage and passed local package, rollback, API, and retrieval QA, but it is not ready to power the public or production Evalis Opening Explorer. The existing 29,640-game event-qualified Masters release and 429,150-game Elite release remain active and unchanged.
