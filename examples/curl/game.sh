#!/usr/bin/env bash
# Download one stored game as PGN by its stable 64-bit game ID.
#
# Usage:
#   ./examples/curl/game.sh <collection> <game-id> [output.pgn]
#
# Only works when the server runs with game retrieval enabled
# (--enable-game-retrieval true, the default). When retrieval is disabled the
# route is absent and the server answers 404 with the error code not_found.
# Check the dataset terms before publishing or redistributing PGN text.
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <masters-otb|elite-online> <game-id> [output.pgn]" >&2
  exit 2
fi

BASE="${OPENING_EXPLORER_URL:-http://127.0.0.1:8080}"
COLLECTION="$1"
GAME_ID="$2"
OUTPUT="${3:-}"

# The ID is a decimal u64 path segment. Reject anything else before sending it.
if ! [[ "$GAME_ID" =~ ^[0-9]+$ ]]; then
  echo "error: game-id must be an unsigned decimal integer" >&2
  exit 2
fi

URL="$BASE/v1/collections/$COLLECTION/games/$GAME_ID"

if [ -n "$OUTPUT" ]; then
  curl --fail --silent --show-error "$URL" --output "$OUTPUT"
  echo "wrote $(wc -c < "$OUTPUT") bytes to $OUTPUT"
else
  curl --fail --silent --show-error "$URL"
fi
