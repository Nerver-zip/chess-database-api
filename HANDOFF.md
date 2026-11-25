# Chess Database API Handoff

## Current state

[Draft PR #11](https://github.com/Nerver-zip/opening-explorer-api/pull/11) targets `main` on `feat/chess-database-api`. Chess Database API is the current product identity and `chess-database` is the canonical CLI. Existing CLI, environment, image and network aliases remain available. The canonical and legacy environment samples rendered identical default and named-volume Compose configurations. The owner can rename the repository after merge using the [naming procedure](docs/NAMING_MIGRATION.md).

The service pins separate immutable Masters OTB and Elite Online snapshots. Legacy opening statistics use CPI v2 and legacy PGNs use canonical game storage. Optional discovery adds a SQLite catalog and compact occurrence postings. Common rating and date filters use a bounded ordinal scan. An otherwise unrestricted `min_plies=0` query uses CPI. Two search workers and one filtered aggregation slot are the serving defaults.

## Verification evidence

- Masters OTB R3 D40 has 3,284,429 games and 130,852,249 occurrences; Elite Online R1 D30 has 429,150 games and 13,191,619 occurrences. Both enriched snapshots passed deep verification and retain their canonical artifact checksums. See [production search evidence](docs/research/search-index-experiments.md).
- All three originally failing Masters filtered requests returned HTTP 200 in the hardened serial workload. An independent walk of canonical Masters games verified every move's W/D/L for the two rating queries and the Sicilian year and date filters. The no-op ply filter matched the legacy CPI response body. See the [hardening evidence and replay commands](docs/research/search-index-experiments.md#final-hardening-on-both-production-snapshots).
- Two workers served all 40 attempts each for player autocomplete, metadata, positional search and latest-board navigation in the four-user workload. Broad rating and date filters each served 24/40 with 16 busy responses. All 300 legacy requests passed in a separate overload workload.
- The legacy warm pair passed all timing thresholds. The first eviction-advised run exceeded the 5% sequential p50 threshold at +7.9%. One five-pair repeat measured −0.9% sequential p50 and +6.7% sequential p95. A second repeat with the guest paused passed sequential thresholds but failed concurrent p95 (+12.2%), p99 (+13.7%) and throughput (−5.5%). All matched bodies, ETags and canonical artifact identities. The raw repetitions, failed timing gates and older experiments are preserved.
- An identical-binary control under the same paused-guest setup passed the directional comparator and measured 13.9% lower concurrent p95 in its second arm. A representative isolated native comparison remains open. See the [paired timing evidence](docs/research/search-index-experiments.md#paired-legacy-http-and-cpi).
- The unified validation gate passed all eight stages, including 217 Python tests with eight environment-dependent skips. The native discovery flows passed on both enriched collections. The README search and PGN workflow and the live curl, JavaScript and Python quickstarts also passed against locally built fixtures.

## Operational acceptance

Serving checks digests, headers, accounting and the full CPI structure at startup and reload. Deep canonical and search verification is mandatory at publication and optional as a repeated serving check. Native same-snapshot reload of both enriched collections took 14.84 seconds under mixed traffic. The Elite-only QEMU rollback passed all 214 legacy probes.

Both enriched snapshots reached readiness in the pinned runtime with the native release binary under QEMU after 1,006 seconds at 512 MiB and two CPUs. This missed the 600-second window. The service cgroup reported zero OOM events. All 480 legacy probes in the two emulated interactive runs passed, and all 240 passed during reload verification. The reload client timed out at 900 seconds. After a guest pause for native benchmarking, the server logged successful completion for both versions. The separate repository Dockerfile build remains in progress. Local diagnostics are in `data/hardening/`. The host's cgroup filesystem is read-only, and the fallback guest has no KVM acceleration. Run the [isolated two-collection Docker acceptance](docs/DOCKER_DEPLOYMENT.md#two-collection-acceptance-on-a-local-docker-host) on the owner host.

The local observer in `data/hardening/observe-repository-image.sh` waits for the repository image labeled `5b94bbe311cff5dca1681886e5cdec4d18e5127b`. It then checks the installed binary and CLI alias and measures both snapshots in a separate guest Compose project with default serving settings. The build and observer logs are under `data/hardening/`.

## Open owner actions

Review the eviction-advised timing results and complete native Docker acceptance before rollout. Leave Masters discovery and search metadata disabled until the owner reviews public enumeration and attribution. Legacy PGN retrieval is separately configurable. Masters R3 and Elite R1 stay the canonical releases. See [dataset releases](docs/DATASET_RELEASES.md) and [licensing](docs/LICENSING.md).
