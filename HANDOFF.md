# Agent Handoff (chess-openings-db)

## Current objective

Continue Draft PR #6 on feat/corpus-release-pipeline, based on the latest fetched origin/main. Keep it Draft and unmerged. Work in the required ssh agents workspace. Do not modify Evalis or add frontend, cloud, Kubernetes, replication, new CPI formats, Explorer filters, or the future 15-million-game corpus.

## Current state

- The PR branch is based on fetched origin/main ddad720. The implementation covers a Dockerized read-only API, Compose deployment, portable release bootstrap, Masters event qualification, and atomic release publication.
- All 13 pinned source archives are present and were checksum-verified. Path B has been used to rebuild the final Masters release; Path A package bootstrap was exercised for both final releases.
- The Masters event audit reconciles the 42,612-game source-qualified baseline: 29,640 verified OTB games across 622 events, 1 verified online game, and 12,971 unresolved games across 365 events. Only the verified OTB event IDs enter the release.
- Final Masters release: `codb-masters-otb-1794689403953239696-66953-9aa01bd5606eaf65-cpi-d40`; 29,640 games, 705,718 unique positions, depth 40, deployed directory 29,892,368 bytes. The executed-process build RSS was 23,015,424 bytes.
- Elite remains unchanged: `codb-elite-online-1794670050929819232-27177-f042ce0f4dfe2697-cpi-d30`; 429,150 games, 6,136,307 unique positions, depth 30, deployed directory 210,308,033 bytes. Current binary artifact hashes match two independently rebuilt baseline releases.
- The user clarified that roughly 31 MB ingestion RSS is acceptable. The strict 30 MB cutoff was removed from repository guidance and release tooling; per-process RSS remains recorded for build evidence.
- Both verified packages passed checksum, safe extraction, and deep integrity verification through a fresh Path A bootstrap at `data/work/path-a-bootstrap-final`.
- `corpus.sh http-check --requests 1000 --per-stratum 40 --sample-games 6000` passed 414/414 checks and 262 real HTTP/CLI position comparisons with no mismatches.
- Real Compose smoke passed against both final releases with an independent digest-pinned client reaching `http://codb:8080`: health/readiness, correct opening counts (3,902 Masters; 30,390 Elite), real PGNs, 96/96 concurrent requests, restart, invalid mounts, read-only volumes/root, no host ports, admin reload disabled, and graceful SIGTERM. Sampled peak RSS was 159,224 KiB under the 512 MiB cap. Evidence: `benchmarks/corpus/docker_real_check.json`.
- The Docker-in-Docker daemon could not access workspace bind paths, so the real smoke copied the releases into read-only named volumes. The normal Compose file uses read-only host bind mounts; its configuration is included in validation.
- Masters binary artifact hashes matched an independent rebuild and CPI-index pass. Elite hashes match existing independent rebuild evidence. Package checksums, release evidence, HTTP examples, and documentation are being refreshed in the current worktree.

- Package extraction now works on Python 3.11 without the unavailable TarFile.extractall(filter=...) API. It validates paths and symlink containment, extracts directories and files before relative symlinks, and normalizes file modes. Regression tests cover path traversal, symlink escapes, metadata normalization, and real package round trips.
- RSS capture reads the child process's procfs VmHWM when available and falls back to child-specific wait4 usage for short-lived commands. This keeps the Python parent out of the measurement and no fixed 30 MB ceiling is enforced.
- The pinned Rust 1.100 validation container completed ./scripts/validate.sh successfully on Python 3.11: formatting, strict Clippy, workspace tests, all 95 Python regression tests, corpus evidence, benchmark schemas, docs, and MANIFEST.sha256 passed. Dockerfile.validation installs rustfmt and Clippy. The host itself has no cargo executable.

## Next actions

1. Review the complete base-to-head diff, confirm tracked evidence and docs match the current releases, and commit the final updates.
2. Push feat/corpus-release-pipeline, refresh the existing Draft PR #6 description with current provenance and verification evidence, and inspect hosted CI. Keep the PR Draft and unmerged.

## Evidence paths

- `corpus/masters_event_qualification.json`: per-event provenance and policy evidence.
- `benchmarks/corpus/`: current release, reproducibility, package, HTTP, and real Docker evidence.
- `benchmarks/corpus/historical/2026.11-r2/`: preserved R2 evidence.
- `docs/DOCKER_DEPLOYMENT.md`: provisioning, deployment, networking, and operations guide.
- Source archives, releases, packages, and build scratch remain under git-ignored `data/`; never commit them.
