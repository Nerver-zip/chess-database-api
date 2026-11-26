---
summary: "Integration guide for game discovery, position search, filtered opening statistics, and metadata lookups."
read_when: "Building a client that searches games or players, follows position matches, or consumes filtered Opening Explorer statistics."
status: current
---

# Search API

This guide covers the discovery and search endpoints added alongside the legacy Opening Explorer routes. For the legacy position and PGN contracts, see the [API Reference](API_REFERENCE.md). Dataset terms are separate from the software license; see [Licensing](LICENSING.md), especially [the current Masters OTB policy](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment).

The examples use a local service at `http://127.0.0.1:8080` and the `elite-online` collection. Replace names and event prefixes with values present in the installed snapshot. Each collection is queried separately; FIDE Elo and Lichess Glicko-2 values and counts are never combined.

## 1. Endpoints and capabilities

Search and discovery handlers use GET and return JSON with `Cache-Control: private, no-store`. The existing `GET /v1/collections` metadata route retains its caching behavior from the [API Reference](API_REFERENCE.md).

| Method and path | Purpose |
| --- | --- |
| `GET /v1/collections` | List loaded collections, dataset versions, CPI information, and feature capabilities. |
| `GET /v1/collections/{collection}/players` | Find players by exact normalized name, name prefix, or FIDE ID. |
| `GET /v1/collections/{collection}/players/{id}?version={dataset_version}` | Read one snapshot-local player record. |
| `GET /v1/collections/{collection}/events` | Find event groups by exact normalized name or prefix, optionally narrowed by date, year, or site. |
| `GET /v1/collections/{collection}/events/{id}?version={dataset_version}` | Read one snapshot-local event record. |
| `GET /v1/collections/{collection}/games` | Search game summaries using metadata filters, a position, or both. |
| `GET /v1/collections/{collection}/games/{id}/metadata` | Read one game summary by its stable game ID. |
| `GET /v1/explorer/position/filtered` | Return CPI-compatible position statistics after applying metadata filters. |
| `GET /v1/collections/{collection}/games/{id}` | Return the stored PGN when legacy game retrieval is enabled. |

The `capabilities` object on each collection returned by `/v1/collections` reports what is available in that running service and snapshot:

- `opening_explorer` and `cpi_depth_plies` describe the CPI position-statistics endpoint.
- `game_search`, `position_search`, `player_search`, and `event_search` require the search index and the collection's discovery setting.
- `filtered_explorer` is true only when discovery is enabled and the search index coverage supports the collection's CPI depth.
- `game_metadata` describes the separate game-metadata setting.
- `game_retrieval` describes the existing PGN route and its independent setting.
- `position_search_coverage` is either `{"mode":"bounded","max_ply":N}`, `{"mode":"full"}`, or null.
- `game_filter_families` lists the supported search families when game discovery is enabled.

A false capability means the feature is not available through that service configuration. A missing search index also makes search unavailable even if an operator enabled the corresponding setting.

## 2. Start the service with search enabled

Search indexes are built and verified as part of the dataset snapshot. The discovery, metadata-detail, and PGN controls are separate. Discovery and game metadata default to enabled for Elite Online and disabled for Masters OTB. PGN retrieval remains controlled by the existing `--enable-game-retrieval` setting, which defaults to true.

For example, to explicitly enable Elite Online discovery and game metadata:

~~~sh
chess-database serve \
  --elite-dataset /data/elite-online \
  --elite-discovery true \
  --elite-game-metadata true \
  --search-max-page-size 100 \
  --search-concurrency 2 \
  --search-deadline-ms 2000
~~~

The discovery flags are `--masters-discovery` and `--elite-discovery`. The game metadata flags are `--masters-game-metadata` and `--elite-game-metadata`. They also accept environment variables `CHESS_DATABASE_MASTERS_DISCOVERY`, `CHESS_DATABASE_ELITE_DISCOVERY`, `CHESS_DATABASE_MASTERS_GAME_METADATA`, and `CHESS_DATABASE_ELITE_GAME_METADATA`.

Search controls are `--search-max-page-size` (1–100, default 100), `--search-concurrency` (1–8, default 2), and `--search-deadline-ms` (greater than zero and at most 15000, default 2000). Search requests can wait in a bounded queue for up to 100 ms. Only one filtered aggregation can execute at a time. Saturation returns `search_busy`. A separate overall HTTP request timeout also applies. Move-sequence queries default to a maximum of 200 plies and can be configured up to 512 with `--max-move-plies`.

