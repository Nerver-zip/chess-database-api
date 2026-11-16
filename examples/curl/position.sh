#!/usr/bin/env bash
# Query one position from the Opening Explorer API.
#
# Usage:
#   ./examples/curl/position.sh <collection> --moves "e4,e5,Nf3"
#   ./examples/curl/position.sh <collection> --fen "<FEN>"
#   ./examples/curl/position.sh <collection> --moves ""          # initial position
#
# <collection> is masters-otb or elite-online. Values are URL-encoded by curl,
# so spaces, commas and FEN slashes are safe. Set OPENING_EXPLORER_URL to
# change the server (default http://127.0.0.1:8080).
set -euo pipefail

if [ "$#" -lt 3 ]; then
  echo "usage: $0 <masters-otb|elite-online> --moves <SAN-or-UCI list> | --fen <FEN>" >&2
  exit 2
fi

BASE="${OPENING_EXPLORER_URL:-http://127.0.0.1:8080}"
COLLECTION="$1"
MODE="$2"
VALUE="$3"

case "$MODE" in
  --moves) PARAM="moves" ;;
  --fen) PARAM="fen" ;;
  *)
    echo "error: second argument must be --moves or --fen" >&2
    exit 2
    ;;
esac

# -G turns the --data-urlencode pairs into a query string on a GET request.
# The body and the HTTP status are printed even for 4xx responses, so the
# error envelope (error.code, error.message) is visible.
curl --silent --show-error --get --write-out '\nHTTP %{http_code}\n' \
  "$BASE/v1/explorer/position" \
  --data-urlencode "collection=$COLLECTION" \
  --data-urlencode "$PARAM=$VALUE"
