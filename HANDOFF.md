# Chess Database API Handoff

## Current state

[Draft PR #11](https://github.com/Nerver-zip/opening-explorer-api/pull/11) targets `main` on `feat/chess-database-api`. Chess Database API is the current product identity and `chess-database` is the canonical CLI. Existing CLI, environment, image and network aliases remain available. The canonical and legacy environment samples rendered identical default and named-volume Compose configurations. The owner can rename the repository after merge using the [naming procedure](docs/NAMING_MIGRATION.md).

The service pins separate immutable Masters OTB and Elite Online snapshots. Legacy opening statistics use CPI v2 and legacy PGNs use canonical game storage. Optional discovery adds a SQLite catalog and compact occurrence postings. Common rating and date filters use a bounded ordinal scan. An otherwise unrestricted `min_plies=0` query uses CPI. Two search workers and one filtered aggregation slot are the serving defaults.

## Verification evidence

- Masters OTB R3 D40 has 3,284,429 games and 130,852,249 occurrences; Elite Online R1 D30 has 429,150 games and 13,191,619 occurrences. Both enriched snapshots passed deep verification and retain their canonical artifact checksums. See [production search evidence](docs/research/search-index-experiments.md).
- All three originally failing Masters filtered requests returned HTTP 200 in the hardened serial workload. An independent walk of canonical Masters games verified every move's W/D/L for the two rating queries and the Sicilian year and date filters. The no-op ply filter matched the legacy CPI response body. See the [hardening evidence and replay commands](docs/research/search-index-experiments.md#final-hardening-on-both-production-snapshots).
- Two workers served all 40 attempts each for player autocomplete, metadata, positional search and latest-board navigation in the four-user workload. Broad rating and date filters each served 24/40 with 16 busy responses. All 300 legacy requests passed in a separate overload workload.
- The legacy warm pair passed all timing thresholds. The eviction-advised pair matched bodies, ETags and artifact identities but sequential p50 increased 7.9%, above its 5% threshold. Sequential p95 increased 1.4%, concurrent p95 fell 3.8%, and throughput rose 2.9% in that pair. The raw repetitions and older experiments are preserved.
- The unified validation gate passed all eight stages, including 217 Python tests with eight environment-dependent skips. The native discovery flows passed on both enriched collections. The README search and PGN workflow and the live curl, JavaScript and Python quickstarts also passed against locally built fixtures.

## Operational acceptance

Serving checks digests, headers, accounting and the full CPI structure at startup and reload. Deep canonical and search verification is mandatory at publication and optional as a repeated serving check. The completed Elite-only QEMU tests include 214 successful Explorer and PGN probes through live rollback. The host cgroup filesystem is read-only and the fallback guest has no KVM acceleration. Use the [isolated two-collection acceptance commands](docs/DOCKER_DEPLOYMENT.md#two-collection-acceptance-on-a-local-docker-host) for representative cold-start, limit, sustained-traffic and reload measurements. Local QEMU work uses the existing dataset copies under the ignored `data/` directory.

## Open owner actions

Review the documented eviction-advised p50 result and production Docker acceptance before rollout. Leave Masters discovery and search metadata disabled until the owner reviews public enumeration and attribution. Legacy PGN retrieval is separately configurable. Masters R3 and Elite R1 stay the canonical releases. See [dataset releases](docs/DATASET_RELEASES.md) and [licensing](docs/LICENSING.md).
