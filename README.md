# Chess Database API

Chess Database API is a self-hosted, read-only service for discovering curated games by player, event, rating, date and position. Opening Explorer serves precomputed position statistics, and optional search indexes add paginated game summaries and exact filtered opening statistics. Retrieve individual games as canonical PGN.

The Rust service uses immutable snapshots of separately curated and deduplicated collections.

> **Availability.** Run the service yourself using the [Masters OTB R3](https://github.com/Nerver-zip/opening-explorer-api/releases/tag/dataset-masters-otb-r3) and [Elite Online R1](https://github.com/Nerver-zip/opening-explorer-api/releases/tag/dataset-elite-online-r1) releases. Each release contains an indexed dataset and a filtered PGN. [Dataset releases](docs/DATASET_RELEASES.md) explains installation and licenses.

## Who it is for

- Developers building an **opening explorer** or opening trainer into a chess app.
- Analysts who want **opening statistics in a pipeline** (batch queries, CSV export).
- Researchers who want **curated, legal, deduplicated PGN** to work with.

## Collections

Two collections are kept strictly apart. Their ratings and statistics are never pooled.

| | `masters-otb` | `elite-online` |
| :--- | :--- | :--- |
| Games | 3,284,429 | 429,150 |
| Source | Lumbra's Gigabase OTB Complete (publisher-asserted over-the-board) | Lichess rated archive |
| Rating scale | Publisher-supplied Elo (FIDE-style) | Lichess platform rating |
| Selection | Standard legal chess, at least four plies, rating thresholds | Both players 2200+, or one 2400+ and the other 2000+; no bullet; at least six plies |
| Indexed depth | 40 plies | 30 plies |
| Dataset license | CC BY-NC 4.0 (publisher claim); non-commercial, attribution required | CC0 1.0 |

Elite Online covers seven selected monthly archives from September 2015 through March 2017. It is historical data, not a feed of contemporary games. Its player ratings are Lichess Glicko-2 ratings and are not comparable to the Masters collection's FIDE-style Elo values.

Do not compare an OTB Elo with an online rating as if they were one scale. Details: [API reference, interpreting statistics](docs/API_REFERENCE.md).

## What the API provides

| Endpoint | Purpose |
| :--- | :--- |
| `GET /v1/explorer/position` | Position statistics from `moves=` (SAN or UCI) or `fen=`: total games, W/D/L, next moves, each with W/D/L |
| `GET /v1/collections` | Loaded collections, snapshot versions and capabilities |
| `GET /v1/collections/{collection}/games` | Paginated game discovery by metadata, position or continuation |
| `GET /v1/collections/{collection}/players` and `/events` | Player and event discovery |
| `GET /v1/explorer/position/filtered` | Exact opening statistics for a subset of one collection |
| `GET /v1/collections/{collection}/games/{id}` | Canonical PGN of one stored game (enabled by default) |
| `GET /health`, `GET /ready` | Liveness and readiness |
| `POST /v1/admin/reload` | Token-protected hot reload of a newly published dataset |

See the [API reference](docs/API_REFERENCE.md) and [search contract](docs/SEARCH_API.md).

## Quick start

Build the CLI (`chess-database`), make a small sample dataset from the bundled synthetic fixture, serve it, and query it:

```bash
cargo build --locked --release --package opening-explorer-cli

./target/release/chess-database build \
  --input fixtures/stage_a_validation.pgn --collection elite-online \
  --output data/sample/elite-online
./target/release/chess-database build-index \
  --dataset data/sample/elite-online --collection elite-online

./target/release/chess-database build-search-index \
  --dataset data/sample/elite-online

./target/release/chess-database serve \
  --elite-dataset data/sample/elite-online --host 127.0.0.1 --port 8080
```

In another terminal, ask for the position after `1.e4 e5 2.Nf3`:

```bash
curl --get 'http://127.0.0.1:8080/v1/explorer/position' \
  --data-urlencode 'collection=elite-online' \
  --data-urlencode 'moves=e4,e5,Nf3'
```

The result lists `total_games`, `white_wins`, `draws`, `black_wins`, and a `moves` array with the same counts per candidate move. The sample is synthetic test data. [Getting started](docs/GETTING_STARTED.md) walks through it step by step.

### With Docker

```bash
cp .env.example .env          # point it at your dataset directory
docker compose up -d --build
curl http://127.0.0.1:8080/ready
```

See [Deployment](docs/DOCKER_DEPLOYMENT.md) for read-only mounts, resource bounds, and operational settings.

## Get the datasets

Both collection releases use two products, and the indexed and PGN products for one collection contain the same canonical games. Both indexed releases can be enriched locally with [derived search indexes](docs/SEARCH_INDEX.md).

| Collection | Indexed dataset | Filtered PGN | Coverage and license |
| :--- | :--- | :--- | :--- |
| Masters OTB R3 | [Published release](https://github.com/Nerver-zip/opening-explorer-api/releases/tag/dataset-masters-otb-r3), 3,284,429 games, CPI depth 40 | Zstandard-compressed PGN in the same release | CC BY-NC 4.0 publisher claim; non-commercial use with attribution. See the [current policy](docs/LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment). |
| Elite Online R1 | [Published release](https://github.com/Nerver-zip/opening-explorer-api/releases/tag/dataset-elite-online-r1), CPI depth 30 | One complete canonical game per PGN record, Zstandard-compressed | 429,150 games from selected Lichess rated archives in 2015–2017; CC0 1.0. Commercial redistribution is permitted under CC0. |

The indexed package contains the immutable snapshot, position index, release metadata, and checksums. The filtered PGN preserves supported stored metadata and full legal mainlines. Masters R3's indexed package is split into two numbered assets; the Elite package is split only if it exceeds the per-asset limit. See [Dataset releases](docs/DATASET_RELEASES.md) for download links, checksum verification, reconstruction, installation, PGN extraction, and versioning. To reproduce Elite Online from the pinned sources, follow [Corpus build](docs/CORPUS_BUILD.md).

## Use cases

Worked examples are in [docs/USE_CASES.md](docs/USE_CASES.md), with runnable code in [`examples/`](examples/) (curl, dependency-free JavaScript, Python):

1. **Opening explorer in a chess app**: query on every move and render next-move bars.
2. **Masters vs. Elite comparison**: query both collections for one position and show them side by side, never merged.
3. **Next-move frequencies and W/D/L** for a position.
4. **Analysis pipeline**: batch positions through the API and export CSV.
5. **Curated PGN for research**: use the filtered PGN download.

A minimal JavaScript client:

```js
import { OpeningExplorer } from "./examples/js/opening-explorer.mjs";

const explorer = new OpeningExplorer("http://127.0.0.1:8080");
const result = await explorer.position({ collection: "elite-online", moves: ["e4", "e5", "Nf3"] });
console.log(result.total_games, result.moves[0]);
```

## Documentation

| Need | Read |
| :--- | :--- |
| First run | [Getting started](docs/GETTING_STARTED.md) |
| Endpoints, errors, config | [API reference](docs/API_REFERENCE.md) |
| Downloading and installing datasets | [Dataset releases](docs/DATASET_RELEASES.md) |
| Running it in production | [Deployment](docs/DOCKER_DEPLOYMENT.md) |
| Building from source and rebuilding corpora | [Corpus build](docs/CORPUS_BUILD.md), [Contributing](CONTRIBUTING.md) |
| Data and licensing | [Licensing and attribution](docs/LICENSING.md) |
| Internals and formats | [Architecture](docs/ARCHITECTURE.md), [Data contracts](docs/DATA_CONTRACTS.md) |
| Benchmarks and research | [Benchmarks](docs/BENCHMARKS.md), [research archive](docs/research/) |
| Upgrading CLI, image or configuration names | [Naming migration](docs/NAMING_MIGRATION.md) |

The complete index is [docs/README.md](docs/README.md).

## Development

```bash
./scripts/validate.sh   # format, clippy, tests, evidence and documentation gates
```

Contribution rules are in [CONTRIBUTING.md](CONTRIBUTING.md); agent workflow notes are in [AGENTS.md](AGENTS.md).

## Licenses

- **Software:** [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. These licenses do not apply to the datasets.
- **Masters OTB data:** games from [Lumbra's Gigabase](https://lumbrasgigabase.com/), which the publisher offers under [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/). This project filtered, deduplicated, normalized, and indexed them; it claims no authorship of any game and is not endorsed by the publisher. **Non-commercial use only, with attribution.** Public, non-commercial deployment of the API (position statistics and single-game PGN retrieval) is supported under the maintainer's recorded decisions; commercial use is not covered. The terms for your application, the required acknowledgment, and the open questions are in [docs/LICENSING.md](docs/LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment).
- **Elite Online data:** selected standard rated archives from [Lichess](https://database.lichess.org/standard/), licensed under [CC0 1.0 Universal](https://creativecommons.org/publicdomain/zero/1.0/). Opening Explorer filters, deduplicates, normalizes, and indexes these historical games; the data license is separate from the software license. See [LICENSING.md](docs/LICENSING.md) for source periods and details.

Attribution text, changes made, and the open upstream-rights questions: [docs/LICENSING.md](docs/LICENSING.md).
