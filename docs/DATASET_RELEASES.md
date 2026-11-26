---
summary: "Masters OTB and Elite Online release products: indexed packages, filtered PGNs, checksum verification, installation, collection-specific licensing, and publication."
read_when: "When downloading, verifying, reconstructing, or installing a dataset release, or preparing, uploading, or publishing one."
status: current
---

# Dataset releases

This document covers the two products for each collection, how release assets are named and verified, and how the reviewed publication workflow works. The collections remain separate and use different source data and licenses. The software is described in [README.md](../README.md), format contracts are in [DATA_CONTRACTS.md](DATA_CONTRACTS.md), and licensing facts are in [LICENSING.md](LICENSING.md).

**Status.** [Masters OTB R3](https://github.com/Nerver-zip/chess-database-api/releases/tag/dataset-masters-otb-r3) and [Elite Online R1](https://github.com/Nerver-zip/chess-database-api/releases/tag/dataset-elite-online-r1) are available as separate releases. The Elite baseline is 429,150 games and 6,136,307 positions at CPI depth 30. The release manifests record build identities, product sizes and checksums. The packages can be [locally enriched with search indexes](SEARCH_INDEX.md).

## 1. Products

Each collection's products contain the same canonical games as one another. No unfiltered source archive is distributed.

| Collection | Indexed dataset | Filtered PGN | License and coverage |
| --- | --- | --- | --- |
| Masters OTB R3 | `masters-otb-r3-indexed.tar` (two parts); 3,284,429 games, CPI depth 40 | `masters-otb-r3-filtered.pgn.zst`; 3,284,429 games | Gigabase source; publisher claim CC BY-NC 4.0. Non-commercial terms and attribution are described in section 6. |
| Elite Online R1 | `elite-online-r1-indexed.tar`; 429,150 games, 6,136,307 positions, CPI depth 30 | `elite-online-r1-filtered.pgn.zst`; 429,150 games | Lichess standard rated archives from selected months in 2015–2017; CC0 1.0. See [LICENSING.md](LICENSING.md#41-elite-online-artifacts-cc0-10-universal). |

Both indexed products contain CGB3 game shards, `games.idx`, CPI v2 `positions.cpi`, an immutable build snapshot, its release record, and integrity checksums. The Masters package also includes its existing source-attribution sidecar. Elite has no such sidecar; release files do not invent one. Each package installs through the standard dataset installation workflow.

## 2. Versioning

- **Software versions** follow the Cargo workspace version and `vX.Y.Z` tags. They are independent of dataset versions.
- **Dataset release identifiers** are collection-scoped, such as `masters-otb-r3` and `elite-online-r1`. Each GitHub tag is `dataset-<release-id>`.
- **Search companion releases** are separately versioned derived indexes tied to a canonical fingerprint and coverage. They augment a compatible installed dataset without replacing its canonical release or requiring the original corpus to be downloaded again. The [Masters D40](https://github.com/Nerver-zip/chess-database-api/releases/tag/search-masters-otb-r3-d40-v1) and [Elite D30](https://github.com/Nerver-zip/chess-database-api/releases/tag/search-elite-online-r1-d30-v1) companions are published.
- **Immutable build identifier** is the snapshot directory name and the `dataset_version` in the manifest (for example `codb-masters-otb-…-cpi-d40` or `codb-elite-online-…-cpi-d30`). A release manifest records the actual identifier; it is never forced to match a prior build.
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

The repository and published releases are public, so no authentication is needed beyond the GitHub CLI (`gh`). Requirements depend on the selected product and are stated in its release manifest. Run the commands from a checkout of the repository at the corresponding release tag. For Elite Online R1, replace `masters-otb-r3` below with `elite-online-r1` and install into a separate `elite-online` dataset root.

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
cargo build --release -p chess-database-cli
python3 scripts/release_assets.py install \
  --manifest releases/masters-otb-r3.json \
  --package reconstructed/masters-otb-r3-indexed.tar \
  --target /srv/opening-explorer/masters-otb \
  --codb target/release/chess-database

# 5. Check the installed snapshot and query it
target/release/chess-database verify --dataset /srv/opening-explorer/masters-otb --deep
target/release/chess-database query --dataset /srv/opening-explorer/masters-otb --moves "e2e4" --json
```

For Elite Online R1, use `releases/elite-online-r1.json`, the reconstructed `elite-online-r1-indexed.tar`, and a separate target such as `/srv/opening-explorer/elite-online`. The installer verifies the collection identity and CPI depth from the package manifest. Query an opening with `target/release/chess-database query --dataset /srv/opening-explorer/elite-online --moves "e2e4" --json`.

The complete Elite download and installation sequence is:

```bash
gh release download dataset-elite-online-r1 --dir elite-dist/
python3 scripts/release_assets.py verify \
  --manifest releases/elite-online-r1.json \
  --dist-dir elite-dist/ --committed
python3 scripts/release_assets.py reconstruct \
  --manifest releases/elite-online-r1.json \
  --dist-dir elite-dist/ --output-dir elite-reconstructed/
cargo build --release -p chess-database-cli
python3 scripts/release_assets.py install \
  --manifest releases/elite-online-r1.json \
  --package elite-reconstructed/elite-online-r1-indexed.tar \
  --target /srv/opening-explorer/elite-online \
  --codb target/release/chess-database
target/release/chess-database verify \
  --dataset /srv/opening-explorer/elite-online --deep
target/release/chess-database query \
  --dataset /srv/opening-explorer/elite-online --moves "e2e4" --json
zstd -dc elite-dist/elite-online-r1-filtered.pgn.zst | head
```

After publication, download the individual products from the same release. `elite-online-r1-SHA256SUMS.txt` lists their SHA-256 digests; the `verify` command also checks every uploaded part and reconstructed product against the committed manifest.

`install` extracts the package into a staging directory, checks its release record, runs `chess-database verify --deep`, publishes it atomically with `chess-database publish`, and deep-verifies the target. A failed step leaves the previously active snapshot unchanged.

### Prepared search companions

The [Elite Online R1 D30](https://github.com/Nerver-zip/chess-database-api/releases/tag/search-elite-online-r1-d30-v1) and [Masters OTB R3 D40](https://github.com/Nerver-zip/chess-database-api/releases/tag/search-masters-otb-r3-d40-v1) search companions are published. Download the release assets to separate distribution directories. The checksummed original manifests retain their historical “0.2.0 (planned)” wording; v0.2.0 is now published and the manifests remain unchanged. Use the matching canonical dataset installed above, retain its CPI-only version for rollback, and use Chess Database API 0.2.0 or a compatible build supporting search-index format 1. A compatible existing installation does not need to download the canonical dataset again. The software and data licenses are separate; read the companion notice and the [Masters rights disclosures](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment) before use.

In each example, compare the downloaded manifest to the reviewed repository copy and verify all listed assets before using them. Do not extract an archive or install a bundle if a check fails. The commands require Bash, Python 3.11 or newer, `tar` with Zstandard support, and `sha256sum`. Paths to the distribution, repository and dataset are examples.

For the single-file Elite companion:

```bash
set -e
cd /path/to/elite-companion-dist
cmp elite-online-r1-d30-search-v1-RELEASE_MANIFEST.json \
  /path/to/chess-database-api/releases/search-elite-online-r1-d30-v1.json
sha256sum -c SHA256SUMS
python3 - <<'PYVERIFY'
import hashlib
import json
from pathlib import Path

manifest = json.loads(Path("elite-online-r1-d30-search-v1-RELEASE_MANIFEST.json").read_text())
archive = Path(manifest["archive"]["name"])
with archive.open("rb") as stream:
    digest = hashlib.file_digest(stream, "sha256").hexdigest()
if archive.stat().st_size != manifest["archive"]["size_bytes"] or digest != manifest["archive"]["sha256"]:
    raise SystemExit("Elite archive failed verification against the reviewed manifest")
PYVERIFY
mkdir -p extracted
tar --zstd -xf elite-online-r1-d30-search-v1.tar.zst -C extracted
chess-database install-search-index --dataset /srv/opening-explorer/elite-online \
  --bundle extracted/elite-search-bundle
chess-database verify-search-index --dataset /srv/opening-explorer/elite-online
```

For the split Masters companion, verify each part, concatenate in order, and verify the reconstructed archive against the reviewed manifest before extraction:

```bash
set -e
cd /path/to/masters-companion-dist
cmp search-masters-otb-r3-d40-v1.json \
  /path/to/chess-database-api/releases/search-masters-otb-r3-d40-v1.json
sha256sum -c SHA256SUMS
cat masters-otb-r3-d40-search-v1.tar.zst.part-001 \
    masters-otb-r3-d40-search-v1.tar.zst.part-002 > masters-otb-r3-d40-search-v1.tar.zst
python3 - <<'PYVERIFY'
import hashlib
import json
from pathlib import Path

manifest = json.loads(Path("search-masters-otb-r3-d40-v1.json").read_text())
archive = Path(manifest["archive"]["name"])
with archive.open("rb") as stream:
    digest = hashlib.file_digest(stream, "sha256").hexdigest()
if archive.stat().st_size != manifest["archive"]["size_bytes"] or digest != manifest["archive"]["sha256"]:
    raise SystemExit("Reconstructed archive failed verification")
PYVERIFY
mkdir -p extracted
tar --zstd -xf masters-otb-r3-d40-search-v1.tar.zst -C extracted
chess-database install-search-index --dataset /srv/opening-explorer/masters-otb \
  --bundle extracted/masters-search-bundle
chess-database verify-search-index --dataset /srv/opening-explorer/masters-otb
```

Both installs validate the canonical fingerprint and deeply verify the staged snapshot before atomic activation. Check collection capabilities after reloading the service. Masters discovery and game metadata remain disabled by default. An interrupted or rejected installation must not change the active snapshot; see [Search indexes](SEARCH_INDEX.md) for details.

**Rollback.** Earlier versions are kept under `versions/`. Copy the desired version to a staging directory outside the dataset root, retaining its exact build ID as the directory basename. Run `chess-database publish --candidate <staging/build-id> --target <root> --collection masters-otb` (or `elite-online`), then verify and reload or restart the service. The publisher rejects candidates inside the active dataset root and directory names that differ from the manifest build ID. See the [Docker rollback example](DOCKER_DEPLOYMENT.md#safe-dataset-updates).

**The filtered PGN** is a standard Zstandard stream: `zstd -dc <release>-filtered.pgn.zst | head`, or any Zstandard-capable reader. Verify its SHA-256 before extracting or processing it.

## 5. Filtered PGN

- One game per canonical stored game, in physical storage order (shard, block, slot). Output is deterministic for a given snapshot.
- **Written tags**: `Event`, `Site`, `Date`, `Round`, `White`, `Black`, `Result`, `WhiteElo`, `BlackElo`, `ECO`, `TimeControl`, `PlyCount`, `GameURL` (source game URL), `BroadcastURL`, `SetUp`/`FEN` for a non-standard start. Missing values use the PGN placeholders `?` and `????.??.??`.
- **Sidecar-derived tags** come from `source-attribution.jsonl`, joined by stable game ID:
  - `Source` lists every distinct publisher source tag attributed to the game, sorted and joined with `; `. A game can list more than one because duplicate source records were merged into it. The sidecar is project curation data. Its value for a game can differ from the `Source` tag of the single source record whose other tags were kept (see [benchmarks/gigabase/FINAL_REPORT.md](../benchmarks/gigabase/FINAL_REPORT.md)).
  - `WhiteTitle`, `BlackTitle`, `WhiteFideId`, `BlackFideId`, and `Mode` are written only when every attribution agrees on one value. Otherwise the tag is omitted and counted in the export report. `Mode` is absent from every game in this release.
- **Movetext** is the complete legal mainline in SAN with check and mate markers, wrapped before column 80. The final token equals the `Result` tag.
- **Not reconstructable** from stored data: comments, NAGs, variations, clock annotations, `Termination`, identity-evidence labels, and source-quality labels. None are written.
- Tag values escape `"` and `\`. A control character in a value fails the export rather than producing an invalid file.

Elite Online R1 exports the complete stored mainline for each of its 429,150 canonical games, not a 30-ply truncation. It uses persisted metadata and has no attribution sidecar, so no sidecar-derived fields are fabricated. As with Masters, comments, NAGs, variations, clock annotations, and original termination annotations are unavailable after ingestion and are not recreated.

The exporter is `chess-database export-pgn --dataset <snapshot> --output <file>[.zst] [--report <json>]`. It refuses to overwrite an output file and writes through a `.partial` file that is renamed only after a complete, checksummed stream.

## 6. Licensing and the distribution gate

- Software and data licenses are separate. Masters OTB R3 carries the Gigabase publisher's CC BY-NC 4.0 claim, unresolved upstream rights, and its collection-specific distribution decision; see [LICENSING.md](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment). Elite Online R1 is sourced from Lichess archives under CC0 1.0 and does not use the Masters clearance record; see [LICENSING.md](LICENSING.md#41-elite-online-artifacts-cc0-10-universal).
- Each release includes its own license notice and provenance in the indexed package and as a `<release>-NOTICE.md` asset. The notice is generated from that collection's source and license facts; Masters attribution text is not copied into Elite.
- The publication workflow validates each collection's license and provenance separately. The stricter [`releases/distribution_clearance.json`](../releases/distribution_clearance.json) gate remains required for Masters R3; CC0 Elite releases are checked against their committed provenance and manifest without requiring the Masters-specific record.
- The canonical statement of current policy, including public API deployment, is [LICENSING.md section 6](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment); this section covers only the release gate.
- **Current status: `cleared` (maintainer decision, 2026-11-16).** The record states that the maintainer relies on the publisher's public CC BY-NC 4.0 grant for non-commercial GitHub Release redistribution. It is the maintainer's interpretation, not separate written permission from the publisher. The recorded conflicts stay disclosed in that file. They include the snapshot's own release notice (local evaluation only, do not redistribute), the publisher's terms as reviewed (end-user bulk and raw-PGN downloads prohibited), and the absence of a publisher checksum.
- **Notice wording in `masters-otb-r3`.** The release assets are immutable and checksummed, so they keep their build-time text. The `RELEASE/DATASET_LICENSE.txt` inside the indexed package describes local evaluation only, and `masters-otb-r3-NOTICE.md` records the status "not_cleared" as of packaging. Both predate the maintainer's decision. Read them together with `releases/distribution_clearance.json` and the release notes, which state the current position. A future release should render a fresh notice at build time.
- The release notes of `dataset-masters-otb-r3` are editable and were written to reflect the current status; the asset files were not changed.

## 7. Maintainer procedure

1. **Prepare** from the active collection snapshot using its release identifier and root. This verifies the snapshot, builds the indexed package with the corpus release-package member rules, runs `export-pgn`, splits each product only when needed, and writes the manifest to `data/releases-dist/<release>/` and `releases/<release>.json`. Prepare never rebuilds the corpus. Elite R1 uses the existing `data/releases/elite-online` snapshot and release ID `elite-online-r1`.
2. **Verify** the dist directory: `python3 scripts/release_assets.py verify --manifest releases/<release>.json --dist-dir data/releases-dist/<release> --committed`. Run the independent PGN checker in the scratch environment (see section 8).
3. **Commit** `releases/<release>.json`, focused tests, documentation, and verification evidence on a branch, and merge it through review. Keep binary assets out of Git history.
4. **Tag** the reviewed merge commit: `git tag -a dataset-<release> <commit>`, then push the tag.
5. **Dry run** the upload: `python3 scripts/release_assets.py upload --manifest releases/<release>.json --dist-dir data/releases-dist/<release>`. It prints the plan and every blocker and uploads nothing.
6. **Upload to a draft** only after the reviewed commit has been tagged. Run `upload --execute` with the prepared asset directory. This creates a draft release and never publishes it. Masters must pass its clearance gate; Elite uses its CC0 validation.
7. **Publish** with the `Publish dataset release` workflow (manual, `workflow_dispatch`). It requires the tag to be reachable from `main`, the committed manifest and gate to pass, the draft to match the manifest by name and size, every downloaded byte to match its SHA-256, and the uploaded manifest to equal the committed one. Only then does it run `gh release edit --draft=false --latest=false`.

The workflow never rebuilds data. It asks GitHub not to mark the dataset release as "Latest" so that it does not displace software releases. GitHub still reports a release as latest when it is the repository's only published release (this is the case for `dataset-masters-otb-r3` until a software release exists); the flag then stops mattering once a software release is published.

## 8. Verification evidence

| Evidence | Produced by | Covers |
| --- | --- | --- |
| `benchmarks/gigabase/PGN_EXPORT_VERIFICATION_R3.json` | `scripts/release_pgn_verify.py` (scratch venv: python-chess, zstandard) | Masters headers, sidecar tags, `Result`, `PlyCount`, and SAN token count of every game against a direct decode of the CGB3 shards; full python-chess replay of every 16th game (205,277 of 3,284,429), comparing the replayed moves; probe positions compared with `chess-database query` |
| `benchmarks/gigabase/PGN_REIMPORT_LEGALITY_R3.json` | `chess-database build` over the whole filtered PGN | All 3,284,429 games reimported through the project's own ingest (shakmaty, not an independent parser): 0 illegal, 0 malformed, 0 variant rejections. Its policy and duplicate counts come from the default Masters policy and are not findings about the export |
| `benchmarks/gigabase/PGN_EXPORT_R3_REPORT.json` | `chess-database export-pgn --report` | Exact counts, omitted tags, and the sidecar hash used |
| `benchmarks/gigabase/INSTALL_VERIFICATION_R3.json` | Reconstruction, installation, rollback, and API run on the prepared assets | Reconstructed hashes, install into a scratch root, deep verification, rollback, and API responses. The published asset digests reported by GitHub equal the manifest checksums |

Elite Online R1 verification is kept separately under `benchmarks/elite-online/`; it checks every exported game against the canonical CGB3 records and independently replays legal mainlines, including results, supported metadata, final positions, and representative index queries. Installation and HTTP/API verification use the packaged assets and a separate scratch root.

The offline test suites are `scripts/test_release_assets.py` and `scripts/test_release_pgn_verify.py`. They run in `./scripts/validate.sh`. The network and scratch-environment checks are not part of the offline gate.
