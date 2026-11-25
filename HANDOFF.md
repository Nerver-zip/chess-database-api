# Chess Database API Handoff

## Current state

[Draft PR #11](https://github.com/Nerver-zip/opening-explorer-api/pull/11) targets `main` on `feat/chess-database-api`. The GitHub repository still uses its original slug. The Rust workspace and canonical CLI use the Chess Database API names. Compose preserves the previous project and service names, mount targets, network aliases and image compatibility tag. The owner’s post-merge rename procedure is in [Naming migration](docs/NAMING_MIGRATION.md).

The service reads immutable, independently curated Masters OTB and Elite Online snapshots. Legacy clients use CPI v2 and canonical PGNs. Optional discovery uses the SQLite catalog and occurrence postings. Common exact rating, year and date filters now use a bounded ordinal projection. `min_plies=0` can use CPI directly. Two search workers are the default, with bounded queueing and a single filtered aggregation slot.

## Production evidence

- Both compact snapshots already passed deep verification on release inputs without rebuilding canonical games. Elite R1 D30 contains 429,150 games and 13,191,619 search occurrences. Masters R3 D40 contains 3,284,429 games and 130,852,249 occurrences. Canonical file checksums and published release identities are intact. See [benchmark evidence](docs/research/search-index-experiments.md).
- The first native hardening repeat converted each of three reproducible Masters 422s to 200. Initial position with both ratings at least 2200 measured 204 ms p95. Sicilian position with both ratings at least 2400 measured 37 ms p95. The long Ruy Lopez `min_plies=0` request measured 1.5 ms p95.
- The projected-filter unit tests passed for exact SQL equivalence, nullable ratings, partial dates, unknown results, cancellation and preservation of the complex-filter work limit.
- Colab's host cgroup mount is read-only and there is no KVM. A local QEMU guest is live in session `28396`. The actual pinned repository Docker build and the ext4 production dataset transfer are in sessions `5895` and `7587`; outputs are under ignored `data/hardening/`. No owner server is involved. Compose startup still checks artifact identities, checksums and required headers. Set `CHESS_DATABASE_DEEP_VALIDATE_ON_STARTUP=true` to repeat the deep publication checks.
- The final paired legacy and interactive run is in session `67329`. Final binary and original PR baseline hashes are in `data/hardening/final-binaries.sha256`. Unified validation is running in session `61787`. Restore the historical Stage A JSON if the reproduction test rewrites it.

Pending: archive final exact-count, concurrency, paired HTTP and operational measurements. Apply any validation fixes, then update and verify the remote draft PR. Keep all earlier benchmark experiments.

## Published datasets and licensing

Masters OTB R3 and Elite Online R1 are published and remain the canonical release inputs. Use the versioned release records and checksums in [Dataset releases](docs/DATASET_RELEASES.md). Historical Masters qualification, reimport, Docker and package evidence is under [benchmarks/gigabase](benchmarks/gigabase/). Elite R1 verification is in the [release evidence](benchmarks/elite-online/REBUILD_VERIFICATION_R1.json). Release records and those artifacts retain their original names and terms.

[Licensing](docs/LICENSING.md) links the maintainer's distribution and deployment decisions and records unresolved upstream rights. Masters discovery and search metadata default to disabled and require a separate owner review before public enumeration. Legacy PGN retrieval remains independently configurable.
