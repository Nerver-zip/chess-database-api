#!/usr/bin/env bash
# Ask a running server to reload (re-verify and swap) its dataset snapshots.
#
# Usage:
#   OPENING_EXPLORER_ADMIN_TOKEN=<token> ./examples/curl/reload.sh
#
# The server must be started with an admin token (--admin-token or
# OPENING_EXPLORER_ADMIN_TOKEN). The token is read from the environment and
# is never printed. Requests that carry an Origin header are refused, so this
# script sends none.
#
# Verifying a large snapshot can take a while. The reload is not subject to the
# server's read request timeout: the request returns when the reload has finished.
# Signals are an alternative: `kill -HUP <pid>` runs the same reload.
set -euo pipefail

BASE="${OPENING_EXPLORER_URL:-http://127.0.0.1:8080}"

if [ -z "${OPENING_EXPLORER_ADMIN_TOKEN:-}" ]; then
  echo "error: set OPENING_EXPLORER_ADMIN_TOKEN to the server's admin token" >&2
  exit 2
fi

# Bearer header read from the environment; curl reads it via a config file
# descriptor so the token does not appear in the process list.
curl --fail --silent --show-error --request POST "$BASE/v1/admin/reload" \
  --header @<(printf 'Authorization: Bearer %s\n' "$OPENING_EXPLORER_ADMIN_TOKEN")
echo
