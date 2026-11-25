---
summary: "Reference for the Opening Explorer HTTP API: endpoints, parameters, response fields, statistics semantics, errors, caching, CORS, admin reload, and server configuration."
read_when: "Integrating a client with the Chess Database API, checking an endpoint, parameter, field, or error code, or interpreting position statistics."
status: current
---

# Chess Database API Reference

This document describes the HTTP API served by `chess-database serve`. It covers the behavior of the current implementation. Where the implementation has a known deviation or limitation, it is stated in [Known limitations](#known-limitations).

For a first request, see [GETTING_STARTED.md](GETTING_STARTED.md). For end-to-end integration patterns, see [USE_CASES.md](USE_CASES.md). Runnable examples live in [`examples/`](../examples/).

The [Search API](SEARCH_API.md) documents discovery, metadata and filtered statistics.

## Contents

1. [Conventions](#1-conventions)
2. [Endpoint summary](#2-endpoint-summary)
3. [Health and readiness](#3-health-and-readiness)
4. [Collections and dataset metadata](#4-collections-and-dataset-metadata)
5. [Position statistics](#5-position-statistics)
6. [Stored game retrieval](#6-stored-game-retrieval)
7. [Administrative reload](#7-administrative-reload)
8. [Errors](#8-errors)
9. [HTTP semantics: caching, CORS, timeouts, limits](#9-http-semantics-caching-cors-timeouts-limits)
10. [Server configuration](#10-server-configuration)
11. [Dataset version identification](#11-dataset-version-identification)
12. [Known limitations](#known-limitations)

---

## 1. Conventions

- **Base URL.** The examples use `http://127.0.0.1:8080`, the default bind address and port. Every data route is under `/v1/`. A breaking change would introduce `/v2/`.
- **Format.** Responses are JSON (`application/json`), except game retrieval, which returns PGN text.
- **Collections.** Every collection-specific request names one collection: `masters-otb` or `elite-online`. Names are case-sensitive. The two collections are separate datasets with separate indexes. The API never combines their counts (see [section 5.5](#55-interpreting-statistics)).
- **Errors.** Errors return a JSON envelope with a stable `code` and a human-readable `message`. Branch on `code`; the message text may change. The full list is in [section 8](#8-errors).
- **Methods.** `GET` and `HEAD` read data. `POST` is used only for `/v1/admin/reload`. Other methods return `405`.

## 2. Endpoint summary

| Method | Path | Purpose | Authentication | `Cache-Control` |
| :--- | :--- | :--- | :--- | :--- |
| `GET` | `/health` | Liveness and software version | none | not set |
| `GET` | `/ready` | Readiness: which collections are loaded | none | not set |
| `GET` | `/v1/collections` | Metadata of each loaded collection | none | `public, max-age=60` |
| `GET` | `/v1/explorer/position` | Statistics and next moves for one position | none | `public, max-age=60` |
| `GET` | `/v1/collections/{collection}/games`, `/players`, and `/events` | Game, player and event discovery on an enabled search snapshot | none | `no-store` |
| `GET` | `/v1/collections/{collection}/games/{id}/metadata` | Stored game summary | none | `no-store` |
| `GET` | `/v1/explorer/position/filtered` | Exact opening statistics with metadata filters | none | `no-store` |
| `GET` | `/v1/collections/{collection}/games/{id}` | One stored game as PGN (only if enabled) | none | `public, max-age=3600` |
| `POST` | `/v1/admin/reload` | Re-verify and swap dataset snapshots | admin token (must be configured) | not set |

## 3. Health and readiness

### `GET /health`

Liveness. Returns `200` while the HTTP listener is running.

```json
{"status":"ok","version":"0.1.0"}
```

| Field | Type | Meaning |
| :--- | :--- | :--- |
| `status` | string | Always `"ok"` when the server answers. |
| `version` | string | Software version of the running server. This is not a dataset version. |

### `GET /ready`

Readiness. The listener only accepts connections after every configured dataset has been opened and verified. A running server therefore reports every configured collection as loaded.

```json
{"status":"ready","collections_loaded":["masters-otb","elite-online"]}
```

| Field | Type | Meaning |
| :--- | :--- | :--- |
| `status` | string | `"ready"` when at least one collection is loaded. |
| `collections_loaded` | string[] | Loaded collections, in the order `masters-otb`, `elite-online`. |

If no collection is loaded, the server answers `503` with `{"status":"not_ready","collections_loaded":[]}`. The current code cannot reach that state after startup, because startup fails instead of binding. Container health checks in this repository use `/ready`.

## 4. Collections and dataset metadata

### `GET /v1/collections`

Returns one object per loaded collection.

Example (values are illustrative; the real output reflects the installed release):

```json
{
  "collections": [
    {
      "collection": "masters-otb",
      "dataset_version": "codb-masters-otb-...-cpi-d40",
      "build_timestamp": "2026-01-01T00:00:00Z",
      "total_games": 3000000,
      "cpi_available": true,
      "cpi_depth_plies": 40,
      "cpi_total_positions": 50000000,
      "format_version": "CGB3/v1"
    }
  ]
}
```

| Field | Type | Meaning |
| :--- | :--- | :--- |
| `collection` | string | `masters-otb` or `elite-online`. |
| `dataset_version` | string | Immutable build identifier of the active snapshot. Treat it as opaque; compare for equality only. |
| `build_timestamp` | string | UTC build time of the snapshot (ISO 8601). |
| `total_games` | integer | Number of games retained in the snapshot. |
| `cpi_available` | boolean | Whether a position index is present. Position queries need it. |
| `cpi_depth_plies` | integer | Present only when `cpi_available` is true. Plies per game covered by the index (see [section 5.4](#54-depth-and-coverage)). |
| `cpi_total_positions` | integer | Present only when `cpi_available` is true. Distinct positions in the index. |
| `format_version` | string | Storage format of the snapshot (for example `CGB3/v1`). |

The additive `capabilities` object describes which features are enabled for this snapshot and operator configuration. See [capability discovery](SEARCH_API.md).

### The two collections

| | `masters-otb` | `elite-online` |
| :--- | :--- | :--- |
| Games | Over-the-board games between strong players, from the publisher archive of the installed release. | Standard rated games from the Lichess rated archives. |
| Rating system | FIDE Elo, as supplied by the source archive. | Lichess Glicko-2 (online platform rating). |
| Selection rule | Specific to each release. The installed snapshot records its rule in `curation_policy` in `manifest.json`. | Both players at least 2200, or one at least 2400 and the other at least 2000. Bullet excluded. At least 6 plies. |
| Index depth | 40 plies in the current release. | 30 plies in the current release. |
| Licensing | Release-specific. See [LICENSING.md](LICENSING.md). | CC0 1.0. |

The legacy `/v1/explorer/position` endpoint reads corpus-wide CPI statistics. [Game search and filtered Explorer](SEARCH_API.md) accept rating ranges on search-enabled snapshots. Use each collection’s own rating scale.

## 5. Position statistics

### 5.1 `GET /v1/explorer/position`

Returns the aggregate results of games that reached a position, and the moves played from it.

#### Parameters

| Parameter | Required | Description |
| :--- | :--- | :--- |
| `collection` | yes | `masters-otb` or `elite-online`. A missing or other value returns `invalid_collection`. |
| `moves` | one of `moves` or `fen` | Move sequence from the initial position. Tokens are separated by commas, spaces, or both. Each token is SAN (`Nf3`, `O-O`, `exd5+`) or UCI (`g1f3`), and SAN and UCI may be mixed. An empty value means the initial position. |
| `fen` | one of `moves` or `fen` | A legal orthodox position in FEN. Chess960 and other variants are not supported. |

Pass exactly one of `moves` and `fen`. Unknown parameters are ignored. The parameters `since_year` and `min_rating` are rejected with `unsupported_parameter` whenever they are present, even when empty, because the position index cannot filter by year or rating.

URL encoding: commas may be sent as `,` or `%2C`. Spaces may be sent as `%20` or `+`. Use `curl --get --data-urlencode` (see [`examples/curl/position.sh`](../examples/curl/position.sh)) to avoid mistakes with FEN slashes and spaces.

Move-sequence length is limited by the server setting `--max-move-plies` (default 200, range 1 to 512). The count is the number of tokens, so 200 tokens is the largest accepted sequence by default.

#### Example

Position after `1.e4 e5 2.Nf3` in `masters-otb`, abbreviated. The counts below come from a synthetic sample dataset and are illustrative. Real values depend on the installed release.

```json
{
  "collection": "masters-otb",
  "dataset_version": "codb-masters-otb-...-cpi-d40",
  "dataset_freshness": "2026-01-01T00:00:00Z",
  "cpi_depth_plies": 40,
  "position_key": "3e88f9327e6dd0a7d3207fec0612d89d",
  "fen": "rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2",
  "total_games": 300,
  "white_wins": 100,
  "draws": 120,
  "black_wins": 80,
  "white_win_pct": 33.3,
  "draw_pct": 40.0,
  "black_win_pct": 26.7,
  "moves": [
    {"uci": "b8c6", "san": "Nc6", "white_wins": 90, "draws": 100, "black_wins": 60,
     "total_games": 250, "white_win_pct": 36.0, "draw_pct": 40.0, "black_win_pct": 24.0},
    {"uci": "g8f6", "san": "Nf6", "white_wins": 10, "draws": 20, "black_wins": 20,
     "total_games": 50, "white_win_pct": 20.0, "draw_pct": 40.0, "black_win_pct": 40.0}
  ]
}
```

#### Response fields

| Field | Type | Meaning |
| :--- | :--- | :--- |
| `collection` | string | Collection that answered. |
| `dataset_version` | string | Build identifier of the snapshot that answered (see [section 11](#11-dataset-version-identification)). |
| `dataset_freshness` | string | UTC build time of that snapshot. Despite its name, this is the build time, not the time the value was fetched. |
| `cpi_depth_plies` | integer | Plies per game covered by this snapshot's position index. Moves beyond this many plies are not indexed (see [section 5.4](#54-depth-and-coverage)). |
| `position_key` | string | 32 lowercase hexadecimal characters: a 128-bit hash of the position (see [section 5.2](#52-position-identity)). |
| `fen` | string | Normalized FEN of the queried position (see [section 5.2](#52-position-identity)). |
| `total_games` | integer | Games that reached this position and continued with a move inside the index depth. Equals `white_wins + draws + black_wins`, and equals the sum of `moves[].total_games`. |
| `white_wins`, `draws`, `black_wins` | integer | Games with that result among `total_games`. |
| `white_win_pct`, `draw_pct`, `black_win_pct` | number | Percentages of `total_games`, rounded to one decimal place. `0.0` when `total_games` is `0`. |
| `moves` | array | Candidate next moves observed from this position. Empty when `total_games` is `0`. |

Each `moves` entry contains:

| Field | Type | Meaning |
| :--- | :--- | :--- |
| `uci` | string | Move in UCI notation (for example `e2e4`, `e7e8q`). |
| `san` | string | The same move in SAN, in the context of this position. |
| `white_wins`, `draws`, `black_wins` | integer | Results of games in which this move was played from this position. |
| `total_games` | integer | `white_wins + draws + black_wins` for this move. |
| `white_win_pct`, `draw_pct`, `black_win_pct` | number | Percentages of this move's `total_games`, rounded to one decimal place. |

`moves` is sorted by `total_games` descending, then by `uci` ascending. All observed candidates are returned; the list is not truncated.

### 5.2 Position identity

- `position_key` is a 128-bit hash of the board, the side to move, castling rights, and the en passant square when a legal en passant capture exists. Move counters are not part of it. Two move orders that reach the same board therefore share one key and one set of statistics. The key is the same in both collections, because it describes the board only.
- `fen` is the normalized FEN of the queried position. Its move counters come from the move sequence, or from the supplied FEN. Input FENs that omit the counters receive `0 1`.
- The ETag (section 9) includes `fen`. Two requests for the same board with different counters have the same statistics but different ETags.

### 5.3 Next-move statistics

`moves` lists every continuation that a retained game played from this position inside the index depth. Each entry carries the W/D/B results of only those games that played that move. Summing `moves[].total_games` gives the position's `total_games`.

### 5.4 Depth and coverage

Only the first `cpi_depth_plies` plies of each game are indexed. Counting starts at the game's own starting position. The index stores the position before each move that falls inside the depth window. A position reached only after the depth limit has no stored continuation.

Measured on the current releases with the greedy line that follows the most-played reply at each step:

| Collection | Depth | Plies played | `total_games` |
| :--- | ---: | ---: | ---: |
| `masters-otb` | 40 | 39 | nonzero (continuations stored) |
| `masters-otb` | 40 | 40 | `0`, `moves: []` |
| `elite-online` | 30 | 29 | nonzero (continuations stored) |
| `elite-online` | 30 | 30 | `0`, `moves: []` |

A `total_games` of `0` means that no retained game reached this position with a continuation inside the index window. This can happen because the position is rare or unreachable in the dataset, or because every game that reaches it does so beyond the depth limit. The response does not distinguish these cases. Compare the length of your move list with the `cpi_depth_plies` value in the same response before drawing a conclusion. A position can also appear inside the window through a different move order, in which case it has statistics.

Each game counts at most once per position. A repetition inside one game does not add a second count.

Games whose result is `*` (unknown) are excluded from the index. The current releases record no such games.

### 5.5 Interpreting statistics

- **Results are game results, not scores for each side.** `white_wins` counts games that ended `1-0`. `black_wins` counts games that ended `0-1`. `draws` counts `1/2-1/2`.
- **Percentages describe the sample.** A high percentage on a small `total_games` is weak evidence. Show the counts next to the percentages, and consider a minimum sample size before you rank moves.
- **Results are conditional on reaching the position.** The results describe games that reached the position under the source archive's selection rule. They do not describe the strength of either move in general.
- **Compare collections side by side.** Masters OTB ratings are FIDE Elo values. Elite Online ratings are Lichess Glicko-2 values. The two systems have different pools and inflation, so a 2500 in one is not a 2500 in the other. Show each collection's numbers with its own label and never combine them. [`examples/js/compare-collections.mjs`](../examples/js/compare-collections.mjs) does this.
- **Coverage follows the index.** Moves after the depth limit are not represented. Each response carries `cpi_depth_plies`, so a client can decide whether a missing result is informative.

## 6. Stored game retrieval

### `GET /v1/collections/{collection}/games/{id}`

Returns one stored game as PGN.

- **Path.** `collection` is `masters-otb` or `elite-online`. `id` is the stable game ID: a decimal integer from `0` to `18446744073709551615`. The server also accepts a leading `+` and leading zeros. Negative numbers, whitespace, exponents, and values above the range return `invalid_game_id`. The bundled clients are stricter and send only plain digits.
- **Success.** `200` with `Content-Type: application/x-chess-pgn; charset=utf-8`, an `ETag`, and `Cache-Control: public, max-age=3600`. The body is the stored game's PGN, including the stored tags.
- **Not found.** `404` with `game_not_found` when the ID is not in that collection. An ID from one collection returns `404` in the other.
- **Conditional requests.** `If-None-Match` with a matching ETag returns `304` with an empty body. Existence is checked first, so an unknown ID never returns `304`.
- **Collection not loaded.** `503` with `collection_unavailable`.

### Enabling and disabling

Retrieval is enabled by default. Start the server with `--enable-game-retrieval false` (or the environment variable in [section 10](#10-server-configuration)) to remove the route. When disabled:

- the route does not exist, so the server answers `404` with the error code `not_found`, the same as any unknown path;
- position queries and collection metadata keep working.

The setting applies to the whole process and to both collections; there is no per-collection switch. Single-game retrieval is a supported capability of public deployments under the project's current policy ([LICENSING.md section 6](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment)), which also gives the required attribution for Masters OTB data and states that commercial use is not covered. Operators may still disable the route (for example, for a statistics-only service). The API has no bulk-export endpoint.

### Finding game IDs

Use [game search](SEARCH_API.md) with metadata or a board position to discover stable game IDs, represented as decimal strings. The legacy position response contains aggregate statistics. On CPI-only snapshots, use identities already known to your client or the release's [lookup index](DATA_CONTRACTS.md#23-global-game-lookup-gamesidx).

Example: [`examples/curl/game.sh`](../examples/curl/game.sh).

## 7. Administrative reload

### `POST /v1/admin/reload`

Re-opens and re-verifies the configured dataset paths, then atomically swaps in the new snapshots. Requests in flight keep their pinned snapshot. If verification fails, the active snapshots remain in service.

**Enabling.** The endpoint is available only when the server was started with an admin token (`--admin-token`, or `OPENING_EXPLORER_ADMIN_TOKEN`). Without a token it always returns `403` with `admin_disabled`.

**Authentication.** Send the token in either header:

- `Authorization: Bearer <token>`
- `X-Admin-Token: <token>`

A missing or wrong token returns `401` with `unauthorized`.

**Browser requests.** A request that carries an `Origin` header is refused with `401`, even with a valid token. The endpoint is for operators and scripts, not for web pages.

**Success.** `200`:

```json
{
  "status": "ok",
  "reloaded_collections": [
    "masters-otb (version: codb-masters-otb-...-cpi-d40)",
    "elite-online (version: codb-elite-online-...-cpi-d30)"
  ]
}
```

**Failure.** `500` with `reload_failed`. The message is generic; the detail is in the server log. The previous snapshots stay active.

**Duration.** The reload is not subject to the request timeout that applies to read endpoints. Verifying a large snapshot can take a long time, and the response arrives when the reload finishes. The reload runs in its own task, so it completes and swaps atomically even if the caller stops waiting. Concurrent reloads are serialized. Set your HTTP client's timeout to suit the snapshot size. In one measurement on the verification host, with production-size snapshots (about 2.9 GB for both collections), the reload returned `200` after about 20 seconds.

Signals remain available. `SERVER_PID` is the process ID of the running `chess-database serve`:

```sh
kill -HUP "$SERVER_PID"
```

SIGHUP runs the same validation and swap. Progress and the result are written to the server log. Example: [`examples/curl/reload.sh`](../examples/curl/reload.sh).

## 8. Errors

Errors from the API handlers use this envelope:

```json
{"error":{"code":"invalid_moves","message":"Invalid or illegal move sequence: Invalid move token 'e4': invalid uci"}}
```

| Code | HTTP status | When it occurs |
| :--- | ---: | :--- |
| `invalid_collection` | 400 | `collection` is missing or is not `masters-otb` / `elite-online`. |
| `missing_position` | 400 | Neither `moves` nor `fen` was given. |
| `ambiguous_position` | 400 | Both `moves` and `fen` were given. |
| `invalid_fen` | 400 | The FEN cannot be parsed or describes an illegal position. |
| `invalid_moves` | 400 | A move is illegal in its position, or a token is neither SAN nor UCI. The message names the token and its ply. |
| `invalid_query` | 400 | The request could not be decoded: a repeated query parameter (for example `moves=e4&moves=d4`), or invalid percent-encoding in the query string or path. |
| `move_sequence_too_long` | 400 | More tokens than `--max-move-plies`. |
| `unsupported_parameter` | 400 | `since_year` or `min_rating` was supplied. |
| `invalid_game_id` | 400 | The game ID is not a decimal 64-bit unsigned integer. |
| `unauthorized` | 401 | Missing or invalid admin token, or a request with an `Origin` header to the admin route. |
| `admin_disabled` | 403 | Admin reload was requested but no admin token is configured. |
| `game_not_found` | 404 | The game ID is not in the collection. |
| `not_found` | 404 | No route matches the path, or the game route is disabled. |
| `method_not_allowed` | 405 | The HTTP method is not supported for the path. The `Allow` header lists the supported methods. |
| `request_timeout` | 408 | A read request exceeded `--request-timeout-secs`. Retry, or narrow the request. |
| `collection_unavailable` | 503 | The collection is not loaded on this server. |
| `index_unavailable` | 503 | The snapshot has no position index. |
| `reader_pool_timeout` | 503 | No game reader became free within `--reader-pool-timeout-secs`. Retry later. |
| `reload_failed` | 500 | The replacement snapshot failed verification. |
| `storage_error` | 500 | A storage read or decode failed. |
| `internal_error` | 500 | An unexpected server error. |
| `invalid_cursor` | 400 | The cursor token is malformed or belongs to a different query. |
| `search_disabled` | 403 | The requested search capability is disabled for the collection. |
| `stale_cursor` | 409 | The search cursor or requested catalog version belongs to a previous snapshot. |
| `search_timeout` | 408 | Search exceeded its execution budget. Narrow the query. |
| `search_work_limit` | 422 | The request exceeds the search work limit. Narrow the query. |
| `search_busy` | 429 | All search workers are occupied. |
| `search_unavailable` | 503 | The loaded snapshot lacks the index or required coverage. |

Every error response uses this envelope, including responses the router produces before a handler runs (unknown paths, wrong methods, undecodable query strings, and timeouts). Those keep their HTTP status codes.

Messages for `500` responses are generic. The details are in the server log, and a `500` never includes a filesystem path or engine detail. Messages for `4xx` and `503` responses describe the condition and are returned as written.

Clients should still handle a non-JSON error body, for example one returned by a proxy in front of the server, as `http_error` with the status code.

## 9. HTTP semantics: caching, CORS, timeouts, limits

### Caching and conditional requests

- **ETags.** Position responses carry `ETag: "<collection>-<dataset_version>-<position_key>-<hash>"`, where the hash covers the normalized FEN. The dataset version is part of the tag, so a new snapshot invalidates cached entries. Game responses carry `"<collection>-<dataset_version>-<id>"`.
- **Conditional requests.** `If-None-Match` is supported with `*`, weak `W/` prefixes, and comma-separated lists. A match returns `304` with the same `ETag` and `Cache-Control`.
- **`Cache-Control`.** Positions and collections: `public, max-age=60`. Games: `public, max-age=3600`. Other routes set no cache directive.

[`examples/js/opening-explorer.mjs`](../examples/js/opening-explorer.mjs) uses these headers to reuse position responses.

### CORS

The default is restrictive: no cross-origin browser origin is allowed. A foreign origin receives no `Access-Control-Allow-Origin` header, so the browser refuses to expose the response to that page.

- `--allowed-origins "https://app.example,http://localhost:3000"` allows exact origins (scheme, host, and port).
- `--allowed-origins "*"` allows any origin for read requests. Admin reload still refuses requests that carry an `Origin` header.

Allowed methods are `GET`, `HEAD`, `POST`, and `OPTIONS`. Allowed request headers are `Content-Type`, `Accept`, `Authorization`, `If-None-Match`, and `If-Match`. Exposed response headers are `ETag`, `Content-Type`, and `Cache-Control`.

### Timeouts and limits

| Limit | Default | Bounds | Behavior when exceeded |
| :--- | ---: | :--- | :--- |
| Request timeout (`--request-timeout-secs`) | 15 s | 1 to 300 | Read endpoints only. Exceeding it returns `408 request_timeout`. The admin reload is exempt (section 7). |
| Move tokens (`--max-move-plies`) | 200 | 1 to 512 | `400 move_sequence_too_long`. |
| Game reader pool per collection (`--reader-pool-size`) | 16 | 1 to 64 | Game retrieval waits. |
| Wait for a reader (`--reader-pool-timeout-secs`) | 5 s | 1 to 60 | `503 reader_pool_timeout`. |

Position queries do not use the reader pool. They read memory-mapped index pages without locks, so they are not limited by it. The application sets no URL length or body size limit beyond the defaults of the HTTP stack.

## 10. Server configuration

Run `chess-database serve --help` for the flags. `CHESS_DATABASE_*` takes precedence over the listed `OPENING_EXPLORER_*` and applicable `CODB_*` compatibility names. See the [search settings](SEARCH_API.md) and [naming migration](NAMING_MIGRATION.md).

| Flag | Environment variable | Default | Notes |
| :--- | :--- | :--- | :--- |
| `--masters-dataset <DIR>` | `OPENING_EXPLORER_MASTERS_DATASET` | none | Dataset root (contains `current/`). |
| `--elite-dataset <DIR>` | `OPENING_EXPLORER_ELITE_DATASET` | none | Dataset root. At least one dataset is required. |
| `--host <ADDR>` | `OPENING_EXPLORER_HOST` | `127.0.0.1` | Use `0.0.0.0` inside containers. |
| `--port <PORT>` (`-p`) | `OPENING_EXPLORER_PORT` | `8080` | |
| `--allowed-origins <LIST>` | `OPENING_EXPLORER_ALLOWED_ORIGINS` | empty | Comma-separated, or `*`. See [section 9](#cors). |
| `--request-timeout-secs <N>` | `OPENING_EXPLORER_REQUEST_TIMEOUT_SECS` | `15` | 1 to 300. Read endpoints only. |
| `--max-move-plies <N>` | `OPENING_EXPLORER_MAX_MOVE_PLIES` | `200` | 1 to 512. |
| `--reader-pool-size <N>` | `OPENING_EXPLORER_READER_POOL_SIZE` | `16` | 1 to 64. |
| `--reader-pool-timeout-secs <N>` | `OPENING_EXPLORER_READER_POOL_TIMEOUT_SECS` | `5` | 1 to 60. |
| `--enable-game-retrieval <true\|false>` | `OPENING_EXPLORER_ENABLE_GAME_RETRIEVAL` | `true` | `false` removes the game route for both collections. |
| `--admin-token <TOKEN>` | `OPENING_EXPLORER_ADMIN_TOKEN` | none | Enables `POST /v1/admin/reload`. Keep it secret. |
| `--deep-validate-on-startup` | none (flag only) | off | Also decodes and checks every stored block before readiness. Startup always verifies the checksums of all artifacts. |

Log verbosity is controlled by `RUST_LOG` (for example `RUST_LOG=info`). `SIGHUP` triggers a reload, and `SIGINT` or `SIGTERM` stops the server gracefully.

Startup fails with a message if a configured dataset is missing, belongs to another collection, or fails verification.

## 11. Dataset version identification

- **`dataset_version`** identifies the immutable snapshot that answered a request. It appears in position responses, `/v1/collections`, and the reload response. It is opaque. Use it for equality checks, logging, and cache keys.
- **`dataset_freshness` and `build_timestamp`** give the UTC build time of the snapshot.
- **ETags** embed the dataset version, so a reload invalidates caches automatically.
- **`/health.version`** is the software version. It does not change when a dataset changes.
- Build identifiers keep their historical `codb-` prefix. They are immutable release identifiers, so the prefix is not renamed.

## Known limitations

These behaviors are part of the current implementation. Each one is recorded so that clients can work around it.

1. **Game retrieval is a single process-wide switch.** Masters and Elite cannot differ within one server. Run a separate instance, or place a proxy in front, if one collection's PGN must stay private.
2. **PGN export omits stable IDs.** Use a game search result’s `id` when identifying a game through the API.
3. **`/ready` cannot report `not_ready` after startup.** Startup validates every configured dataset before binding, so readiness is effectively fixed for the life of the process.
