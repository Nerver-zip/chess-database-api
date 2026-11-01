#!/usr/bin/env python3
"""
scripts/validate_production_cpi_benchmarks.py — Schema, Accounting & Memory Budget Validator
for Production CPI Benchmarks in benchmarks/production_cpi/.
"""

import glob
import json
import os
import sys

# Contract: --memory-limit-mib bounds the indexer's anonymous (heap) memory. A fixed allowance covers
# runtime state outside the budgeted buffers (allocator slack, thread stacks, zstd contexts, stdio).
# File-backed pages of mmap'd CPI/CGB3 files are reclaimable page cache and are reported, not gated.
FIXED_ANON_OVERHEAD_MIB = 8

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
    "rss_within_configured_limit",
    "peak_rss_to_limit_ratio",
    "sampled_peak_rss_anon_mib",
    "sampled_peak_rss_file_mib",
    "anon_within_configured_limit",
    "cpi_sha256",
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
    if d["schema_version"] != "2.0.0":
        raise ValueError(f"{path}: Unsupported schema_version {d['schema_version']} (expected 2.0.0)")
    if "scenario" not in d:
        raise ValueError(f"{path}: Missing 'scenario'")

    ident = d["dataset_identity"]
    for field in ("game_count_requested", "game_count_processed", "scale_status", "input_sha256"):
        if field not in ident:
            raise ValueError(f"{path}: Missing dataset_identity field '{field}'")
    req, proc = ident["game_count_requested"], ident["game_count_processed"]
    if proc > req:
        raise ValueError(f"{path}: processed ({proc}) exceeds requested ({req})")
    expected_status = "target_reached" if proc >= req else "capped_by_source"
    if ident["scale_status"] != expected_status:
        raise ValueError(f"{path}: scale_status {ident['scale_status']} inconsistent with counts ({proc}/{req})")

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

    # 4. Memory contract: sampled peak anonymous RSS must stay within configured limit + fixed allowance.
    mem_budget_mib = d["dataset_identity"]["configuration"]["memory_limit_mib"]
    peak_rss_mib = c["peak_process_rss_mib"]
    anon_mib = c["sampled_peak_rss_anon_mib"]
    if c["rss_within_configured_limit"] != (peak_rss_mib <= mem_budget_mib):
        raise ValueError(f"{path}: rss_within_configured_limit flag inconsistent with measured RSS")
    if c["anon_within_configured_limit"] != (anon_mib <= mem_budget_mib):
        raise ValueError(f"{path}: anon_within_configured_limit flag inconsistent with measured anon RSS")
    if anon_mib > mem_budget_mib + FIXED_ANON_OVERHEAD_MIB:
        raise ValueError(
            f"{path}: sampled peak anonymous RSS {anon_mib} MiB exceeds configured limit {mem_budget_mib} MiB "
            f"+ fixed allowance {FIXED_ANON_OVERHEAD_MIB} MiB"
        )

    if c["runs_generated"] < 1 or c["merge_passes"] < 1:
        raise ValueError(f"{path}: runs_generated/merge_passes must be >= 1")
    fan_in = d["dataset_identity"]["configuration"]["max_merge_fan_in"]
    if c["runs_generated"] > fan_in and c["merge_passes"] < 2:
        raise ValueError(f"{path}: {c['runs_generated']} runs with fan-in {fan_in} requires >= 2 merge passes")

    # 5. Verification status
    if not c["verification_passed"]:
        raise ValueError(f"{path}: verification_passed is False")

    # 6. Latency percentile invariants
    qp = d["query_performance"]
    warm = qp["warm_cache"]
    cold = qp["fresh_process_cold_start"]
    if qp.get("os_page_cache_invalidated") is not False:
        raise ValueError(f"{path}: os_page_cache_invalidated must be recorded as False (no drop_caches performed)")
    if not (warm["p50_latency_us"] <= warm["p95_latency_us"] <= warm["p99_latency_us"]):
        raise ValueError(f"{path}: warm latency ordering violated: {warm}")
    if not (cold["p50_latency_us"] <= cold["p95_latency_us"] <= cold["p99_latency_us"]):
        raise ValueError(f"{path}: fresh-process latency ordering violated: {cold}")

    return d


def validate_all():
    files = sorted(glob.glob(os.path.join(BENCHMARKS_DIR, "*.json")))
    if not files:
        print(f"No benchmark artifacts found in {BENCHMARKS_DIR}")
        sys.exit(1)

    by_collection = {}
    data_all = []
    for f in files:
        data = validate_file(f)
        data_all.append(data)
        col = data["dataset_identity"]["collection"]
        by_collection.setdefault(col, []).append(data)

    # Monotonicity across processed scale (default-config scenarios only)
    for col, entries in by_collection.items():
        base = [e for e in entries if e["dataset_identity"]["configuration"]["memory_limit_mib"] == 128
                and e["dataset_identity"]["configuration"]["max_merge_fan_in"] == 64]
        base.sort(key=lambda e: e["dataset_identity"]["game_count_processed"])
        for i in range(1, len(base)):
            prev, curr = base[i - 1], base[i]
            pc, cc = prev["index_construction"], curr["index_construction"]
            if cc["unique_positions"] < pc["unique_positions"] or cc["final_cpi_size_bytes"] < pc["final_cpi_size_bytes"]:
                raise ValueError(f"Collection {col}: monotonicity violation between requested N={prev['dataset_identity']['game_count_requested']} and N={curr['dataset_identity']['game_count_requested']}")

    # Determinism: identical (collection, requested N, input checksum, depth) must give identical CPI bytes
    groups = {}
    for e in data_all:
        i = e["dataset_identity"]
        key = (i["collection"], i["game_count_requested"], i["input_sha256"], i["configuration"]["depth_plies"])
        groups.setdefault(key, set()).add(e["index_construction"]["cpi_sha256"])
    for key, shas in groups.items():
        if len(shas) != 1:
            raise ValueError(f"Non-deterministic CPI output across memory/fan-in configs for {key}: {sorted(shas)}")

    print(f"\nAll {len(files)} production CPI benchmark artifacts are 100% VALID!")


if __name__ == "__main__":
    validate_all()
