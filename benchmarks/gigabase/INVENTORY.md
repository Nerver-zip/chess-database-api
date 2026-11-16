# Gigabase OTB Archive Inventory

## Scope and method

This report describes the user-supplied `OTB_DATABASE.7z` archive and its single PGN member. The archive was not downloaded again. The local archive SHA-256 is `4e4209ccc1870e7fede557aae09b4dd40b721d4697db9b4c272ff0e4fbbdb500`; the expanded PGN SHA-256 is `87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759`. Both hashes are locally computed. The publisher supplies no checksum for this archive.

The PGN was read with the repository's streaming `codb inventory` command. It hashes the input, parses record boundaries and headers, and aggregates distributions in a disk-backed SQLite file. It does not replay SAN moves or establish move legality. The complete machine-readable output is [INVENTORY.json](INVENTORY.json). Structural parsing encountered 13,450,696 records, parsed all 13,450,696, recovered from zero syntax errors, and reached clean EOF. This is an observed PGN record count, not a count of legal or eligible games. A second full extraction with `py7zr 1.1.4` reproduced the locally pinned archive and member hashes; see [ARCHIVE_RECHECK.json](ARCHIVE_RECHECK.json). The archive has no publisher checksum and no packed-stream CRC metadata.

The publisher's current pages describe the 2026-11-11 OTB Complete release as all OTB PGN files (excluding LichessBroadcastDB) and say the Gigabase contains over 13 million games. The observed record count is consistent with that rounded claim. No exact publisher game count is stated. [Publisher download page](https://lumbrasgigabase.com/en/download-area-en/) · [Publisher overview](https://lumbrasgigabase.com/en/)

## Source categories

Every structurally parsed record has a `Source` tag. Counts reconcile exactly to 13,450,696. The publisher's source page assigns these tags to the categories below. The rightmost column is only a candidate intersection: the repository's existing rating threshold applied to the PGN `WhiteElo`/`BlackElo` fields, and both players marked `Strong` or `Confirmed` in `IdentityEvidenceWhite` / `IdentityEvidenceBlack`. It is not a legality, provenance, license, or acceptance count.

| Publisher category | Records | Rating + identity candidates | Publisher's source-level rights description |
| --- | ---: | ---: | --- |
| Legacy Software Dump | 4,922,559 | 548,988 | Former commercial/semi-commercial software downloads; original source terms are not individually enumerated. |
| Independent Collection | 3,882,200 | 117,252 | Abandoned collections; publisher says reliable/binding original license information is often unavailable. |
| Historical Archive | 2,585,591 | 89,735 | Abandoned historical collections; publisher says reliable/binding original license information is often unavailable. |
| AnaSchaigorodsky | 675,531 | 24,410 | Publisher identifies the academic source as CC BY 4.0. |
| LichessBroadcastAPI | 667,945 | 120,991 | Publisher says it is retrieved via the Lichess API and reports that Lichess said this API data has no specific restrictive license. The Gigabase release itself is still presented under CC BY-NC 4.0. |
| Official federation tags (9) | 644,468 | 40,146 | Publisher describes game notation as factual and says it is practically treated as public domain. This is the publisher's legal position, not an independent legal opinion. |
| Community Project | 69,080 | 1,514 | Publisher describes the group as free-licensed or openly downloaded, but does not identify the upstream source/license for each record in this export. |
| VargaCsabaZoltan | 3,307 | 457 | Publisher describes player-provided PGN packages shared for distribution with the Gigabase; a record-level permission statement is not included. |
| LEDB | 15 | 0 | Publisher identifies LEDB as the Lichess Elite Database and says that source is CC0. Its play mode is not established by this tag alone. |
| **Total** | **13,450,696** | **943,493** | |

The 185,547 candidate records in the academic, federation, and LichessBroadcastAPI groups are the subset of the 943,493 intersection whose source categories have a specific publisher description of rights or source status. This narrows review; it does not clear commercial/public use. The three publisher-identified categories with expressly unclear original terms contribute 11,390,350 records, 84.7% of the archive. Community and player-provided groups also need upstream permission detail before they are treated as independently cleared.

