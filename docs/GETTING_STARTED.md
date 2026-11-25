---
summary: "Build the CLI and sample indexes, start the service, and try position and game search."
read_when: "Setting up Chess Database API for the first time, or needing a working local server to test a client against."
status: current
---

# Getting Started

This guide takes about ten minutes. You will build the command-line tool, create a small local dataset from the sample PGN that ships with the repository, start the HTTP API, then search for a game and retrieve its PGN.

The sample is synthetic test data. Its statistics are for trying the API only. For real datasets, see [section 6](#6-real-datasets).

## 1. Prerequisites

- A current stable Rust toolchain. The release Dockerfile pins a toolchain for reproducible builds. Build with `--locked` so `Cargo.lock` is respected.
- `curl` for the examples. `node` 18 or later or `python3` 3.9 or later is optional, for the client examples.
- Disk space for the build: the sample needs a few megabytes.

## 2. Build the CLI

From the repository root:

```sh
cargo build --locked --release --package chess-database-cli
./target/release/chess-database --help
```

The binary provides the commands used below: `build`, `build-index`, `serve`, `query`, `export`, `verify`, and `inspect`. `chess-database serve --help` lists every server option.

## 3. Create a local sample dataset

The repository ships `fixtures/stage_a_validation.pgn`, 1,000 synthetic games. Build them into an `elite-online` dataset, then build its position index:

```sh
./target/release/chess-database build \
  --input fixtures/stage_a_validation.pgn \
  --collection elite-online \
  --output data/sample/elite-online

./target/release/chess-database build-index \
  --dataset data/sample/elite-online \
  --collection elite-online

./target/release/chess-database build-search-index \
  --dataset data/sample/elite-online
```

`build` applies the Elite Online selection rule and reports how many games it retained. Those games are the sample. `build-index` writes the position index (depth 30 by default for this collection) and publishes it as the dataset's `current` snapshot. `build-search-index` publishes the optional discovery artifacts while reusing the canonical files and CPI. The `data/` directory is ignored by Git.

Do not present a locally built sample as the published Masters OTB dataset. The published Masters release is described in [DATASET_RELEASES.md](DATASET_RELEASES.md); building a corpus from source is covered in [CORPUS_BUILD.md](CORPUS_BUILD.md).

## 4. Start the API

```sh
./target/release/chess-database serve \
  --elite-dataset data/sample/elite-online \
  --host 127.0.0.1 \
  --port 8080
```

The server verifies the dataset before it listens, so the port opens only when the data is ready. Check it from another terminal:

```sh
curl http://127.0.0.1:8080/ready
```

```json
{"status":"ready","collections_loaded":["elite-online"]}
```

## 5. Make the first query

Ask for the position after `1.e4 e5 2.Nf3`:

```sh
curl --get 'http://127.0.0.1:8080/v1/explorer/position' \
  --data-urlencode 'collection=elite-online' \
  --data-urlencode 'moves=e4,e5,Nf3'
```

The response reports the position's game count, its White, draw, and Black results, and the next moves played from it, each with its own counts. A field-by-field description is in [API_REFERENCE.md](API_REFERENCE.md#51-get-v1explorerposition). The same position can be queried by FEN:

```sh
curl --get 'http://127.0.0.1:8080/v1/explorer/position' \
  --data-urlencode 'collection=elite-online' \
  --data-urlencode 'fen=rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2'
```

Both requests return the same `position_key` and statistics, because they describe the same board.

To run a tour of the API, including both query forms, use the bundled script. The sample server loads only Elite Online, so select that collection:

```sh
OPENING_EXPLORER_URL=http://127.0.0.1:8080 \
OPENING_EXPLORER_COLLECTION=elite-online \
  ./examples/curl/quickstart.sh
```

To cross-check the HTTP answer against the CLI, run:

```sh
./target/release/chess-database query \
  --dataset data/sample/elite-online \
  --moves "e4,e5,Nf3" --json
```

The CLI reports the same `total_games`, `white_wins`, `draws`, and `black_wins` as the HTTP response. Its JSON field names differ slightly from the HTTP contract (for example, `total` in each move instead of `total_games`). The HTTP reference is authoritative for integrations.

### Find a game and open its PGN

Check the advertised `game_search` capability, then constrain game discovery to the same board. The response includes matching plies, played continuations and stable game IDs.

```sh
API=http://127.0.0.1:8080
GAMES=$API/v1/collections/elite-online/games
curl --fail-with-body "$API/v1/collections"
curl --fail-with-body --get "$GAMES" \
  --data-urlencode 'moves=e4,e5,Nf3' --data-urlencode 'limit=3'
```

Copy a returned decimal `id` into `GAME_ID`, then run `curl --fail-with-body "$GAMES/$GAME_ID"`. More search parameters and cursor examples are in the [Search API](SEARCH_API.md).

## 6. Real datasets

Real datasets are installed under `data/releases/<collection>/current`. The release packaging, verification, and Docker procedures are documented in [CORPUS_BUILD.md](CORPUS_BUILD.md) and [DOCKER_DEPLOYMENT.md](DOCKER_DEPLOYMENT.md). Serve a release by passing its root directory to `--masters-dataset` or `--elite-dataset`.

The Masters OTB dataset (3,284,429 games, 40-ply index) is available as a [GitHub release](https://github.com/Nerver-zip/opening-explorer-api/releases/tag/dataset-masters-otb-r3); [DATASET_RELEASES.md](DATASET_RELEASES.md#4-download-verify-reconstruct-install) shows how to download, verify, and install it. It is licensed for non-commercial use with attribution.

Each dataset carries its own license. Masters OTB data is for non-commercial use with attribution, and Elite Online is CC0. Before you publish or redistribute a dataset, its PGN, or a service built on it, read [LICENSING.md](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment). Single-game retrieval is enabled by default; it can be switched off (see [API_REFERENCE.md, section 6](API_REFERENCE.md#enabling-and-disabling)).

## 7. Next steps

- [API_REFERENCE.md](API_REFERENCE.md): every endpoint, parameter, field, and error code, plus caching, CORS, and configuration.
- [USE_CASES.md](USE_CASES.md): integration patterns for a chess application, a comparison of collections, an analysis pipeline, and research downloads.
- [`examples/`](../examples/): curl scripts, a dependency-free JavaScript client, and a Python client with a batch exporter.

## Troubleshooting

| Symptom | Likely cause and fix |
| :--- | :--- |
| `Connection refused` right after start | The server binds only after every dataset is verified. Wait for `/ready`. Real releases can take tens of seconds. |
| `collection_unavailable` (503) | The collection was not passed to `serve`. Add `--masters-dataset` or `--elite-dataset`. |
| `invalid_collection` (400) | `collection` is missing or misspelled. Use `masters-otb` or `elite-online`. |
| `invalid_moves` (400) | A move is illegal in its position, or is written in unsupported notation. Check the order and turn, or use UCI (`e2e4`). |
| `total_games` is `0` | The position is rare, or it occurs only beyond the index depth. Compare your move count with `cpi_depth_plies` (see [API_REFERENCE.md, section 5.4](API_REFERENCE.md#54-depth-and-coverage)). |
| Reload request takes a long time | A reload verifies every artifact before it swaps, so large snapshots take a while. The request returns when the reload has finished. |
| `404` with code `not_found` on a game route | Game retrieval is disabled on this server. An unknown ID returns `game_not_found` instead. |