## 3. Game search

The collection is part of the path. The game-search endpoint rejects a `collection` query parameter. Unknown query parameters are rejected. Use `curl --get --data-urlencode` for FEN strings, moves, and names.

### Position and continuation

Supply one of `fen` or `moves`. Moves start from the standard initial position and may be legal SAN or UCI tokens separated by spaces or commas. FEN and moves are mutually exclusive. A position search returns each matching game once. With `continuation`, the game must have the specified legal SAN or UCI move after a matching occurrence of the position. Repeated visits are searched, so the requested continuation may match a later occurrence.

~~~sh
API=http://127.0.0.1:8080
COLLECTION=elite-online

# Games containing the position after 1.e4 e5 2.Nf3.
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "moves=e4,e5,Nf3" \
  --data-urlencode "limit=10"

# The same position followed by ...Nc6 (UCI: b8c6).
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "moves=e4,e5,Nf3" \
  --data-urlencode "continuation=b8c6" \
  --data-urlencode "limit=10"
~~~

Without a continuation filter, games with a terminal occurrence inside recorded position coverage can match; their `position_match.continuation` is null. With a continuation filter, terminal occurrences do not match. A game summary's `position_match` identifies the earliest matching ply and, when present, its played continuation.

Search coverage is reported as `position_coverage` in a position-search response and as `capabilities.position_search_coverage` from collection discovery. Bounded coverage only supports occurrences recorded through its advertised maximum ply. A missing game match outside that window does not establish that the position never occurred.

### Players, matchups, events, and ratings

Player and opponent name filters are exact normalized-name matches. Use `player_side=either` (the default), `white`, or `black` to select the side for `player`. An `opponent` filter requires `player` and matches the opposite side. The `white` and `black` selectors can filter each side directly. For each selector, supply either the name parameter or its `_id` parameter, not both.

~~~sh
# Exact-name player search.
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/players" \
  --data-urlencode "name=Davigator"

# Matchup in either color order.
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "player=Davigator" \
  --data-urlencode "opponent=schnappi"

# Position search restricted to the selected player.
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "moves=e4,e5,Nf3" \
  --data-urlencode "player=Davigator"

# Both players must have a rating in the requested inclusive range.
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "min_both_ratings=2400" \
  --data-urlencode "moves=e4,e5,Nf3" \
  --data-urlencode "limit=10"
~~~

To find an event group, search by exact name or prefix, then use its ID and the response's dataset version to filter games. Event IDs belong to one collection snapshot.

~~~sh
EVENTS=$(curl --fail-with-body --get "$API/v1/collections/$COLLECTION/events" \
  --data-urlencode "prefix=rated blitz" --data-urlencode "limit=10")
printf '%s\n' "$EVENTS" | jq .
EVENT_ID=$(printf '%s' "$EVENTS" | jq -r '.events[0].id')
VERSION=$(printf '%s' "$EVENTS" | jq -r '.dataset_version')

curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "event_id=$EVENT_ID" \
  --data-urlencode "version=$VERSION" \
  --data-urlencode "limit=10"
~~~

If the prefix returns no events, choose a prefix that exists in the installed snapshot. The event list returns conservative groups based on metadata preserved by the collection. A site value identical to a game-specific URL remains available in game metadata and is omitted from event grouping.

Player discovery requires exactly one of `name`, `prefix`, or `fide_id`. Name and prefix lookups use normalized names; FIDE ID lookup is exact. Event discovery requires exactly one of `name` or `prefix`; it can also take `year`, `from_date`, `through_date`, and `site`. Both routes accept `limit` and `cursor`, return `has_more` and `next_cursor`, and omit exact totals. Player summaries include `id`, `display_name`, `fide_id`, `game_count`, `min_rating`, `max_rating`, `first_date_lower`, and `last_date_upper`. Event summaries include `id`, `display_name`, `year`, `site`, `game_count`, `first_date_lower`, and `last_date_upper`.

### Filter reference

All range bounds are inclusive. Inverted ranges are invalid. Missing ratings do not satisfy a rating range.

