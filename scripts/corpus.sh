#!/usr/bin/env bash
# scripts/corpus.sh — reproducible corpus acquisition, build, release, and serving.
#
#   ./scripts/corpus.sh plan        [--collection C]
#   ./scripts/corpus.sh sources     [--collection C]
#   ./scripts/corpus.sh fetch       [--collection C] [--offline]
#   ./scripts/corpus.sh verify-sources
#   ./scripts/corpus.sh build       --collection masters-otb|elite-online
#   ./scripts/corpus.sh verify
#   ./scripts/corpus.sh package     --collection C
#   ./scripts/corpus.sh serve
#   ./scripts/corpus.sh http-check
#
# Thin wrapper around scripts/corpus.py, which in turn only orchestrates the existing
# `chess-database` CLI. Data lives in $CHESS_DATABASE_DATA_DIR (default ./data,
# git-ignored). $OPENING_EXPLORER_DATA_DIR and $CODB_DATA_DIR remain supported.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -d "$HOME/.cargo/bin" ]; then export PATH="$HOME/.cargo/bin:$PATH"; fi
exec python3 -I "$ROOT_DIR/scripts/corpus.py" "$@"
