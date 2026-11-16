---
summary: "Masters OTB dataset products: indexed package and filtered PGN, asset naming, reconstruction, verification, installation, versioning, licensing gate, and maintainer publication procedure."
read_when: "When downloading, verifying, reconstructing, or installing Masters OTB dataset assets, or when preparing, uploading, or publishing a dataset release."
status: current
---

# Dataset releases

This document covers the two distributed products for the Masters OTB corpus, how their assets are named and verified, and how they are published. The software is described in [README.md](../README.md) and the format contracts in [DATA_CONTRACTS.md](DATA_CONTRACTS.md). Licensing facts are in [LICENSING.md](LICENSING.md).

**Status.** `masters-otb-r3` is published as the GitHub release [`dataset-masters-otb-r3`](https://github.com/Nerver-zip/opening-explorer-api/releases/tag/dataset-masters-otb-r3) (2026-11-16). It contains the indexed package (two parts), the filtered PGN, the dataset notice, the release manifest, and `SHA256SUMS`, about 3.2 GB in total. Distribution rests on the maintainer's recorded decision described in section 6.

## 1. Products

Both products contain the same 3,284,429 canonical games. No unfiltered source archive is distributed.

| Product | Asset | Contents |
| --- | --- | --- |
| Indexed dataset | `masters-otb-r3-indexed.tar` (parts) | CGB3 game shards, `games.idx`, CPI v2 `positions.cpi` (depth 40), source-attribution sidecar, and the release record (`RELEASE/`, including the dataset notice). Installs directly into a dataset root. |
| Filtered PGN | `masters-otb-r3-filtered.pgn.zst` (parts if needed) | One PGN game per canonical game, Zstandard-compressed. See section 5. |

## 2. Versioning

- **Software versions** follow the Cargo workspace version and `vX.Y.Z` tags. They are independent of dataset versions.
- **Dataset release identifier** is `masters-otb-r<N>`. Its GitHub tag is `dataset-masters-otb-r<N>`, for example `dataset-masters-otb-r3`.
- **Immutable build identifier** (`codb-masters-otb-…-cpi-d40`) is the snapshot directory name and the `dataset_version` in the manifest. It is never rewritten.
- **Format versions** are recorded in the manifest (`CGB3/v1` shards, `CPI/v2` index). A software release must state which format versions it reads.
- **Tags are immutable.** Create a dataset tag only on a reviewed commit on `main`, protect it with a repository tag ruleset, and never move or reuse it.

## 3. Asset naming

| Name | Meaning |
| --- | --- |
| `<release>-indexed.tar` | Indexed package, when it fits in one asset (below 1900 MiB) |
| `<release>-filtered.pgn.zst` | Filtered PGN, when it fits in one asset |
| `<name>.part-001`, `.part-002`, … | Numbered parts of a product larger than 1900 MiB (1,992,294,400 bytes). GitHub limits each asset to 2 GiB. |
| `<release>-NOTICE.md` | Dataset notice (also inside the indexed package at `versions/<build>/RELEASE/NOTICE.md`) |
| `<release>-RELEASE_MANIFEST.json` | Release manifest; byte-identical to `releases/<release>.json` in Git |
| `<release>-SHA256SUMS.txt` | SHA-256 of each uploaded part and of each reconstructed product |

Parts are cut at fixed multiples of the part size, so the split is deterministic. The reconstructed product is the concatenation of its parts in numeric order.

## 4. Download, verify, reconstruct, install

The repository and release are public, so no authentication is needed beyond the GitHub CLI (`gh`). Requirements: Python 3.11 or later and about 3.2 GB of free disk for the download, plus about 2.7 GB more for the reconstructed package and the installed snapshot. Run the commands from a checkout of the repository at the release tag.

To fetch only the PGN, run `gh release download dataset-masters-otb-r3 --pattern 'masters-otb-r3-filtered.pgn.zst'` and compare it with the `sha256sum` listed in `masters-otb-r3-SHA256SUMS.txt`.

```bash
# 1. Download the published assets
gh release download dataset-masters-otb-r3 --dir dist/

# 2. Verify every part and every reconstructed product against the committed manifest
python3 scripts/release_assets.py verify \
  --manifest releases/masters-otb-r3.json --dist-dir dist/ --committed

# 3. Reconstruct the whole products (verified before and after concatenation)
python3 scripts/release_assets.py reconstruct \
  --manifest releases/masters-otb-r3.json --dist-dir dist/ --output-dir reconstructed/

# 4. Install the indexed package into a dataset root (never into a live root in use)
cargo build --release -p opening-explorer-cli
python3 scripts/release_assets.py install \
  --manifest releases/masters-otb-r3.json \
  --package reconstructed/masters-otb-r3-indexed.tar \
  --target /srv/opening-explorer/masters-otb \
  --codb target/release/opening-explorer

# 5. Check the installed snapshot and query it
target/release/opening-explorer verify --dataset /srv/opening-explorer/masters-otb --deep
target/release/opening-explorer query --dataset /srv/opening-explorer/masters-otb --moves "e2e4" --json
```

`install` extracts the package into a staging directory, checks its release record, runs `opening-explorer verify --deep`, publishes it atomically with `opening-explorer publish`, and deep-verifies the target. A failed step leaves the previously active snapshot unchanged.

**Rollback.** Earlier versions are kept under `versions/`. To reactivate one, run `opening-explorer publish --candidate <versions/build-id> --target <root> --collection masters-otb`, then verify and restart the service.

**The filtered PGN** is a standard Zstandard stream: `zstd -dc masters-otb-r3-filtered.pgn.zst | head`, or any Zstandard-capable reader.

## 5. Filtered PGN

- One game per canonical stored game, in physical storage order (shard, block, slot). Output is deterministic for a given snapshot.
- **Written tags**: `Event`, `Site`, `Date`, `Round`, `White`, `Black`, `Result`, `WhiteElo`, `BlackElo`, `ECO`, `TimeControl`, `PlyCount`, `GameURL` (source game URL), `BroadcastURL`, `SetUp`/`FEN` for a non-standard start. Missing values use the PGN placeholders `?` and `????.??.??`.
- **Sidecar-derived tags** come from `source-attribution.jsonl`, joined by stable game ID:
  - `Source` lists every distinct publisher source tag attributed to the game, sorted and joined with `; `. A game can list more than one because duplicate source records were merged into it. The sidecar is project curation data. Its value for a game can differ from the `Source` tag of the single source record whose other tags were kept (see [benchmarks/gigabase/FINAL_REPORT.md](../benchmarks/gigabase/FINAL_REPORT.md)).
  - `WhiteTitle`, `BlackTitle`, `WhiteFideId`, `BlackFideId`, and `Mode` are written only when every attribution agrees on one value. Otherwise the tag is omitted and counted in the export report. `Mode` is absent from every game in this release.
- **Movetext** is the complete legal mainline in SAN with check and mate markers, wrapped before column 80. The final token equals the `Result` tag.
- **Not reconstructable** from stored data: comments, NAGs, variations, clock annotations, `Termination`, identity-evidence labels, and source-quality labels. None are written.
- Tag values escape `"` and `\`. A control character in a value fails the export rather than producing an invalid file.

The exporter is `opening-explorer export-pgn --dataset <snapshot> --output <file>[.zst] [--report <json>]`. It refuses to overwrite an output file and writes through a `.partial` file that is renamed only after a complete, checksummed stream.

## 6. Licensing and the distribution gate

- The software (MIT OR Apache-2.0) does not license the dataset. The dataset carries the publisher's CC BY-NC 4.0 claim, and upstream rights are unresolved. See [LICENSING.md](LICENSING.md) section 4.4.
- The attribution notice is [dataset/ATTRIBUTION.md](../dataset/ATTRIBUTION.md). It is rendered into `<release>-NOTICE.md` and into the indexed package. It states the distribution status at packaging time.
- [releases/distribution_clearance.json](../releases/distribution_clearance.json) is the gate. `upload --execute` and the publish workflow refuse unless `status` is `cleared`, `clearance_reference` names the written basis, and `release_id` matches.
- The canonical statement of current policy, including public API deployment, is [LICENSING.md section 6](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment); this section covers only the release gate.
- **Current status: `cleared` (maintainer decision, 2026-11-16).** The record states that the maintainer relies on the publisher's public CC BY-NC 4.0 grant for non-commercial GitHub Release redistribution. It is the maintainer's interpretation, not separate written permission from the publisher. The recorded conflicts stay disclosed in that file. They include the snapshot's own release notice (local evaluation only, do not redistribute), the publisher's terms as reviewed (end-user bulk and raw-PGN downloads prohibited), and the absence of a publisher checksum.
- **Notice wording in `masters-otb-r3`.** The release assets are immutable and checksummed, so they keep their build-time text. The `RELEASE/DATASET_LICENSE.txt` inside the indexed package describes local evaluation only, and `masters-otb-r3-NOTICE.md` records the status "not_cleared" as of packaging. Both predate the maintainer's decision. Read them together with `releases/distribution_clearance.json` and the release notes, which state the current position. A future release should render a fresh notice at build time.
- The release notes of `dataset-masters-otb-r3` are editable and were written to reflect the current status; the asset files were not changed.

## 7. Maintainer procedure

1. **Prepare** from the active snapshot: `python3 scripts/release_assets.py prepare --codb target/release/opening-explorer`. This verifies the snapshot, builds the indexed package with the corpus release-package member rules, runs `export-pgn`, splits both products, and writes the manifest to `data/releases-dist/<release>/` and `releases/<release>.json`. Prepare never rebuilds the corpus.
2. **Verify** the dist directory: `python3 scripts/release_assets.py verify --manifest releases/<release>.json --dist-dir data/releases-dist/<release> --committed`. Run the independent PGN checker in the scratch environment (see section 8).
3. **Commit** `releases/<release>.json` and any evidence on a branch, and merge it through review.
4. **Tag** the reviewed merge commit: `git tag -a dataset-<release> <commit>`, then push the tag.
5. **Dry run** the upload: `python3 scripts/release_assets.py upload --manifest releases/<release>.json --dist-dir data/releases-dist/<release>`. It prints the plan and every blocker and uploads nothing.
6. **Upload to a draft** only after the gate is cleared: add `--execute`. This creates a draft release and never publishes it.
7. **Publish** with the `Publish dataset release` workflow (manual, `workflow_dispatch`). It requires the tag to be reachable from `main`, the committed manifest and gate to pass, the draft to match the manifest by name and size, every downloaded byte to match its SHA-256, and the uploaded manifest to equal the committed one. Only then does it run `gh release edit --draft=false --latest=false`.

The workflow never rebuilds data. It asks GitHub not to mark the dataset release as "Latest" so that it does not displace software releases. GitHub still reports a release as latest when it is the repository's only published release (this is the case for `dataset-masters-otb-r3` until a software release exists); the flag then stops mattering once a software release is published.

## 8. Verification evidence

| Evidence | Produced by | Covers |
| --- | --- | --- |
| `benchmarks/gigabase/PGN_EXPORT_VERIFICATION_R3.json` | `scripts/release_pgn_verify.py` (scratch venv: python-chess, zstandard) | Headers, sidecar tags, `Result`, `PlyCount`, and SAN token count of every game against a direct decode of the CGB3 shards; full python-chess replay of every 16th game (205,277 of 3,284,429), comparing the replayed moves; probe positions compared with `opening-explorer query` |
| `benchmarks/gigabase/PGN_REIMPORT_LEGALITY_R3.json` | `opening-explorer build` over the whole filtered PGN | All 3,284,429 games reimported through the project's own ingest (shakmaty, not an independent parser): 0 illegal, 0 malformed, 0 variant rejections. Its policy and duplicate counts come from the default Masters policy and are not findings about the export |
| `benchmarks/gigabase/PGN_EXPORT_R3_REPORT.json` | `opening-explorer export-pgn --report` | Exact counts, omitted tags, and the sidecar hash used |
| `benchmarks/gigabase/INSTALL_VERIFICATION_R3.json` | Reconstruction, installation, rollback, and API run on the prepared assets | Reconstructed hashes, install into a scratch root, deep verification, rollback, and API responses. The published asset digests reported by GitHub equal the manifest checksums |

The offline test suites are `scripts/test_release_assets.py` and `scripts/test_release_pgn_verify.py`. They run in `./scripts/validate.sh`. The network and scratch-environment checks are not part of the offline gate.
