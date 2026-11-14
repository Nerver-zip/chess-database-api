# Agent Handoff (chess-openings-db)

## Current objective

Continue Draft PR #6 on feat/corpus-release-pipeline, based on the latest fetched origin/main. Keep it Draft and unmerged. Work in the required ssh agents workspace. Do not modify Evalis or add frontend, cloud, Kubernetes, replication, new CPI formats, Explorer filters, or the future 15-million-game corpus.

## Current state

- The PR branch is based on fetched origin/main ddad720; the current implementation includes Docker/Compose, portable release bootstrap, event-level Masters qualification, and atomic publication changes.
- All 13 pinned source archives are present and passed ./scripts/corpus.sh verify-sources --collection all --profile baseline.
- The deterministic event-level Masters audit reconciles 42,612 accepted source games: 29,640 verified OTB across 622 events, 1 verified online game, and 12,971 unresolved games across 365 events. Only verified OTB event IDs are eligible for Masters.
- The latest published Masters candidate contains 29,640 games, 705,718 positions at depth 40, and a 15,262,380-byte CPI. Its corrected build RSS measurement was 23,273,472 bytes. The user clarified that roughly 31 MB is acceptable; there is no strict 30,000,000-byte release gate.
- Commit c2eb949 fixes RSS measurement by sampling the executed process VmHWM instead of wait4 ru_maxrss, which included inherited Python orchestrator memory. Its regression test passed.
- The latest published Masters candidate records a commit that was removed while honoring the relaxed memory requirement; rebuild after the next clean source commit to keep release provenance traceable.
- The Rust CLI parses the large Masters audit through BufReader; this fix compiled in the pinned production image.
- The production image built successfully: image ID sha256:90e14eb57953d9ffe522071af9e169a984d3adffddac1c0e0e66220cc375526f, size 98,871,472 bytes.
- The fixture Compose smoke passed on that image: health/readiness, both fixture collections, fixture PGNs, 96/96 concurrent requests, restart, read-only root and mounts, no host ports, failed invalid starts, and graceful SIGTERM.
- Elite remains unchanged: 429,150 games, 6,136,307 positions at depth 30. Its active binaries match the previously recorded independent rebuild hashes.
- Earlier real Docker, HTTP, and package-bootstrap checks passed against a candidate Masters release, but release-specific evidence must be regenerated after final publication.
- Docker-in-Docker cannot see workspace host bind paths, so integration smoke uses read-only named-volume copies. Normal Compose deployment continues to use read-only bind mounts.

## Next actions

1. Commit this handoff update, then rebuild and deeply verify Masters from the committed tree.
2. Repackage both real releases and repeat Path A bootstrap into a fresh ignored directory.
3. Regenerate release evidence, reproducibility hashes, package checksums, full HTTP checks, and the real Docker Compose report for final release IDs.
4. Refresh current documentation, preserve R2 evidence, update MANIFEST.sha256, and run ./scripts/validate.sh.
5. Review the complete base-to-head diff, push the existing branch, refresh Draft PR #6, and inspect CI. Do not mark ready or merge.

## Evidence paths

- corpus/masters_event_qualification.json: per-event provenance and policy evidence.
- benchmarks/corpus/historical/2026.11-r2/: preserved R2 evidence.
- docs/DOCKER_DEPLOYMENT.md: provisioning, deployment, networking, and operations guide.
- Source archives, releases, packages, and build scratch remain under git-ignored data/; never commit them.
