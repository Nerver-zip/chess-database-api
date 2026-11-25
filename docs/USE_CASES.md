---
summary: "Worked examples for game discovery, filtered opening statistics, collection comparisons and PGN retrieval."
read_when: "Deciding how to use the Chess Database API in a product, research workflow, or data pipeline, or looking for a worked example."
status: current
---

# Use Cases

Each use case names the endpoints it needs, shows a short pattern, and points to a runnable example. The [API reference](API_REFERENCE.md) defines every field and error code. The examples assume a server at `OPENING_EXPLORER_URL`, which defaults to `http://127.0.0.1:8080`.

## 1. An opening explorer for a chess application

**Goal.** Show, for the position on the board, how often each next move was played and what happened in those games.

**Endpoints.** `GET /v1/explorer/position`. Optionally `GET /v1/collections` at startup to read the loaded depth.

**Pattern.**

1. Keep the game's move list in SAN or UCI. Send the whole list as `moves`, from the initial position, on every move. The server does not need the previous answer.
2. Show the candidates from `moves[]`, most played first. Show the counts next to the percentages.
3. Treat `total_games: 0` as "no data here", not as a refutation of the move. Explain the depth limit if the list is longer than `cpi_depth_plies`.
4. Cache responses by `position_key` and the `ETag`. The client in [`examples/js/opening-explorer.mjs`](../examples/js/opening-explorer.mjs) revalidates with `If-None-Match` and reuses the body on `304`.
5. Keep the server behind your backend in production. A browser can call the API directly only when its origin is listed in `--allowed-origins`.

```js
import { OpeningExplorerClient, OpeningExplorerError } from './examples/js/opening-explorer.mjs';

const explorer = new OpeningExplorerClient({ baseUrl: process.env.OPENING_EXPLORER_URL });

export async function openingStatsFor(playedMoves) {
  try {
    return await explorer.position({ collection: 'masters-otb', moves: playedMoves });
  } catch (error) {
    // An illegal or malformed move list is a client bug. Report it and show nothing.
    if (error instanceof OpeningExplorerError && error.code === 'invalid_moves') return null;
    throw error;
  }
}
```

**Caveats.** Reaching the same board by a different move order returns the same statistics, which is correct for an explorer. The ETag changes when the clocks in the FEN change, even though the statistics do not.

## 2. Comparing Masters OTB and Elite Online

**Goal.** Show how the same position looks among over-the-board masters and among high-rated online players.

**Endpoints.** `GET /v1/explorer/position` once per collection, and `GET /v1/collections` to see which collections are loaded.

**Pattern.** Query each collection separately and display the two results side by side, each labeled with its collection and depth. Run [`examples/js/compare-collections.mjs`](../examples/js/compare-collections.mjs):

```sh
OPENING_EXPLORER_URL=http://127.0.0.1:8080 node examples/js/compare-collections.mjs "d4,d5,c4"
```

The output is one row per collection:

```text
collection     games  White%  Draw%  Black%  top reply (games)
masters-otb    <games>  <w>     <d>     <b>   <san> (<n>)
elite-online   <games>  <w>     <d>     <b>   <san> (<n>)
```

**Rules for interpreting the comparison.**