| Filter family | Query parameters | Meaning |
| --- | --- | --- |
| Player | `player`, `player_id`, `player_side`, `opponent`, `opponent_id`, `white`, `white_id`, `black`, `black_id` | Exact player or opponent match. IDs are snapshot-local. |
| White rating | `min_white_rating`, `max_white_rating` | White player's rating range. |
| Black rating | `min_black_rating`, `max_black_rating` | Black player's rating range. |
| Selected player rating | `min_player_rating`, `max_player_rating` | Rating range for the selected `player`. Requires `player`. |
| Both ratings | `min_both_ratings`, `max_both_ratings` | Both white and black ratings must be in range. |
| Either rating | `min_either_rating`, `max_either_rating` | At least one side's rating must be in range. |
| Average rating | `min_average_rating`, `max_average_rating` | Mean of white and black ratings; both must be known. |
| Calendar year | `from_year`, `through_year` | Inclusive year interval. Use year or date bounds. |
| Date | `from_date`, `through_date` | Inclusive ISO dates `YYYY-MM-DD`. PGN records with partial dates match when their possible date interval overlaps the requested interval. |
| Result | `result` | `1-0`, `0-1`, `1/2-1/2`, or `*` (unknown). |
| Event | `event`, `event_id` | Exact normalized event name or snapshot-local event ID. |
| Site | `site` | Exact normalized site name. |
| ECO | `eco` | ECO code from A00 through E99; case-insensitive. |
| Time control | `time_control_category` | `bullet`, `blitz`, `rapid`, `classical`, `unknown`, or `other_format`. |
| Game length | `min_plies`, `max_plies` | Inclusive range of mainline plies. |

`player_side`, opponent filters, and selected-player rating filters require `player`. Date and year bounds cannot be combined. `event` and `event_id` cannot be combined.

When a game-search request uses a player or event ID, include `version` with the exact `dataset_version` from `/v1/collections` or the discovery response. IDs are snapshot-local. Player and event detail routes also require this version. A mismatched version returns `stale_cursor`.

### Sort, count, and response

Supported `sort` values are `default`, `date_asc`, `date_desc`, `white_rating`, `black_rating`, `average_rating`, and `game_length`. The default follows the snapshot's canonical game order. Rating and length sorts descend. Date sorts use the earliest possible date for partial dates. Missing values sort last. Ties use the canonical game order in the same direction.

`limit` defaults to the smaller of 25 and the configured maximum. It must be between 1 and that maximum. `exact_count=true` requests an exact `total_games`. Otherwise `total_games` is null. Exact counts for position searches can require additional work and may return a bounded-work or deadline error.

A game-search response has this shape:

~~~json
{
  "collection": "elite-online",
  "dataset_version": "opaque-snapshot-id",
  "position_coverage": {"mode": "bounded", "max_ply": 30},
  "games": [
    {
      "id": "123456789",
      "white": "White Name",
      "black": "Black Name",
      "white_rating": 2400,
      "black_rating": 2350,
      "result": "1-0",
      "position_match": {"ply": 4, "continuation": "b8c6"}
    }
  ],
  "has_more": true,
  "next_cursor": "opaque-token",
  "total_games": null
}
~~~

`position_coverage` is null when the request has no position filter. Game IDs are returned as decimal strings. Player and event IDs are decimal strings too, and are snapshot-local. Discovery pages return `players` or `events`, with `has_more` and `next_cursor`; they intentionally omit total counts.

Each `games[]` row and the `game` object returned by game metadata contain `id`, `white`, `black`, `white_rating`, `black_rating`, `result`, `date`, `event`, `site`, `round`, `eco`, `time_control`, `time_control_category`, `plies`, `starting_fen`, `source_id`, `broadcast_url`, `white_player_id`, `black_player_id`, `event_id`, `white_fide_id`, and `black_fide_id`. Values that were not recorded can be null. Player and event IDs in these rows are snapshot-local. `position_match` is omitted when there was no position filter; when present, it contains `ply` and `continuation`, which is null for a terminal occurrence.

