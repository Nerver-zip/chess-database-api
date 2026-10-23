#!/usr/bin/env bash
# scripts/reproduce_benchmarks.sh
#
# Reproducible Benchmark & Empirical Scaling Runner for PR-01.
#
# Usage:
#   ./scripts/reproduce_benchmarks.sh [--ci | --stage-a | --all | --help]
#
# Modes:
#   --ci / --stage-a:
#       Executes only the deterministic Stage A validation fixture (1,000 games),
#       verifies artifact schemas, and runs consistency gates.
#       Requires NO external downloads or large data files.
#   --all / --empirical:
#       Executes Stage A, Stage B (August 2026 Masters OTB + January 2013 Elite Online),
#       and the full 5-point progressive Masters OTB scaling series
#       (N in {1000, 2500, 5000, 10000, 20000}) against data/masters_combined.pgn.zst.
#       Then re-fits Heaps' Law parameters and executes schema/accounting validation.
#
# Source Dataset Provenance:
#   1. data/masters_otb_2026-07.pgn.zst
#      URL: https://database.lichess.org/broadcast/lichess_db_broadcast_2026-07.pgn.zst
#      SHA-256: 714d0eb99f99fca8d791142038b6c59b5ca6a51b3339bd3891a92f4bdffcbf0c
#   2. data/masters_otb_2026-08.pgn.zst
#      URL: https://database.lichess.org/broadcast/lichess_db_broadcast_2026-08.pgn.zst
#      SHA-256: e227c35c3207ebade754849c1825982e2717ae82c013013d896778e27be77724
#   3. data/masters_otb_2026-09.pgn.zst
#      URL: https://database.lichess.org/broadcast/lichess_db_broadcast_2026-09.pgn.zst
#      SHA-256: 27a39d035767a30901b89602f33f2a7b27d4ab5025842071165fa079f8fff1df
#   4. data/elite_online_2013-01.pgn.zst
#      URL: https://database.lichess.org/standard/lichess_db_standard_rated_2013-01.pgn.zst
#      SHA-256: aa40b3671fa3cf1072eb182892cd90b0e1e003a4a5943492f64b77e7f3fd1635
#
# Combined Corpus Construction:
#   Chronological sequential concatenation (2026-07 -> 2026-08 -> 2026-09):
#   zstd -dc data/masters_otb_2026-07.pgn.zst data/masters_otb_2026-08.pgn.zst data/masters_otb_2026-09.pgn.zst | zstd -3 -o data/masters_combined.pgn.zst
#   Uncompressed stream SHA-256: 9cc0f844cf8c788e76ad13f8c634beb19dc73f55018a124c2df00aa7ecc716e7

set -euo pipefail

MODE="${1:---all}"

if [ "$MODE" = "--help" ] || [ "$MODE" = "-h" ]; then
    grep '^#' "$0" | cut -c 3-
    exit 0
fi

echo "=== [1/5] Building release binaries ==="
cargo build --release -p codb-cli

BIN="./target/release/codb"
RESULTS_DIR="benchmarks/results"
mkdir -p "$RESULTS_DIR"

echo "=== [2/5] Stage A Benchmark (Deterministic Fixture: 1,000 games) ==="
"$BIN" bench \
    --input fixtures/stage_a_validation.pgn \
    --collection masters-otb \
    --output-dir "$RESULTS_DIR" \
    --run-name stage_a_validation

if [ "$MODE" = "--ci" ] || [ "$MODE" = "--stage-a" ]; then
    echo "=== Running Schema and Consistency Validation (CI Mode) ==="
    python3 scripts/validate_benchmarks.py
    echo "CI reproduction check complete."
    exit 0
fi

# Empirical mode
echo "=== [3/5] Checking Empirical Datasets ==="
if [ ! -f "data/masters_combined.pgn.zst" ]; then
    if [ -f "data/masters_otb_2026-07.pgn.zst" ] && [ -f "data/masters_otb_2026-08.pgn.zst" ] && [ -f "data/masters_otb_2026-09.pgn.zst" ]; then
        echo "Constructing data/masters_combined.pgn.zst from 2026-07, 2026-08, 2026-09..."
        zstd -dc data/masters_otb_2026-07.pgn.zst data/masters_otb_2026-08.pgn.zst data/masters_otb_2026-09.pgn.zst | zstd -3 -o data/masters_combined.pgn.zst
    else
        echo "NOTE: data/masters_combined.pgn.zst not found and source archives not present."
        echo "To reproduce full empirical benchmarks, please provide data/masters_combined.pgn.zst."
        echo "Falling back to schema and projection validation."
        python3 scripts/validate_benchmarks.py
        exit 0
    fi
fi

# Stage B single-archive benchmarks if available
if [ -f "data/masters_otb_2026-08.pgn.zst" ]; then
    echo "=== Stage B: Masters OTB (August 2026 Broadcast Archive) ==="
    "$BIN" bench \
        --input data/masters_otb_2026-08.pgn.zst \
        --collection masters-otb \
        --output-dir "$RESULTS_DIR" \
        --run-name stage_b_masters_otb
fi

if [ -f "data/elite_online_2013-01.pgn.zst" ]; then
    echo "=== Stage B: Elite Online (January 2013 Rated Archive) ==="
    "$BIN" bench \
        --input data/elite_online_2013-01.pgn.zst \
        --collection elite-online \
        --output-dir "$RESULTS_DIR" \
        --run-name stage_b_elite_online
fi

# Progressive Masters OTB Scaling Series
echo "=== [4/5] Executing Progressive Masters OTB Scaling Series ==="
SAMPLE_LIMITS=(1000 2500 5000 10000 20000)
RUN_NAMES=("masters_1k" "masters_2.5k" "masters_5k" "masters_10k" "masters_20k")

for i in "${!SAMPLE_LIMITS[@]}"; do
    LIMIT="${SAMPLE_LIMITS[$i]}"
    RUN_NAME="${RUN_NAMES[$i]}"
    echo "--- Running $RUN_NAME (--max-games $LIMIT) ---"
    "$BIN" bench \
        --input data/masters_combined.pgn.zst \
        --collection masters-otb \
        --max-games "$LIMIT" \
        --output-dir "$RESULTS_DIR" \
        --run-name "$RUN_NAME"
done

echo "=== [5/5] Re-fitting Heaps' Law & Validating Projections ==="
python3 scripts/fit_heaps.py
python3 scripts/validate_benchmarks.py

echo "=== Reproduction complete. All artifacts are verified and up to date! ==="