- Do not add the two collections' game counts into one total. Each row is its own evidence.
- Do not average or mix FIDE Elo with Lichess Glicko-2. The collections are selected with different rating systems, and their numbers are not on the same scale. See [section 4 of the API reference](API_REFERENCE.md#the-two-collections).
- Differences in percentages can reflect the different player pools (over-the-board events versus online play), not only different opening preferences. Say so when you present them.
- If one collection is not loaded, the server answers `collection_unavailable`. Show "not loaded" for that row rather than hiding it.

## 3. Next-move frequencies and White, draw, and Black results

**Goal.** For one position, list the next moves with how often each was played and the White, draw, and Black results of those games.

**Endpoints.** `GET /v1/explorer/position`, with either `moves` or `fen`.

```sh
curl --get 'http://127.0.0.1:8080/v1/explorer/position' \
  --data-urlencode 'collection=elite-online' \
  --data-urlencode 'fen=rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2'
```

**Reading the response.**

- `total_games` is the number of games that reached this position and continued with a move inside the index depth.
- For each candidate, `moves[].total_games` is its share of those games. Its share of the position is `moves[].total_games / total_games`.
- `white_wins`, `draws`, and `black_wins` are game results. They do not score each side separately.

A common display converts the results into a single expected score for White. This is an application-level formula, not a field the API returns:

```text
white_expected_score = (white_wins + 0.5 × draws) / total_games
```

Show the score together with `total_games`. A 60% score from 12 games says much less than the same score from 12,000 games.

## 4. Integrating statistics into an analysis pipeline

**Goal.** Attach opening statistics to a large set of positions, such as the openings in your own games or an opening repertoire file.

**Endpoints.** `GET /v1/explorer/position` and `GET /v1/collections`.

**Pattern.** Use [`examples/python/opening_explorer.py`](../examples/python/opening_explorer.py), which needs only the Python standard library. Put one move list per line in a text file (blank lines and lines starting with `#` are ignored), then run:

```sh
python3 examples/python/opening_explorer.py batch \
  --collection elite-online \
  --input openings.txt \
  --output stats.csv
```

The CSV contains one row per distinct `position_key`: the input move list, the normalized FEN, the position's W/D/B counts and percentages, the top candidate and its game count, and the `dataset_version` that answered.

**Practices that keep the results trustworthy.**

- Store `dataset_version` with every result. A later reload can change the numbers, and the stored version lets you reproduce or compare runs.
- Record `cpi_depth_plies` for each collection from `/v1/collections`. Flag each zero result as "no continuation inside the depth window" or "absent", based on the move-list length.
- Deduplicate by `position_key`, not by move text. Different move orders that reach the same board have identical statistics.
- Limit concurrency. The server has no rate limiter. Each request is bounded by the server's request timeout (15 seconds by default). Retry `408` and `503` responses with backoff.
- Do not pool the two collections into one statistic. Keep the collection in every row.

## 5. Downloading curated PGN for research or tooling

**Goal.** Obtain the games behind the statistics, for independent analysis or for tools that read PGN.

There are two routes. Check the license of the dataset before you use either (see [LICENSING.md](LICENSING.md)).

**A. Bulk export from a dataset.** The `export` command writes every stored game as PGN to a file or stdout. A `.zst` output path is Zstandard-compressed. `--max-games` limits the output:

```sh
./target/release/chess-database export \
  --dataset data/releases/elite-online/current \
  --output elite-online.pgn.zst
```

Exported PGN does not contain the stable game IDs used by the HTTP route. Keep the dataset's `RELEASE/` notices with any export you share.

**B. One game at a time over HTTP.** When the server has game retrieval enabled, `GET /v1/collections/{collection}/games/{id}` returns one game as PGN. The ID must come from the release's lookup index, because the API does not list games. Use [`examples/curl/game.sh`](../examples/curl/game.sh):

```sh
OPENING_EXPLORER_URL=http://127.0.0.1:8080 \
  ./examples/curl/game.sh elite-online 27289182012770 game.pgn
```

**C. The published Masters OTB PGN.** The filtered PGN of all 3,284,429 Masters OTB games is a release asset (`masters-otb-r3-filtered.pgn.zst`, about 500 MB). Download it from the [`dataset-masters-otb-r3` release](https://github.com/Nerver-zip/opening-explorer-api/releases/tag/dataset-masters-otb-r3) and verify it against `SHA256SUMS` as described in [DATASET_RELEASES.md](DATASET_RELEASES.md#4-download-verify-reconstruct-install). It is for non-commercial use with attribution, and the games carry the tag set described in [section 5 of that guide](DATASET_RELEASES.md#5-filtered-pgn).

**Terms.** Single-game retrieval is enabled by default and is supported in public deployments for non-commercial use. Elite Online data is CC0 1.0, and the API does not require attribution for it. Masters OTB data is licensed for non-commercial use with attribution; the acknowledgment and the full terms are in [LICENSING.md](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment). Operators can disable the route with `OPENING_EXPLORER_ENABLE_GAME_RETRIEVAL=false`, in which case it returns `404`. The API does not offer bulk export; use the published release assets for that.


## 6. Find games from the current board

Discover players by prefix, then combine a returned player ID with a board position. Use the player result's dataset version to pin the player selection. The game search response returns stable game IDs and the matching ply for navigating directly to the position.

```sh
API=http://127.0.0.1:8080
COLLECTION=elite-online
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/players" --data-urlencode 'prefix=mag'
# Copy the returned player id and dataset_version into PLAYER_ID and DATASET_VERSION.
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "player_id=$PLAYER_ID" --data-urlencode "version=$DATASET_VERSION" \
  --data-urlencode 'moves=e4,c5' --data-urlencode 'min_both_ratings=2200'
```

For a fresh sample, obtain a player name from a game result and choose a prefix from it. Follow `next_cursor` for more results, then fetch an individual canonical PGN by its stable game ID. See [Search API](SEARCH_API.md) for player selection, pagination and capability handling.

## 7. Compare filtered opening statistics

Send the board and one or more metadata filters to the filtered endpoint. For a ratings-only view on an enriched Elite Online snapshot:

```sh
curl --fail-with-body --get "$API/v1/explorer/position/filtered" \
  --data-urlencode 'collection=elite-online' --data-urlencode 'moves=e4,c5' \
  --data-urlencode 'min_both_ratings=2200' --data-urlencode 'from_year=2015'
```

The statistics include the same W/D/L and candidate-move counts as the unfiltered endpoint, restricted to the matching games. Keep the corpus and board fixed when comparing filters.
