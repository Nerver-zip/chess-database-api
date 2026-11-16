#!/usr/bin/env bash
# Quick tour of the Opening Explorer HTTP API using curl.
#
# Usage:
#   OPENING_EXPLORER_URL=http://127.0.0.1:8080 ./examples/curl/quickstart.sh
#
# OPENING_EXPLORER_COLLECTION selects the collection to query (default
# masters-otb). Use elite-online for a server that loads only Elite Online.
# Set JSON_FORMATTER="python3 -m json.tool" (or "jq .") for indented output.
# Requires curl. The script exits non-zero if any request fails.
set -euo pipefail

BASE="${OPENING_EXPLORER_URL:-http://127.0.0.1:8080}"
COLLECTION="${OPENING_EXPLORER_COLLECTION:-masters-otb}"
FORMATTER="${JSON_FORMATTER:-cat}"

echo "== Liveness: GET /health"
curl --fail --silent --show-error "$BASE/health" | $FORMATTER

echo "== Readiness: GET /ready"
curl --fail --silent --show-error "$BASE/ready" | $FORMATTER

echo "== Loaded collections and dataset versions: GET /v1/collections"
curl --fail --silent --show-error "$BASE/v1/collections" | $FORMATTER

echo "== Position after 1.e4 e5 2.Nf3 in $COLLECTION: GET /v1/explorer/position"
curl --fail --silent --show-error --get "$BASE/v1/explorer/position" \
  --data-urlencode "collection=$COLLECTION" \
  --data-urlencode "moves=e4,e5,Nf3" | $FORMATTER

echo "== Same position by FEN in $COLLECTION"
curl --fail --silent --show-error --get "$BASE/v1/explorer/position" \
  --data-urlencode "collection=$COLLECTION" \
  --data-urlencode "fen=rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2" | $FORMATTER
