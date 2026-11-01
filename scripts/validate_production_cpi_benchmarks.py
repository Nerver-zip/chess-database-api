#!/usr/bin/env python3
"""
scripts/validate_production_cpi_benchmarks.py — Schema, Accounting & Memory Budget Validator
for Production CPI Benchmarks in benchmarks/production_cpi/.
"""

import glob
import json
import os
import sys

ROOT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BENCHMARKS_DIR = os.path.join(ROOT_DIR, "benchmarks", "production_cpi")

REQUIRED_TOP_FIELDS = [
    "schema_version",
    "benchmark_type",
    "git_commit",
    "dataset_identity",
    "index_construction",
    "storage_accounting",
    "query_performance",
]

REQUIRED_CONSTRUCTION_FIELDS = [
    "games_processed",
    "games_indexed",
    "excluded_unknown_result_games",
    "custom_fen_games",
    "repeated_positions_skipped",
    "raw_observations_extracted",
    "unique_positions",
    "unique_position_move_pairs",
    "cpi_block_count",
    "runs_generated",
    "merge_passes",
    "build_duration_secs",
    "throughput_games_per_sec",
    "peak_process_rss_kib",
    "peak_process_rss_bytes",
    "peak_process_rss_mib",
    "rss_budget_enforced",
    "peak_temporary_disk_bytes",
    "final_cpi_size_bytes",
    "cpi_bytes_per_unique_position",
    "cpi_bytes_per_game",
    "verification_duration_secs",
    "verification_passed",
]


def validate_file(path):
    print(f"Validating production CPI benchmark artifact: {os.path.relpath(path, ROOT_DIR)}")
    with open(path) as f:
        d = json.load(f)

    # 1. Top-level fields
    for field in REQUIRED_TOP_FIELDS:
        if field not in d:
            raise ValueError(f"{path}: Missing top-level field '{field}'")

    if d["benchmark_type"] != "production_cpi_external_sort":
        raise ValueError(f"{path}: Invalid benchmark_type: {d['benchmark_type']}")

    # 2. Construction fields
    c = d["index_construction"]
    for field in REQUIRED_CONSTRUCTION_FIELDS:
        if field not in c:
            raise ValueError(f"{path}: Missing index_construction field '{field}'")

    # 3. Accounting invariants
    processed = c["games_processed"]
    indexed = c["games_indexed"]
    excluded_star = c["excluded_unknown_result_games"]
    if processed != indexed + excluded_star:
        raise ValueError(
            f"{path}: games_processed ({processed}) != games_indexed ({indexed}) + excluded_star ({excluded_star})"
        )

    unique_pos = c["unique_positions"]
    unique_pairs = c["unique_position_move_pairs"]
    if unique_pos == 0:
        raise ValueError(f"{path}: unique_positions must be > 0")
    if unique_pairs < unique_pos:
        raise ValueError(
            f"{path}: unique_position_move_pairs ({unique_pairs}) < unique_positions ({unique_pos})"
        )

    # 4. Memory budget invariant
    mem_budget_mib = d["dataset_identity"]["configuration"]["memory_limit_mib"]
    peak_rss_mib = c["peak_process_rss_mib"]
    if not c["rss_budget_enforced"]:
        raise ValueError(f"{path}: rss_budget_enforced is False (peak {peak_rss_mib} MiB > budget {mem_budget_mib} MiB)")
    if peak_rss_mib > mem_budget_mib * 1.10:
        raise ValueError(
            f"{path}: peak_process_rss_mib ({peak_rss_mib}) exceeded memory budget ({mem_budget_mib} MiB) by >10%"
        )

    # 5. Verification status
    if not c["verification_passed"]:
        raise ValueError(f"{path}: verification_passed is False")

    # 6. Latency percentile invariants
    qp = d["query_performance"]
    warm = qp["warm_cache"]
    cold = qp["cold_cache_end_to_end"]
    if not (warm["p50_latency_us"] <= warm["p95_latency_us"] <= warm["p99_latency_us"]):
        raise ValueError(f"{path}: warm latency ordering violated: {warm}")
    if not (cold["p50_latency_us"] <= cold["p95_latency_us"] <= cold["p99_latency_us"]):
        raise ValueError(f"{path}: cold latency ordering violated: {cold}")

    return d


def validate_all():
    files = sorted(glob.glob(os.path.join(BENCHMARKS_DIR, "*.json")))
    if not files:
        print(f"No benchmark artifacts found in {BENCHMARKS_DIR}")
        sys.exit(1)

    by_collection = {}
    for f in files:
        data = validate_file(f)
        col = data["dataset_identity"]["collection"]
        by_collection.setdefault(col, []).append(data)

    # Monotonicity checks across progressive scale
    for col, entries in by_collection.items():
        entries.sort(key=lambda e: e["dataset_identity"]["game_count_requested"])
        for i in range(1, len(entries)):
            prev = entries[i - 1]
            curr = entries[i]
            prev_n = prev["dataset_identity"]["game_count_requested"]
            curr_n = curr["dataset_identity"]["game_count_requested"]
            prev_pos = prev["index_construction"]["unique_positions"]
            curr_pos = curr["index_construction"]["unique_positions"]
            prev_cpi = prev["index_construction"]["final_cpi_size_bytes"]
            curr_cpi = curr["index_construction"]["final_cpi_size_bytes"]

            if curr_pos < prev_pos:
                raise ValueError(
                    f"Collection {col}: Monotonicity violation: N={curr_n} positions ({curr_pos}) < N={prev_n} ({prev_pos})"
                )
            if curr_cpi < prev_cpi:
                raise ValueError(
                    f"Collection {col}: Monotonicity violation: N={curr_n} CPI size ({curr_cpi}) < N={prev_n} ({prev_cpi})"
                )

    print(f"\nAll {len(files)} production CPI benchmark artifacts are 100% VALID!")


if __name__ == "__main__":
    validate_all()
