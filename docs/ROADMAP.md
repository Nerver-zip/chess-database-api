---
summary: "Published corpus milestones, draft Chess Database API delivery and future work."
read_when: "Reviewing milestone status or deciding which work is still open."
status: current
---

# Project roadmap

| Milestone | Status | Evidence |
| --- | --- | --- |
| Initial research and streaming CGB3 storage | Delivered in PRs #1 and #2 | [Storage architecture](ARCHITECTURE.md) and [historical feasibility report](research/feasibility-report.md) |
| Production CPI v2 and first Opening Explorer HTTP routes | Delivered in PRs #3 and #5 | [Position index ADR](adr/0003-production-cpi-index.md) and [HTTP contract](API_REFERENCE.md) |
| Reproducible builds and Docker serving | Delivered in PR #6 | [Corpus builds](CORPUS_BUILD.md) and [deployment](DOCKER_DEPLOYMENT.md) |
| Masters OTB R3, Gigabase qualification and first naming migration | PRs #7 and #8 merged. R3 published on 2026-11-16. | [Dataset releases](DATASET_RELEASES.md), [licensing decision](LICENSING.md), and [R3 measurements](../benchmarks/gigabase/FINAL_REPORT.md) |
| Elite Online R1 | PR #10 merged. R1 published from seven selected Lichess archive periods. | [Dataset releases](DATASET_RELEASES.md) and [release verification](../benchmarks/elite-online/REBUILD_VERIFICATION_R1.json) |
| Chess Database API and optional search enrichment | [Draft PR #11](https://github.com/Nerver-zip/chess-database-api/pull/11) | [Search API](SEARCH_API.md), [naming migration](NAMING_MIGRATION.md), and [production search evidence](research/search-index-experiments.md) |

Draft PR #11 retains the CPI, CGB3 and stable game contracts. It adds game, player and event discovery, positional search, exact filtered Explorer statistics, and immutable search companions. Masters search exposure remains an operator opt-in under the [current deployment policy](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment).

## Future work

- Validate additional query plans for broad combinations of player, event or site with common positions.
- Evaluate incremental updates and content-addressed artifact distribution against measured build and storage costs.
- Evaluate new corpus sources independently, with provenance and license approval. The original 5M Masters and 10M Elite goals remain exploratory.
- Review whether precomputed ECO classification merits a separate derived projection.

Historical benchmarks remain in their original form. Current product behavior is documented in the [documentation index](README.md).
