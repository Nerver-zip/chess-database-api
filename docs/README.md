---
summary: "Navigable index of Chess Database API documentation, grouped by reader need."
read_when: "Starting work, looking for a specification, or deciding which document answers a question."
status: current
---

# Documentation Index

Start with the [project README](../README.md). Always read the source code when changing behavior; this index separates the documents users need from the ones contributors and researchers need.

## 1. Use the service

- [Getting Started](GETTING_STARTED.md): build the CLI, create a sample dataset, serve it, and make a first query.
- [API Reference](API_REFERENCE.md): endpoints, parameters, response fields, errors, HTTP semantics, configuration, and how to interpret statistics.
- [Search API](SEARCH_API.md): game, position, player and event discovery, filtered statistics and Evalis integration.
- [Use Cases](USE_CASES.md): integration patterns for apps, collection comparison, analysis pipelines, and research downloads. Runnable code is in [`examples/`](../examples/).

## 2. Datasets and licensing

- [Dataset Releases](DATASET_RELEASES.md): the separate Masters OTB and Elite Online indexed datasets and filtered PGNs, asset naming, checksum verification, installation, versioning, collection-specific licenses, and the manual publication procedure.
- [Licensing and Attribution](LICENSING.md): software licenses, dataset licenses, the current policy for public non-commercial use, the terms for applications, required acknowledgments, modifications made, and open rights questions.
- [Corpus and Deduplication Policy](CORPUS_AND_DEDUPLICATION.md): collection segregation, rating isolation, selection policies, deduplication, and accounting invariants.

## 3. Deploy

- [Docker Deployment](DOCKER_DEPLOYMENT.md): provision releases, run the API with read-only mounts, resource bounds, rollback, and running a public non-commercial instance.
- [Naming migration](NAMING_MIGRATION.md): Chess Database API defaults and the retained CLI, image and configuration aliases.
- [Migration from CODB](MIGRATION_FROM_CODB.md): legacy-to-current names for the binary, environment variables, compose files, and image.

## 4. Build and develop

- [Corpus Build and Release Procedure](CORPUS_BUILD.md): reproducible acquisition, curation, indexing, verification, packaging, and promotion.
- [Search Indexes](SEARCH_INDEX.md): build, verify and operate derived search indexes on canonical snapshots.
- [QA Strategy](QA_STRATEGY.md): integrity testing, crash recovery, snapshot isolation, and verification gates.
- [Contributing](../CONTRIBUTING.md), [Agent Development Guide](../AGENTS.md), [Changelog](../CHANGELOG.md).

## 5. Technical architecture

- [Architecture](ARCHITECTURE.md): the seven crates, data flow, sharding, locking, and memory bounds.
- [Data Contracts](DATA_CONTRACTS.md): normative binary formats (CGB3, `games.idx`, CPI v2) and domain models.
- [Architecture Decision Records](adr/README.md): [ADR-0001](adr/0001-initial-architecture.md), [ADR-0002](adr/0002-bounded-ingestion-and-persistent-cgb3-archive.md), [ADR-0003](adr/0003-production-cpi-index.md), [ADR-0004](adr/0004-opening-explorer-http-api.md).
- [Decisions and Open Questions](DECISIONS_AND_OPEN_QUESTIONS.md) and [Roadmap](ROADMAP.md).

## 6. Benchmarks and historical research

Preserved for reproducibility. Ordinary users do not need these.

- [Benchmarks and Reproduction Guide](BENCHMARKS.md): methodology, production CPI baselines, and reproduction steps.
- Historical research (status `historical` or `reference`, never a production specification): [storage feasibility](research/feasibility-report.md), [storage options](research/storage-options.md), [source inventory](research/source-inventory.md), [source licensing](research/source-licenses.md), [corpus policy](research/corpus-policy.md), [deduplication](research/deduplication.md), [benchmark reproduction](research/benchmark-reproduction.md), [API contract draft](research/api-contract.md).
- Release evidence for the active Masters corpus: [`benchmarks/gigabase/FINAL_REPORT.md`](../benchmarks/gigabase/FINAL_REPORT.md).