The game metadata response includes `collection`, `dataset_version`, `game`, and `source_attributions`. The attribution array is empty when the source snapshot has no attribution for the game. When present, it can contain source tags, identity evidence, titles, or FIDE IDs as preserved by the sidecar. The per-side FIDE IDs in `game` are the conservative derived identities. Player and event detail responses similarly use a `player` or `event` field. Position statistics use the response fields documented in the [API Reference](API_REFERENCE.md#51-get-v1explorerposition).

## 4. Pagination

Use the returned `next_cursor` unchanged with the same collection, filters, position, and sort. Cursors are opaque and bound to that query and immutable dataset snapshot. A malformed cursor or one used with different filters returns `invalid_cursor`; a cursor from an older snapshot returns `stale_cursor`. Start from the first page again after either error.

~~~sh
FIRST=$(curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "player=Davigator" \
  --data-urlencode "sort=date_desc" \
  --data-urlencode "limit=2")
printf '%s\n' "$FIRST" | jq .

CURSOR=$(printf '%s' "$FIRST" | jq -r '.next_cursor')
curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "player=Davigator" \
  --data-urlencode "sort=date_desc" \
  --data-urlencode "limit=2" \
  --data-urlencode "cursor=$CURSOR"
~~~

The `limit` can be changed between pages, but keep the filters, position, collection, and sort fixed. Discovery cursors are also bound to their discovery selector and filters.

## 5. Retrieve a selected game

Game search and metadata responses return a stable game ID. Read the concise game record from the metadata endpoint or retrieve its PGN from the existing game route when PGN retrieval is enabled.

~~~sh
PAGE=$(curl --fail-with-body --get "$API/v1/collections/$COLLECTION/games" \
  --data-urlencode "player=Davigator" --data-urlencode "limit=1")
GAME_ID=$(printf '%s' "$PAGE" | jq -r '.games[0].id')

curl --fail-with-body "$API/v1/collections/$COLLECTION/games/$GAME_ID/metadata"
curl --fail-with-body "$API/v1/collections/$COLLECTION/games/$GAME_ID"
~~~

PGN retrieval is an independent capability from game search and metadata. Check `capabilities.game_retrieval` before offering a PGN action. If disabled, the PGN route is not mounted.

## 6. Filtered Opening Explorer statistics

Use the filtered endpoint when the UI needs W/D/L and candidate moves for a position after applying game metadata filters. It returns the same position-statistics shape as the existing Explorer endpoint, including `cpi_depth_plies`, but the counts describe only the matching subset.

~~~sh
curl --fail-with-body --get "$API/v1/explorer/position/filtered" \
  --data-urlencode "collection=$COLLECTION" \
  --data-urlencode "moves=e4,e5,Nf3" \
  --data-urlencode "player=Davigator" \
  --data-urlencode "min_both_ratings=2200"
~~~

This endpoint requires `collection` and one of `fen` or `moves`. It accepts the metadata filters in section 3. It does not accept `continuation`, sorting, pagination, or `exact_count`.

The filtered counts preserve CPI v2 semantics: a game contributes at most once, using its first occurrence of the position with a recorded continuation before the CPI depth limit. Unknown results (`*`) and terminal positions do not contribute to W/D/L or candidate-move counts. Consequently, a game search for a position may return a game that does not contribute to filtered Explorer statistics: game search can include unknown results and terminal occurrences within its search coverage.

Common rating, year and date filters use a compact scan on supported snapshots. Broad compound filters can still exceed the work limit, and an expensive query can occupy the single aggregation slot. An otherwise unrestricted request with `min_plies=0` has the same statistics as the legacy CPI query. See the [production measurements](research/search-index-experiments.md#final-hardening-on-both-production-snapshots).

`filtered_explorer` is reported in collection capabilities only when its search coverage can support that collection's CPI depth. If it is false or the endpoint returns `search_unavailable`, use the unfiltered Explorer endpoint or a collection/snapshot that advertises this capability.

## 7. Capability discovery flow

Read capabilities before showing a search control. Feature flags can differ by collection and operator configuration, and a dataset may not contain a search index.

~~~sh
curl --fail-with-body "$API/v1/collections" | jq '
  .collections[] |
  {
    collection,
    dataset_version,
    cpi_available,
    cpi_depth_plies,
    capabilities
  }'
~~~

Pin the returned `dataset_version` alongside snapshot-local player/event IDs. Do not compare numeric ratings across collections or merge their statistics.

## 8. Errors and bounded work

Errors use the standard JSON envelope `{"error":{"code":"...","message":"..."}}`. Branch on `code`; message text is explanatory.

| HTTP status | Codes | Meaning |
| ---: | --- | --- |
| 400 | `invalid_collection`, `invalid_query`, `invalid_game_id`, `missing_position`, `ambiguous_position`, `invalid_fen`, `invalid_moves`, `move_sequence_too_long`, `invalid_cursor` | Invalid or contradictory input, unknown query parameter, bad identifier, missing or conflicting position, bad move/FEN, or malformed cursor. |
| 403 | `search_disabled` | Operator has disabled that search capability for the collection. |
| 404 | `not_found`, `game_not_found` | Unknown route or game ID. |
| 408 | `search_timeout`, `request_timeout` | Search-specific or overall HTTP request deadline elapsed. |
| 409 | `stale_cursor` | Cursor or supplied dataset version belongs to a different snapshot. Restart from the current snapshot. |
| 422 | `search_work_limit` | Query exceeded a bounded work budget. Narrow the filters or remove exact counting. |
| 429 | `search_busy` | The search or aggregation queue is full or the admission wait expired. |
| 503 | `search_unavailable`, `collection_unavailable` | Required search index or collection is unavailable. |
| 500 | `storage_error`, `internal_error` | Server-side failure. |

The query string is limited to 8192 bytes. Search is bounded by page size, work budget, deadline, and configured concurrency. A broad position query, exact count, or broad metadata filter can exceed the work budget; the API returns an error rather than a partial result.

For `search_busy`, retry only if the query still describes the current board or input. Debounce autocomplete and cancel superseded navigation. For `search_timeout`, show that the search expired and consider asking for tighter inputs. For a work-limit error, narrow the filters or use the unfiltered CPI endpoint when aggregate corpus statistics are acceptable. Do not present errors as zero matching games.

## 9. Evalis integration

Release engineering is in progress. The software version `v0.2.0`, versioned container image and downloadable search companions are not yet published; do not pin an unpublished tag. Until a verified release is available, build the merged source and the `chess-database-api:local` image using the [deployment guide](DOCKER_DEPLOYMENT.md). Use the published Masters OTB R3 and Elite Online R1 canonical datasets, and enrich compatible local copies as described in [Search indexes](SEARCH_INDEX.md). Their intended companion coverage is D40 and D30 respectively. Native two-collection Docker checks have been completed; the eviction-advised legacy performance comparison remains inconclusive.

For an integration deployment, use the collection capabilities returned by `/v1/collections` as the source of truth, including `position_search_coverage`, `filtered_explorer` and `game_retrieval`. Configure the appropriate discovery and metadata flags as described in section 2. The Compose default is 512 MiB, two CPUs and two search workers; the native mixed workload preserved legacy responsiveness but returned busy responses for some filtered searches. See the [deployment measurements](DOCKER_DEPLOYMENT.md#two-collection-acceptance-on-a-local-docker-host) and size capacity for the expected workload.

Debounce interactive inputs and cancel obsolete requests. The default search deadline is two seconds and admission can wait up to 100 ms; allow for both and network overhead in client timeouts. Retry `search_busy` with bounded backoff only while the input is still current. Treat `search_timeout` as an expired search, narrow `search_work_limit` queries, and restart pagination on `stale_cursor`. Keep snapshot-local IDs and cursors with their dataset version; use stable decimal game IDs for PGN retrieval. See sections 3–8 for the exact routes, filters, position and continuation semantics, and error contracts.

Evalis currently provides an Opening Explorer gateway, not these new search routes. Its Nginx allowlist in `deploy/nginx-explorer-enabled.conf` exposes only these GET requests under `/opening-explorer`:

1. `/v1/collections` with no query parameters;
2. `/v1/explorer/position` with validated `collection` and exactly one of `fen` or `moves`;
3. `/v1/collections/{collection}/games/{decimal-id}` for PGN retrieval.

All other paths under `/opening-explorer` currently return 404. In particular, player/event discovery, game search, game metadata, and filtered Explorer are not reachable through the current Evalis gateway. The browser should not bypass the same-origin gateway to call the private API directly.

The existing integration can be checked with these requests:

~~~sh
EVALIS_API=http://localhost:8080/opening-explorer

curl --fail-with-body "$EVALIS_API/v1/collections"
curl --fail-with-body --get "$EVALIS_API/v1/explorer/position" \
  --data-urlencode "collection=elite-online" \
  --data-urlencode "moves=e4,e5,Nf3"
~~~

Adding game search to Evalis requires changes in the Evalis repository. Its gateway must deliberately allow the required GET paths and validated query parameters, preserve the fixed upstream and same-origin boundary, and account for the API's `private, no-store` responses. New browser flows must check collection capabilities and keep any player/event IDs paired with their dataset version. No Evalis gateway changes are included in this repository.

Before enabling public Masters OTB discovery, review the current scope in [LICENSING.md section 6](LICENSING.md#6-current-policy-for-the-masters-otb-dataset-and-public-deployment). The documented decision covers public non-commercial position statistics and individual game retrieval, and states that the API does not provide bulk game export. It does not explicitly address these new metadata and search enumeration routes. Do not assume that existing clearance automatically authorizes new public enumeration. The Masters discovery and metadata flags default to disabled. Elite Online is separately documented as CC0 1.0. The API software itself is licensed under MIT OR Apache-2.0; see [LICENSING.md](LICENSING.md).
