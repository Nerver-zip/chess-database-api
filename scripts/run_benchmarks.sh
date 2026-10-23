#!/usr/bin/env bash
set -euo pipefail

echo "=== Compiling release binaries ==="
cargo build --release

BIN="./target/release/codb"
RESULTS_DIR="benchmarks/results"
mkdir -p "$RESULTS_DIR"

echo "=== Stage A Benchmark (Deterministic Fixture: 1,000 games) ==="
"$BIN" bench \
    --input fixtures/stage_a_validation.pgn \
    --collection masters-otb \
    --output-dir "$RESULTS_DIR" \
    --run-name stage_a_validation

echo "Stage A benchmark completed successfully."

# If Stage B datasets exist, run Stage B benchmarks
if [ -f "data/masters_otb_2026-08.pgn.zst" ]; then
    echo "=== Stage B Benchmark: Masters OTB (~48k broadcast games) ==="
    "$BIN" bench \
        --input data/masters_otb_2026-08.pgn.zst \
        --collection masters-otb \
        --output-dir "$RESULTS_DIR" \
        --run-name stage_b_masters_otb
fi

if [ -f "data/elite_online_2013-01.pgn.zst" ]; then
    echo "=== Stage B Benchmark: Elite Online (~121k rated games) ==="
    "$BIN" bench \
        --input data/elite_online_2013-01.pgn.zst \
        --collection elite-online \
        --output-dir "$RESULTS_DIR" \
        --run-name stage_b_elite_online
fi

echo "=== Benchmark results summary ==="
ls -lh "$RESULTS_DIR"
