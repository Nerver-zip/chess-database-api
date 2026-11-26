# Masters OTB R3 search companion (40-ply coverage)

This derived companion augments the existing `dataset-masters-otb-r3` canonical dataset. It does not replace its shards, CPI, game index, PGN export or provenance. Search index format 1 covers 3,284,429 games, 54,827,221 positions and 130,852,249 occurrences. Its canonical fingerprint is `7e451a13737bad29a3d9ccab2eb30458a2be169468f827d0bc473181523b77e0`.

Non-commercial distribution is subject to the attached attribution notice and the recorded unresolved upstream-rights and publisher-terms disclosures. Masters discovery remains disabled by default.

Use Chess Database API 0.2.0 or a compatible build that supports search-index format 1. Verify the attached `search-masters-otb-r3-d40-v1.json` against the reviewed repository manifest and check `SHA256SUMS`. For a split archive, concatenate the numbered parts in order and verify the reconstructed archive against the manifest before extraction. Follow the [companion verification and installation guide](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/DATASET_RELEASES.md#prepared-search-companions), including its rollback procedure. A compatible installed canonical dataset does not need to be downloaded again.

Archive `masters-otb-r3-d40-search-v1.tar.zst`: 2,446,907,948 bytes; SHA-256 `8d25cc01d0e4dad36f41b370a31bc8966d8d92525516ee2e6cefc40669e75a14`.

- `masters-otb-r3-d40-search-v1.tar.zst.part-001` — 1,992,294,400 bytes; SHA-256 `f7fe0e1e70948390493830d25cf0427c1fd29558996fd12618cf5cab9f78ec72`
- `masters-otb-r3-d40-search-v1.tar.zst.part-002` — 454,613,548 bytes; SHA-256 `17dac76e74c734cd8a75a7f195a8d4594210a184ca545b3b2bcfe1352fb7fefd`

The release also includes `search-masters-otb-r3-d40-v1.json`, `masters-otb-r3-d40-search-v1-NOTICE.md` and `SHA256SUMS`. Consult the manifest for artifact hashes, source snapshot, compatibility and coverage details.

## Install the verified companion

Install the matching canonical dataset first, or use an existing compatible installation. Download all assets from this release, then run the following in the download directory (replace the dataset path):

```bash
set -e
sha256sum -c SHA256SUMS
cat masters-otb-r3-d40-search-v1.tar.zst.part-001 masters-otb-r3-d40-search-v1.tar.zst.part-002 > masters-otb-r3-d40-search-v1.tar.zst
printf '%s  %s\n' '8d25cc01d0e4dad36f41b370a31bc8966d8d92525516ee2e6cefc40669e75a14' 'masters-otb-r3-d40-search-v1.tar.zst' | sha256sum -c -
mkdir -p extracted
tar --zstd -xf masters-otb-r3-d40-search-v1.tar.zst -C extracted
chess-database install-search-index --dataset /path/to/masters-otb --bundle extracted/masters-search-bundle
chess-database verify-search-index --dataset /path/to/masters-otb
```

Retain the original CPI-only version for [rollback](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/DATASET_RELEASES.md#4-download-verify-reconstruct-install).
