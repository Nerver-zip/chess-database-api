# Elite Online R1 search companion (30-ply coverage)

This derived companion augments the existing `dataset-elite-online-r1` canonical dataset. It does not replace its shards, CPI, game index, PGN export or provenance. Search index format 1 covers 429,150 games, 6,558,404 positions and 13,191,619 occurrences. Its canonical fingerprint is `431595228cdcfed941c5d3542ba38c417014c0a2e76f333b912c149cf7d2fcac`.

The underlying Lichess sources are dedicated to the public domain under CC0 1.0. See the attached notice and canonical release for provenance.

Use Chess Database API 0.2.0 or a compatible build that supports search-index format 1. Verify the attached `elite-online-r1-d30-search-v1-RELEASE_MANIFEST.json` against the reviewed repository manifest and check `SHA256SUMS`. For a split archive, concatenate the numbered parts in order and verify the reconstructed archive against the manifest before extraction. Follow the [companion verification and installation guide](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/DATASET_RELEASES.md#prepared-search-companions), including its rollback procedure. A compatible installed canonical dataset does not need to be downloaded again.

Archive `elite-online-r1-d30-search-v1.tar.zst`: 269,896,854 bytes; SHA-256 `f011e539407ae49c7623a315456696e33ee5e5c4b0477cbca7f655859d6f1cca`.

- `elite-online-r1-d30-search-v1.tar.zst` — 269,896,854 bytes; SHA-256 `f011e539407ae49c7623a315456696e33ee5e5c4b0477cbca7f655859d6f1cca`

The release also includes `elite-online-r1-d30-search-v1-RELEASE_MANIFEST.json`, `elite-online-r1-d30-search-v1-NOTICE.md` and `SHA256SUMS`. Consult the manifest for artifact hashes, source snapshot, compatibility and coverage details.

## Install the verified companion

Install the matching canonical dataset first, or use an existing compatible installation. Download all assets from this release, then run the following in the download directory (replace the dataset path):

```bash
set -e
sha256sum -c SHA256SUMS
printf '%s  %s\n' 'f011e539407ae49c7623a315456696e33ee5e5c4b0477cbca7f655859d6f1cca' 'elite-online-r1-d30-search-v1.tar.zst' | sha256sum -c -
mkdir -p extracted
tar --zstd -xf elite-online-r1-d30-search-v1.tar.zst -C extracted
chess-database install-search-index --dataset /path/to/elite-online --bundle extracted/elite-search-bundle
chess-database verify-search-index --dataset /path/to/elite-online
```

Retain the original CPI-only version for [rollback](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/DATASET_RELEASES.md#4-download-verify-reconstruct-install).
