# Agent Handoff (chess-openings-db)

## Current objective

Continue Draft PR #6 on `feat/corpus-release-pipeline`, based on the latest fetched `origin/main`. Keep it Draft and unmerged. Work in the required `ssh agents` workspace. Do not modify Evalis or add frontend, cloud, Kubernetes, replication, new CPI formats, Explorer filters, or the future 15-million-game corpus.

## Current state

- Branch head before this follow-up is `01ec9b0`; latest fetched `origin/main` is `ddad720` and remains an ancestor.
- All 13 pinned source archives are present and passed `./scripts/corpus.sh verify-sources --collection all --profile baseline`.
- The event-level Masters qualification audit is deterministic and reconciles 42,612 source games: 29,640 verified OTB across 622 events, 1 verified online game, and 12,971 unresolved games across 365 events. Only verified OTB events are eligible for the next Masters release. Audit SHA-256: `a93763ac46a1e2c0653fe6ac5a4ab16c43ed4d53ca8132e0a806f29084f8c461`.
- The existing Elite Online release and its verified package are available locally. Masters and Elite R2 release trees are available; the Masters package will be created from the qualified R3 release.
- Docker/Compose deployment, safe package bootstrap, deep startup validation, and Rust atomic version publication are implemented. Read-only Compose bind mounts preserve the relative `current` symlink and immutable `versions/<id>` layout.
- The production image built in the prior turn. The fixture container smoke passed after fixing the test-only named-volume override: health and readiness, both collections, real stored fixture PGNs, 96/96 concurrent requests, restart, read-only root and mounts, no published host ports, invalid requests, failed dataset starts, graceful SIGTERM. It used a read-only named-volume copy because the Docker daemon cannot see host bind paths from its namespace.
- The first combined Python regression run had one Docker smoke error from invalid generated override YAML. That defect is fixed; the direct full fixture smoke now passes. Rerun the Docker regression suite and unified validation after the fix.

## Next actions

1. Review the final diff, update this handoff, and commit the implementation so the real release records a clean source commit.
2. Rebuild and deeply verify Masters from the event-qualified 29,640-game policy; keep Elite unchanged. Package and verify both releases.
3. Exercise Path A bootstrap from the two real packages into an isolated data root. Preserve the release IDs, checksums, and deep verification evidence.
4. Run the real Compose smoke with both real releases. Record health, readiness, collection counts, both `e4,e5,Nf3` expectations, stored PGNs, failure cases, restart, graceful shutdown, read-only mounts, concurrent request results, and measured RSS. Be explicit that Docker-in-Docker requires a read-only named-volume copy if host binds remain unavailable.
5. Refresh active documentation and benchmark evidence, preserve R2 evidence under `benchmarks/corpus/historical/2026.11-r2`, update `MANIFEST.sha256`, and run `./scripts/validate.sh`.
6. Inspect the full base-to-head diff, push the existing feature branch, update Draft PR #6 with current evidence, and check CI. Do not mark ready or merge.

## Evidence files

- `corpus/masters_event_qualification.json`: event-level classification evidence.
- `benchmarks/corpus/historical/2026.11-r2/`: archived R2 evidence.
- `docs/DOCKER_DEPLOYMENT.md`: provisioning, deployment, networking, and operations guide.
- Local source archives, releases, packages, and build scratch remain under git-ignored `data/`; never commit them.
