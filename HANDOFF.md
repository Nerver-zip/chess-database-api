# Chess Database API Handoff

## Current state

[Draft PR #11](https://github.com/Nerver-zip/chess-database-api/pull/11) targets `main` on `feat/chess-database-api`. GitHub now reports the repository as `Nerver-zip/chess-database-api` and redirects its former slug. `chess-database` is the canonical CLI. Existing CLI, environment, image and network aliases remain available. The canonical and legacy environment samples rendered identical default and named-volume Compose configurations. See the [post-rename checks](docs/NAMING_MIGRATION.md#repository-rename-and-post-cutover-checks).

The corpus and local Gigabase tools document `--chess-database` and accept `--opening-explorer` as an alias. Current corpus instructions and diagnostics use the canonical name. The published release manifests retain their original provenance and checksums.

Both GitHub repository URLs resolve to the same repository. The Masters R3 and Elite R1 release IDs match across the redirect. GitHub's distribution asset sizes and SHA-256 digests match both committed release manifests.

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

Both enriched snapshots reached readiness in the pinned runtime with the native release binary under QEMU after 1,006 seconds at 512 MiB and two CPUs. This missed the 600-second window. The service cgroup reported zero OOM events. All 480 legacy probes in the two emulated interactive runs passed, and all 240 passed during reload verification. The reload client timed out at 900 seconds. After a guest pause for native benchmarking, the server logged successful completion for both versions. See the [pinned runtime report](benchmarks/search/HARDENING_TWO_COLLECTION_QEMU.json).

The [repository Docker image](benchmarks/search/HARDENING_REPOSITORY_IMAGE_QEMU.json) built successfully at `5b94bbe`. With both enriched snapshots and default serving settings at 512 MiB and two CPUs, `/ready` passed in 549 seconds after guest page-cache eviction. Docker reported healthy and the cgroup recorded 550 CPU seconds, a 536,875,008-byte peak and zero OOM events after startup probes. The Elite discovery flow passed. All 720 legacy requests passed across the mixed and SIGHUP reload workloads. The same-snapshot reload completed in 536.41 seconds. Local diagnostics are in `data/hardening/`. The host's cgroup filesystem is read-only, and the fallback guest has no KVM acceleration. Run the [isolated two-collection Docker acceptance](docs/DOCKER_DEPLOYMENT.md#two-collection-acceptance-on-a-local-docker-host) on the owner host.

## Open owner actions

Review the eviction-advised timing results and complete native Docker acceptance before rollout. The owner-host reload check verifies that Elite switches to the intended alternate snapshot and Masters stays on its pinned version. Leave Masters discovery and search metadata disabled until the owner reviews public enumeration and attribution. Legacy PGN retrieval is separately configurable. Masters R3 and Elite R1 stay the canonical releases. See [dataset releases](docs/DATASET_RELEASES.md) and [licensing](docs/LICENSING.md).

The original goal requires owner approval for expensive remote validation. The [hosted trial](https://github.com/Nerver-zip/chess-database-api/actions/runs/45732126046) was canceled during compilation, before any dataset download or indexing. It produced no acceptance result. The remaining native checks can run on an isolated owner host or on an approved disposable runner.
