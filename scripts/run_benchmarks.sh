#!/usr/bin/env bash
set -euo pipefail

# Delegate to the comprehensive reproduction runner
exec "$(dirname "$0")/reproduce_benchmarks.sh" "$@"