Publisher references: [Sources & Transparency](https://lumbrasgigabase.com/en/sources/), [download and PGN notes](https://lumbrasgigabase.com/en/download-in-pgn-format-en/), and [B2B and licensing terms](https://lumbrasgigabase.com/en/commercial-licenses-en/).

## Metadata coverage and observed values

| Field | Records with tag | Observation |
| --- | ---: | --- |
| `Source`, `Date`, `Event`, `Site`, `White`, `Black`, `Result` | 13,450,696 each | Tags are present on every parsed record; placeholders still occur in Event, Site, and player names. |
| `WhiteElo` | 10,955,043 | 2,495,653 records have no parseable value. |
| `BlackElo` | 10,932,489 | 2,518,207 records have no parseable value. |
| `WhiteFideId` / `BlackFideId` | 6,093,237 / 6,089,313 | These identifiers are present on fewer than half of records. Presence alone does not validate the Elo value. |
| `WhiteTitle` / `BlackTitle` | 2,787,530 / 2,762,314 | Values include recognized chess titles as well as `Not yet awarded` and assorted other strings; they need normalization before policy use. |
| `IdentityEvidenceWhite` / `IdentityEvidenceBlack` | 13,450,696 each | Values are `Confirmed`, `Strong`, `Probable`, `Ambiguous`, `Conflict`, and `NoMatch`. Both sides are Strong/Confirmed in 4,195,910 records. |
| `Mode` | 1,405,063 | All observed values are `Correspondence`. These records are not treated as OTB candidates. No `Mode` tag is present on the other records. |
| `FEN` / `Variant` | 0 / 0 | No such headers were observed. Move replay is still required to verify standard chess and legality. |

`Event` is a placeholder in 304,945 records (2.27%); `Site` is a placeholder in 939,408 (6.98%). All player-name tags are present; 26 Black names and 23 White names are placeholders. Thus tag presence overstates usable event and venue coverage.

No `FEN`, `Variant`, or `Termination` header was observed in the structural inventory. The accepted CPI build also reports zero custom-FEN games. A termination distribution is unavailable because the source records do not carry a `Termination` tag; result headers do not identify why a game ended. The structural inventory itself contains no record-level examples, and no malformed PGN example was observed by its header parser. The completed r3 source-to-canonical audit matched all 3,284,429 retained games and records zero metadata, attribution, move, starting-position, or final-position mismatches; it emits anonymized stratified examples and suppresses strata below 50 games. The r3 source-derived deduplication audit compares all 12,366 duplicate, supersession, and collision outcomes to canonical retrieval, emits 50 anonymized examples, and reports strict move-prefix and board-position counts separately. Both reports omit game IDs, player/event/site values, and raw PGN. See [FINAL_REPORT.md](FINAL_REPORT.md), [SOURCE_CANONICAL_AUDIT.json](SOURCE_CANONICAL_AUDIT.json), and [SOURCE_DEDUPE_AUDIT.json](SOURCE_DEDUPE_AUDIT.json). The absent `Termination` tag and lack of an observed malformed example remain evidence limits; no malformed row is fabricated.

Dates span 1475 through 2026. There are 51,300 partial or unknown dates. The largest decades are 2010–2019 (4,547,980), 2000–2009 (3,257,707), and 2020–2029 (2,614,931). Result values are 5,243,294 White wins, 4,279,610 Black wins, and 3,927,792 draws; no unknown result was observed. Historical r2 rejected 52,625 records reaching its date gate because their date was absent, partial, or invalid. Current r3 preserves valid partial and unknown date components and rejects invalid supplied date components.

Applying the existing Masters numeric thresholds to the two PGN Elo fields yields 3,542,307 records: 3,218,362 meet both `>=2200`; 2,510,706 meet the `>=2400` / other `>=2000` rule (overlapping count); the union is 3,542,307. At least one rating is missing or unparseable in 3,234,119 records. These figures do not identify a rating system.

The publisher says current game Elo values are intended to reflect ratings at the game date, while also describing use of a historical Elo reference file to resolve missing rating data. Its documentation acknowledges that automatic identity, FIDE ID, and Elo assignments can be wrong. Therefore the observed fields are reported as publisher-supplied Elo; this inventory does not independently certify every value as an official FIDE rating at the event date. See the [publisher's PGN notes and version history](https://lumbrasgigabase.com/en/download-in-pgn-format-en/).

`SourceQuality` values are `-1` (3,307), `1` (3,137,428), `2` (3,085,261), `3` (5,164,143), `4` (80,060), and `5` (1,980,497). The publisher documents `-1` as verified player corrections. The meanings and ordering of `1` through `5` were not found in the publisher documentation and are not inferred here.

## Qualification implications

The publisher calls this file OTB Complete and says it separates OTB and online exports. The observed `Mode "Correspondence"` tag on 1,405,063 records is explicit non-face-to-face play and must be excluded from an over-the-board collection. The other records have no `Mode` tag; their play mode is supported only by the publisher's archive-level OTB assertion and source tag, not an independent event audit. `IdentityEvidence` describes player identity confidence, not event play mode.

The provisional 185,547 intersection uses historical r2 strength and identity gates and only source groups with a specific publisher description of upstream rights/source status. It is not a measure of current r3 eligibility, legality, or license coverage. The full r3 header scan counted 3,296,795 candidates under the publisher-trust policy; these remain before move replay, deduplication, and final retention. The r3 production build retained 3,284,429 unique games from the complete file and passed deep verification. It is not in the approved catalog or production release. Current and historical build accounting and limitations are in [FINAL_REPORT.md](FINAL_REPORT.md).

The aggregate Gigabase license is CC BY-NC 4.0. The publisher's licensing page describes its no-cost offer as internal development/closed beta subject to stated limits and requires separate terms for a live user-facing or commercial integration. The source's non-commercial terms and upstream uncertainties prevent a claim of public Evalis production clearance. A locally served Docker evaluation may be useful under the publisher's internal-development terms, but it does not authorize public deployment.

## Machine evidence

- [Complete structural/header inventory](INVENTORY.json): input hash/size, parser accounting, tag counts, source and identity distributions, rating bins and thresholds, dates, aggregate metadata coverage, and limitations. Record-level examples are omitted from this inventory report; [`SOURCE_CANONICAL_AUDIT.json`](SOURCE_CANONICAL_AUDIT.json) and [`SOURCE_DEDUPE_AUDIT.json`](SOURCE_DEDUPE_AUDIT.json) contain anonymized source-derived comparisons. No malformed example was observed by the structural parser.
- [Archive and member inventory](archive_inventory.json): archive integrity evidence, member hashes, publisher metadata, and links to this parser report.
- [Independent PGN parser sample](INDEPENDENT_PGN_AUDIT_TARGETED.json): 300,000 byte windows yielded 227 unique candidate games, all exact across metadata, moves, boards, legality, and attribution. Four supported source-tag quotas were missed; the report records the categories, external parser provenance, and measured 339,388 KiB RSS. It is a sample, not a full-source Python parse.

The inventory command's fixture tests and release build passed in the pinned local validation container. The full PGN scan completed in 3m39s with a sampled resident set below 7 MiB. This is the inventory process only; it is not the corpus build RSS or build-time measurement.

## Current r3 local evaluation

The current full source-qualified evaluation is recorded in [FINAL_REPORT.md](FINAL_REPORT.md), with machine evidence in [R3_REPRODUCIBILITY.json](R3_REPRODUCIBILITY.json), [archive_inventory.json](archive_inventory.json), and the policy impact in [POLICY_IMPACT.json](POLICY_IMPACT.json). The r2 reproduction record and [quality summary](QUALITY_REPORT.json) are historical; current r3 quality counts are in [QUALITY_REPORT_R3.json](QUALITY_REPORT_R3.json).

The current r3 policy retained 3,284,429 unique games from 13,450,696 encountered records. Accounting reconciles across 4,018 confirmed duplicates, 506 superseded prior records, 10,153,901 policy rejections, and 7,842 stable-ID collision quarantines; retained games equal accepted unique games. Retained records passed legal-move ingestion and the candidate passed deep verification. Policy-rejected records were not all replayed for legality. R3 accepts any non-placeholder `Source`, preserves source and publisher identity labels, accepts valid partial or unknown dates, and uses symmetric rating/title strength rules. Missing `Mode` relies on the publisher's archive-level OTB Complete assertion; explicit contradictions are rejected. Publisher-supplied Elo and GM labels are not independently verified, and no event-level OTB audit was performed.

The selected-position CPI oracle replayed all retained games to the 40-ply index depth and matched direct CPI lookups for 18 lines / 17 positions, with no mismatch. It shares the production position-key implementation and starts from CGB3, so source mapping is covered separately by the completed full-source audits. The source-to-canonical audit matched every retained game and reconciled the complete attribution sidecar. The source-dedupe audit compares all duplicate, supersession, and collision outcomes; of 162 strict source-prefix cases, 161 have a different final board and one has the same final board. Both reuse the production parser and deduplicator, so they are traceability checks rather than independent algorithm validation. The earlier r2 audit reports are preserved under the `_R2` filenames.

This is a local evaluation, not an approved corpus release. The source has no publisher checksum, its publisher-stated CC BY-NC 4.0 terms do not resolve heterogeneous upstream rights, and separate rights review is required before redistribution or public service. The final quality-corrected package passed checksum, extraction, deep verification, offline query, isolated bootstrap, and rollback; internal Docker API QA passed for the same indexed build. The existing Lichess-derived Masters and Elite releases remain unchanged. Docker QA used read-only named-volume copies and does not establish host bind-mount behavior or authorize end-user retrieval.

## Historical r2 evaluation

The superseded r2 qualification retained 156,898 games under a source allowlist, Strong/Confirmed identity evidence, complete-date gate, and pre-1970 cutoff. Its accounting, package, bootstrap, rollback, and Docker evidence are preserved in the r2 section of [FINAL_REPORT.md](FINAL_REPORT.md). R2 is historical and does not describe the current candidate.
