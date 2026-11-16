# Dataset notice: {{RELEASE_ID}}

This notice ships inside the indexed package (`RELEASE/NOTICE.md`) and accompanies the filtered PGN. It describes the dataset, not the Opening Explorer software.

## Status at packaging

- Dataset version: `{{DATASET_VERSION}}`
- Distribution status when packaged: **{{DISTRIBUTION_STATUS}}**
- Upstream rights: **unresolved**. The publisher's CC BY-NC 4.0 claim is recorded, but it does not independently clear every upstream game.
- The publisher's commercial-license (B2B) page, as reviewed on 2026-11-15, describes free internal development and a closed beta, permits internal derived metadata, and prohibits end-user bulk exports and direct raw-PGN downloads. No written permission for public redistribution of this release is recorded.
- The `RELEASE/DATASET_LICENSE.txt` file inside the indexed package is the build-time record for this snapshot. It describes local internal evaluation only. Do not treat this package as cleared for redistribution unless the project publishes a later release record that says so.

## Source and attribution

Required acknowledgment:

> Masters OTB games and derived opening statistics from **Lumbra's Gigabase**, OTB Complete version 2026-11-11 (publisher claim: CC BY-NC 4.0). Curated, filtered, normalized, and indexed by the Opening Explorer API project. Not endorsed by the publisher.

- Publisher download page: https://lumbrasgigabase.com/en/download-area-en/
- Publisher sources page: https://lumbrasgigabase.com/en/sources/
- Publisher commercial-license page: https://lumbrasgigabase.com/en/commercial-licenses-en/
- License deed (CC BY-NC 4.0): https://creativecommons.org/licenses/by-nc/4.0/

The publisher says that some upstream collections have no reliable or binding license information. The project has not verified upstream rights for individual games.

## Changes made by the project

Under the CC BY-NC 4.0 "indicate changes" condition, the project made these changes to the publisher's archive:

1. **Selection.** Only games that pass the project's Masters OTB policy `{{POLICY_ID}}` are kept. The policy trusts the publisher's OTB Complete assertion, requires standard legal chess, a minimum of four plies, and publisher-supplied rating thresholds, and rejects explicit correspondence or online evidence. {{TOTAL_ENCOUNTERED}} source records were examined; {{GAME_COUNT}} were retained.
2. **Deduplication.** Confirmed duplicates and superseded prefix records are merged or removed under the project's deduplication policy `{{DEDUP_POLICY}}`. Stable-ID collisions are quarantined.
3. **Representation.** Each retained game is stored as its legal mainline in a compact binary form, with the metadata fields listed under "Preserved metadata" below. Comments, annotations, variations, and clock data are not stored.
4. **Indexing.** A position index of the first 40 plies ({{POSITIONS}} unique positions) was built from the retained games and is not a copy of the publisher's data.
5. **PGN export.** The filtered PGN re-serializes the same retained games with a fixed tag set and standard SAN. It is not a byte copy of the source PGN.
6. **Attribution sidecar.** Source, identity-label, title, and FIDE-ID values were taken from the source records and stored per game. Where merged source records disagree, the PGN omits the tag rather than choosing one value.

## Preserved metadata and its limits

- Preserved: player names, publisher-supplied Elo, date (including partial dates), event, site, round, result, ECO, time control, source URLs, and FEN for any non-standard start. Source tags are written as `Source`.
- Not preserved: comments, annotations, variations, clock data, `Termination`, `Mode` (absent from every retained game), and the publisher's identity-evidence labels.
- Elo values are the publisher's figures. They have not been verified as official FIDE ratings.
- OTB status is the publisher's archive-level assertion, not an independent event audit.

## Source tags

Publisher source categories among the retained source records (before deduplication):

{{SOURCE_TAG_TABLE}}

## Other licenses

- The software in this repository is dual-licensed MIT OR Apache-2.0. Those licenses do not apply to the dataset contents, and nothing in them grants rights to the games.
- The Elite Online collection is a separate dataset under CC0 1.0 and is not part of this release.
- This project makes no claim of authorship of any game. The games remain the work of their players, organizers, and publishers.

## Release facts

- Built: {{BUILT_AT}}
- Games: {{GAME_COUNT}}
- Index depth: {{DEPTH}} plies; unique positions {{POSITIONS}}; position/move pairs {{POSITION_MOVE_PAIRS}}
- Publisher version: {{PUBLISHER_VERSION}}
