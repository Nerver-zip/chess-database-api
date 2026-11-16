# Opening Explorer API

A self-hosted, read-only HTTP API for chess opening statistics. Give it a position (as a move sequence or a FEN) and it returns how many games reached it, the White / draw / Black results, and the moves that were played next, with their own results.

Opening Explorer API curates and deduplicates large game collections, stores them compactly, indexes every position, and serves the statistics from a small Rust service. It is written independently and shares no code with the AGPL-licensed Lichess opening explorer.

> **Availability.** There is no hosted public instance. You run the service yourself. The Masters OTB dataset assets are prepared but **not yet published**: public release is gated on a documented licensing review ([Dataset releases](docs/DATASET_RELEASES.md#6-distribution-gate)).

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

Do not compare an OTB Elo with an online rating as if they were one scale. Details: [API reference, interpreting statistics](docs/API_REFERENCE.md).

## What the API provides

| Endpoint | Purpose |
| :--- | :--- |
| `GET /v1/explorer/position` | Position statistics from `moves=` (SAN or UCI) or `fen=`: total games, W/D/L, next moves, each with W/D/L |
| `GET /v1/collections` | Loaded collections and their dataset versions |
| `GET /v1/collections/{collection}/games/{id}` | Canonical PGN of one stored game (can be disabled per deployment) |
| `GET /health`, `GET /ready` | Liveness and readiness |
| `POST /v1/admin/reload` | Token-protected hot reload of a newly published dataset |

Full contract, errors, caching, and configuration: [docs/API_REFERENCE.md](docs/API_REFERENCE.md).

## Quick start

Build the CLI (`opening-explorer`), make a small sample dataset from the bundled synthetic fixture, serve it, and query it:

```bash
cargo build --locked --release --package opening-explorer-cli

./target/release/opening-explorer build \
  --input fixtures/stage_a_validation.pgn --collection elite-online \
  --output data/sample/elite-online
./target/release/opening-explorer build-index \
  --dataset data/sample/elite-online --collection elite-online

./target/release/opening-explorer serve \
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

See [Deployment](docs/DOCKER_DEPLOYMENT.md) for read-only mounts, resource bounds, and the restricted (retrieval disabled) mode.

## Get the datasets

Two products are prepared, both containing the same 3,284,429 curated Masters OTB games:

| Product | What you get |
| :--- | :--- |
| **Indexed dataset** | Ready-to-serve game storage, position index, metadata, notice, checksums |
| **Filtered PGN** | One PGN per canonical game, Zstandard-compressed |

Once a dataset release is published, the assets are large files on GitHub Releases, split into numbered parts with per-part SHA-256 checksums and a machine-readable manifest. Download, reconstruction, verification, and installation steps are in [docs/DATASET_RELEASES.md](docs/DATASET_RELEASES.md). Elite Online can be rebuilt from pinned Lichess sources ([Corpus build](docs/CORPUS_BUILD.md)).

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
| Upgrading from the old `codb` names | [Migration](docs/MIGRATION_FROM_CODB.md) |

The complete index is [docs/README.md](docs/README.md).

## Development

```bash
./scripts/validate.sh   # format, clippy, tests, evidence and documentation gates
```

Contribution rules are in [CONTRIBUTING.md](CONTRIBUTING.md); agent workflow notes are in [AGENTS.md](AGENTS.md).

## Licenses

- **Software:** [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. These licenses do not apply to the datasets.
- **Masters OTB data:** games from [Lumbra's Gigabase](https://lumbrasgigabase.com/), which the publisher offers under [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/). This project filtered, deduplicated, normalized, and indexed them; it claims no authorship of any game and is not endorsed by the publisher. Non-commercial use only, with attribution.
- **Elite Online data:** CC0 1.0 (Lichess rated games).

Attribution text, changes made, and the open upstream-rights questions: [docs/LICENSING.md](docs/LICENSING.md).
