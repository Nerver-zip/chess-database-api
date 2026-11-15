---
summary: "Navigable index of all canonical engineering documentation and architectural records."
read_when: "Starting work, searching for authoritative specifications, or exploring project documentation."
status: current
---

# Documentation Index (`chess-openings-db`)

Welcome to the canonical technical documentation for `chess-openings-db`. Always read the source code when changing behavior; this index covers current production contracts, maintained workflows, and historical decision records.

---

## 1. System Architecture & Core Concepts

- [System Architecture](ARCHITECTURE.md) — Comprehensive end-to-end architecture across all seven crates (`core`, `ingest`, `curate`, `storage`, `index`, `api`, `cli`), data flow, sharding layout, advisory locking, and memory bounds.
- [Docker Deployment](DOCKER_DEPLOYMENT.md) — Provision verified releases, run the API with read-only mounts, and connect another container over a shared network.
- [Corpus Build & Release Procedure](CORPUS_BUILD.md) — Step-by-step reproducible workflow from the pinned source catalog through checksum-verified acquisition, curated builds, CPI indexing, deep verification, release packaging, `codb serve`, and HTTP validation; resource requirements, license obligations, and failure recovery.
- [Data Contracts](DATA_CONTRACTS.md) — Normative binary format specifications for CGB3 containers (`shard_XXXX.cgb3`), global binary search index (`games.idx`), Compact Position Index (`positions.cpi`), domain models, and the current HTTP API payload schema.
- [Corpus & Deduplication Policy](CORPUS_AND_DEDUPLICATION.md) — Rules governing collection segregation (`masters-otb` vs `elite-online`), rating isolation, speed category filtering, multi-key deduplication, prefix-supersession, and accounting reconciliation invariants.

---

## 2. Quality, Testing & Performance

- [QA Strategy](QA_STRATEGY.md) — Integrity testing methodology, crash recovery simulations, reader snapshot isolation, staging fault injection, clippy linting rules, and verification gates.
- [Benchmarks & Reproduction Guide](BENCHMARKS.md) — Authoritative benchmark methodology, production CPI baseline matrix, empirical dataset checksums, step-by-step reproduction instructions, and Heaps' Law scaling projections.

---

## 3. Governance, Licensing & Legal

- [Licensing & Data Provenance](LICENSING.md) — Codebase dual-licensing (`LICENSE-MIT` / `LICENSE-APACHE`), clean-room zero-AGPL boundary, source dataset provenance (Lichess CC0 and CC BY-SA 4.0), and redistribution requirements.
- [Contributor Guidelines](../CONTRIBUTING.md) — Contribution workflow, code formatting, PR standards, and local verification.
- [Delivery Changelog](../CHANGELOG.md) — Versioned history of milestone deliverables and release capabilities.

---

## 4. Architectural Decisions & Planning

- [Roadmap & Milestones](ROADMAP.md) — Progress tracker from Stage A feasibility through persistent CGB3 archives, production CPI v2 index, and the Dockerized standalone API service.
- [Decisions & Open Questions](DECISIONS_AND_OPEN_QUESTIONS.md) — Record of accepted technical trade-offs, open design questions for the upcoming HTTP service, and historical research classifications.
- [Architecture Decision Records (ADRs)](adr/README.md) — Formal register of architectural decisions:
  - [ADR-0001: Initial Architecture Decisions](adr/0001-initial-architecture.md)
  - [ADR-0002: Bounded-Memory Ingestion, CGB3 Sharding & Global Lookup](adr/0002-bounded-ingestion-and-persistent-cgb3-archive.md)
  - [ADR-0003: Production CPI Index, External Sorting & Explorer Queries](adr/0003-production-cpi-index.md)
  - [ADR-0004: Production Opening Explorer HTTP API](adr/0004-opening-explorer-http-api.md)

---

## 5. Historical Research Documents

Initial feasibility studies and exploratory research deliverables preserved for historical provenance and reproducibility:
- [Storage Feasibility Report](research/feasibility-report.md) — Empirical feasibility calculations and Heaps' Law parameter fitting.
- [Storage Options Trade-Off Study](research/storage-options.md) — Comparative evaluation of SQLite, RocksDB, flat binary blocks, and CGB containers.
- [Source Inventory & Evaluation](research/source-inventory.md) — Early survey and legal classification of prospective chess game data sources.
- [Source Licensing Architecture](research/source-licenses.md) — Original legal architecture and CC BY-SA / CC0 segregation plan.
- [Corpus Inclusion Policies (Research)](research/corpus-policy.md) — Early draft of collection policies (superseded by [CORPUS_AND_DEDUPLICATION.md](CORPUS_AND_DEDUPLICATION.md)).
- [Deduplication & Collision Study](research/deduplication.md) — Research notes on multi-key chess identity (superseded by [CORPUS_AND_DEDUPLICATION.md](CORPUS_AND_DEDUPLICATION.md)).
- [Benchmark Reproduction (Research)](research/benchmark-reproduction.md) — Initial PR-01 reproduction notes (superseded by [BENCHMARKS.md](BENCHMARKS.md)).
- [API Contract (Research)](research/api-contract.md) — Preliminary API draft (superseded by [DATA_CONTRACTS.md](DATA_CONTRACTS.md)).
