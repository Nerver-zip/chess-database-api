---
summary: "Build, verify, upgrade and operate derived game-search indexes."
read_when: "Enriching a local canonical snapshot, reviewing index format changes, or reproducing search measurements."
status: current
---

# Search indexes

Search indexes derive from one immutable collection snapshot. CGB3 holds canonical games, `games.idx` resolves public game IDs, and CPI v2 serves unfiltered Opening Explorer statistics. Game search reads a SQLite metadata projection and a memory-mapped position-to-game occurrence index. The schema stores game summaries and source attribution, with no full PGNs or move sequences.

## Build and verify

Install the indexed release package into an isolated, versioned dataset root using the [release installer](DATASET_RELEASES.md). Build the CLI and enrich the selected active version.

```bash
cargo build --locked --release --package opening-explorer-cli
target/release/chess-database build-search-index \
  --dataset data/releases/elite-online --memory-limit-mib 128
target/release/chess-database verify-search-index \
  --dataset data/releases/elite-online
```

The default depth follows the source CPI depth when present. For snapshots without CPI, it is 40 plies for Masters OTB and 30 for Elite Online. Pass `--depth-plies 80` for expanded coverage or `--full` for all recorded positions, including the final board. Use `--temp-dir` to place external-sort runs, postings assembly spools and SQLite scratch files on an existing filesystem outside the snapshot versions. The sort-record buffer, merge-reader buffers, SQLite page cache, and kernel page cache are separate. The build report gives the configured sort-buffer size, peak temporary run bytes, posting assembly spool bytes and final artifact sizes. Measure total RSS and peak disk usage on the build host.

The builder streams canonical games in shard, block and slot order. It maintains a dense, zero-based ordinal for each game and projects the stable unsigned 64-bit ID as an eight-byte, big-endian SQLite BLOB. It then imports source-attribution records by stable ID. The builder sorts position records in bounded runs, merges them in a finite fan-in, and prepares a staged version under `versions/.staging_search_*`. Deep verification checks the catalog, file checksums, canonical ordinal mapping, full posting count, structure and declared depth. The top-level manifest records search artifact bytes as part of `total_physical_bytes`.

The dataset lock serializes builds, publication and recovery. A successful build fsyncs stage contents, renames the version, and atomically switches `current`. Copies or immutable hard links preserve CGB3, `games.idx`, CPI and any source-attribution sidecar byte-for-byte. Previous versions remain available. Releases' original notices and records are copied into `source-release/` for local derivatives. Build failures before activation leave the running snapshot intact. The storage library's `recover_dataset` function removes abandoned staging directories while holding the dataset lock. A killed process may also leave a private `search-runs-*` directory under a custom temporary root.

Use the normal [reload and rollback procedure](DOCKER_DEPLOYMENT.md#safe-dataset-updates) after publication. A server can switch between legacy and enriched snapshots. Search capability depends on both dataset contents and operator settings. Rebuilding CPI preserves a verified search index from the same canonical source. Filtered Explorer requires search coverage at least as deep as the current CPI.

## Search format version 1

The additive `search_index` manifest entry records format version, source snapshot identity, canonical artifact fingerprint, coverage, game, position and occurrence counts, sort-buffer configuration, file sizes and SHA-256 digests. The fingerprint includes collection, game count, lookup checksum, ordered shard IDs and checksums, and the optional source-attribution checksum. CPI has its own identity and checksum. The same game source can support multiple search and CPI depths.

Each enriched snapshot has two files under `search/`:

- `metadata.sqlite` contains the versioned game projection, player and event catalogs, query indexes and preserved source-attribution records. Player and event IDs are positive and snapshot-local. See [Search API](SEARCH_API.md) for identity and grouping semantics.
- `positions.postings` contains a versioned header, mapped position directory, occurrence streams and seek checkpoints. Position keys are the existing 128-bit canonical keys. Occurrences are ordered by key, game ordinal and ply. Within a position group the file uses a variable-length game ordinal gap and a variable-length ply, followed by a two-byte packed continuation. The reserved continuation value `0xffff` records the final board without a continuation. Checkpoints every 256 occurrences permit bounded seeks in long groups.

The postings file starts with a 64-byte header. Numeric fields in the header, directory, continuations, and checkpoint table are big-endian.

| Header byte offset | Width | Field |
| ---: | ---: | --- |
| 0 | 8 | ASCII magic `CHSPST01` |
| 8 | 4 | Format version, currently 1 |
| 12 | 4 | Header size, 64 |
| 16 | 8 | Number of distinct position keys |
| 24 | 8 | Number of position occurrences |
| 32 | 8 | Number of canonical games |
| 40 | 8 | Directory start |
| 48 | 8 | Checkpoint table start |
| 56 | 8 | File size in bytes |

Directory records are a 16-byte position key and an 8-byte absolute byte offset. Each position stream ends at the next position offset or the checkpoint table. The stream uses unsigned base-128 varints for the ordinal and ply. A new game uses its ordinal gap and absolute ply. A repeated occurrence uses zero ordinal gap and a positive ply difference. The first occurrence and every 256th subsequent record use absolute ordinal and ply. Each checkpoint entry has an eight-byte directory record index, four-byte zero-based occurrence index within that position, four-byte ordinal, and eight-byte absolute offset. The first occurrence can be addressed by the directory alone. The reader also accepts an explicit checkpoint at that first occurrence.

A search position is indexed for plies `0..=max_ply` in bounded mode. A recorded continuation at the boundary is retained. A game contributes each occurrence it actually reached, including repeats and games with result `*`. The discovery API returns one result per game, with the earliest matching occurrence. Filtered Explorer applies the CPI first-visit policy, requires a continuation before the CPI depth and excludes unknown results.

An artifact set is accepted only with the corresponding manifest and matching canonical fingerprint. Copying search files into a running or published version bypasses staging and verification. Users of unchanged release packages can build the derived indexes locally from installed canonical files. A companion bundle can be built and installed locally without transferring CGB3 or CPI again.

```bash
target/release/chess-database pack-search-index \
  --dataset data/releases/elite-online --output data/elite-search-bundle
target/release/chess-database install-search-index \
  --dataset data/compatible/elite-online --bundle data/elite-search-bundle
```

The bundle directory contains `search-index.json` and the two `search/` artifacts. Transfer the directory without changing its file contents. Installation verifies the exact canonical fingerprint, bundle checksums, schema and references before activating a new snapshot. Existing dataset release assets remain unchanged. Review dataset terms before distributing a companion bundle.

## Measurement

See the [search index experiments](research/search-index-experiments.md) for the sample layout comparison and reproducible legacy performance gate. Use each production collection separately to measure index construction, verification, search latency and mixed traffic. Full coverage increases the number of predominantly rare position keys and can make the position directory substantially larger.
