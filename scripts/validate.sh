#!/usr/bin/env bash
# scripts/validate.sh — Unified verification gate for chess-openings-db
#
# Runs in sequence:
# 1. cargo fmt --all -- --check
# 2. cargo clippy --workspace --all-targets -- -D warnings
# 3. cargo test --workspace
# 4. python3 -m unittest for Python regression test suites
# 5. python3 scripts/validate_production_cpi_benchmarks.py
# 6. python3 scripts/validate_benchmarks.py
# 7. python3 scripts/validate_docs.py
# 8. sha256sum -c MANIFEST.sha256

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# Ensure Cargo binary directory is available in PATH
if [ -d "$HOME/.cargo/bin" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

echo "========================================================================"
echo "          chess-openings-db Unified Verification Gate"
echo "========================================================================"

echo ""
echo "=== [1/8] Checking Rust code formatting (cargo fmt) ==="
cargo fmt --all -- --check
echo "  [PASS] Formatting is clean."

echo ""
echo "=== [2/8] Running Clippy linter (zero warnings allowed) ==="
cargo clippy --workspace --all-targets -- -D warnings
echo "  [PASS] Clippy passed with zero warnings."

echo ""
echo "=== [3/8] Running workspace unit and integration tests (cargo test) ==="
cargo test --workspace
echo "  [PASS] All Rust test suites passed."

echo ""
echo "=== [4/8] Running Python test suites (unittest) ==="
python3 -m unittest \
    scripts/test_feasibility_alignment.py \
    scripts/test_reproduce_runner.py \
    scripts/test_validate_benchmarks.py \
    scripts/test_bench_http.py
echo "  [PASS] Python unit and regression tests passed."

echo ""
echo "=== [5/8] Validating Production CPI benchmark artifacts ==="
python3 scripts/validate_production_cpi_benchmarks.py
echo "  [PASS] Production CPI benchmarks valid."

echo ""
echo "=== [6/8] Validating historical benchmark schemas and accounting ==="
python3 scripts/validate_benchmarks.py
echo "  [PASS] Historical benchmarks and Heaps' projections valid."

echo ""
echo "=== [7/8] Validating documentation integrity & markdown links ==="
python3 scripts/validate_docs.py
echo "  [PASS] Documentation integrity verified."

echo ""
echo "=== [8/8] Verifying cryptographic manifest (MANIFEST.sha256) ==="
sha256sum -c MANIFEST.sha256
echo "  [PASS] Manifest contracts verified."

echo ""
echo "========================================================================"
echo "          SUCCESS: All verification gates passed cleanly!"
echo "========================================================================"
