# Masters OTB R3 D40 search companion

This derived search index complements the canonical [Masters OTB R3 release](https://github.com/Nerver-zip/chess-database-api/releases/tag/dataset-masters-otb-r3). It contains a searchable metadata projection and position occurrences for 3,284,429 curated games through ply 40. It does not replace the canonical dataset, CPI or PGN export.

Masters OTB games and derived opening statistics are from Lumbra's Gigabase, OTB Complete version 2026-11-11 (publisher claim: [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/)). Curated, filtered, normalized and indexed by the Chess Database API project. Not endorsed by the publisher. This companion is distributed for non-commercial use under the owner's recorded decision. That decision is not separate permission from the publisher: upstream game rights and the interaction with the publisher's B2B terms remain unresolved. Read the [full attribution and rights disclosures](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment) and the canonical release notices before use.

Use Chess Database API 0.2.0 or a compatible build supporting search-index format 1. Verify all parts, reconstruct the archive if split, and verify its SHA-256 against the accompanying release manifest before extraction. Install the canonical Masters OTB R3 indexed dataset first, or retain an existing compatible installation. Then extract the verified archive and run:

```sh
chess-database install-search-index --dataset /path/to/masters-otb --bundle masters-search-bundle
chess-database verify-search-index --dataset /path/to/masters-otb
```

Installation checks the canonical fingerprint and stages and verifies a new immutable snapshot before activation. See [Search indexes](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/SEARCH_INDEX.md) and [Dataset releases](https://github.com/Nerver-zip/chess-database-api/blob/main/docs/DATASET_RELEASES.md) for compatibility and rollback. Keep the original canonical version for rollback. Software and dataset licenses are separate. Masters discovery and search metadata remain disabled by default; the distribution decision does not itself authorize public search enumeration.
