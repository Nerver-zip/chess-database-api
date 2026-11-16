#!/usr/bin/env bash
# scripts/validate.sh — Unified verification gate for opening-explorer-api
#
# Runs in sequence:
# 1. cargo fmt --all -- --check
# 2. cargo clippy --workspace --all-targets -- -D warnings
# 3. cargo test --workspace
# 4. python3 -m unittest for Python regression test suites (including optional Docker smoke)
# 5. python3 scripts/validate_production_cpi_benchmarks.py (+ validate_corpus_evidence.py)
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
echo "          opening-explorer-api Unified Verification Gate"
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
cargo test -p opening-explorer-cli --example audit_dedupe_source
echo "  [PASS] All Rust test suites passed."

echo ""
echo "=== [4/8] Running Python test suites (unittest) ==="
python3 -m unittest \
    scripts/test_feasibility_alignment.py \
    scripts/test_reproduce_runner.py \
    scripts/test_validate_benchmarks.py \
    scripts/test_bench_http.py \
    scripts/test_corpus.py \
    scripts/test_gigabase_eval.py \
    scripts/test_docker_deployment.py \
    scripts/test_masters_event_audit.py \
    scripts/test_independent_pgn_audit_evidence.py \
    scripts/test_release_assets.py \
    scripts/test_release_pgn_verify.py \
    scripts/test_docs_examples.py \
    scripts/test_policy_decisions.py
echo "  [PASS] Python unit and regression tests passed."

echo ""
echo "=== [5/8] Validating Production CPI benchmark artifacts ==="
python3 scripts/validate_production_cpi_benchmarks.py
echo "  [PASS] Production CPI benchmarks valid."
python3 scripts/validate_corpus_evidence.py
echo "  [PASS] Real-corpus release evidence valid."

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
