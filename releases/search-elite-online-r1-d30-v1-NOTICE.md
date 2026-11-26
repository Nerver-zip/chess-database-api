# Elite Online R1 D30 search companion

This derived search index complements the canonical [Elite Online R1 release](https://github.com/Nerver-zip/chess-database-api/releases/tag/dataset-elite-online-r1). It contains a searchable metadata projection and position occurrences for 429,150 curated games through ply 30. It does not replace the canonical dataset, CPI or PGN export. The underlying Lichess rated-game archives are dedicated to the public domain under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/); source periods and provenance are recorded in the canonical release manifest.

Use Chess Database API 0.2.0 or a compatible build supporting search-index format 1. Verify the archive against the accompanying SHA-256 and release manifest before extraction. Install the canonical Elite Online R1 indexed dataset first, or retain an existing compatible installation. Then extract the verified archive and run:

```sh
chess-database install-search-index --dataset /path/to/elite-online --bundle elite-search-bundle
chess-database verify-search-index --dataset /path/to/elite-online
```

Installation checks the canonical fingerprint and stages and verifies a new immutable snapshot before activation. See [Search indexes](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/SEARCH_INDEX.md) and [Dataset releases](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/DATASET_RELEASES.md) for compatibility, operation and rollback. Keep the original canonical version for rollback. Software and dataset licenses are separate.
