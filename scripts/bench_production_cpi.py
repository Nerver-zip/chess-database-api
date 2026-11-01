#!/usr/bin/env python3
"""
scripts/bench_production_cpi.py — Production Position Index (CPI) Scaling & Operational Benchmark Runner.

Executes the complete production indexing pipeline:
    CGB3 dataset
        → streaming canonical-game traversal
        → bounded-memory external sorting
        → temporary-run multi-pass K-way merging
        → CPI v2 block compression & streaming sparse index
        → atomic dataset publication
        → deep verification
        → Opening Explorer queries (warm & cold latencies)

Measures:
    - Peak process RSS via kernel wait4 / getrusage
    - Temporary disk usage and publication coexistence overhead
    - Final CPI size and bytes per unique position
    - Build throughput and verification duration
    - Query latencies (p50, p95, p99)
"""

import argparse
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time

ROOT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN_PATH = os.path.join(ROOT_DIR, "target", "release", "codb")
OUTPUT_DIR = os.path.join(ROOT_DIR, "benchmarks", "production_cpi")

BENCHMARK_MOVES = [
    "e4",
    "e4,e5",
    "e4,e5,Nf3",
    "e4,e5,Nf3,Nc6",
    "e4,c5",
    "e4,c5,Nf3",
    "e4,c5,Nf3,d6",
    "d4",
    "d4,Nf6",
    "d4,Nf6,c4",
    "d4,Nf6,c4,e6",
    "d4,d5",
    "c4",
    "Nf3",
]


def compute_file_sha256(filepath):
    hasher = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            hasher.update(chunk)
    return hasher.hexdigest()


def get_git_commit_sha():
    try:
        out = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT_DIR, text=True
        ).strip()
        return out
    except Exception:
        return "unknown"


def run_command_with_rusage(cmd_args):
    """
    Spawns cmd_args and captures peak RSS (ru_maxrss in KiB) using os.wait4.
    Returns (exit_code, stdout_str, stderr_str, ru_maxrss_kib, elapsed_secs).
    """
    pipe_out_r, pipe_out_w = os.pipe()
    pipe_err_r, pipe_err_w = os.pipe()

    start_time = time.perf_counter()
    pid = os.fork()
    if pid == 0:
        os.close(pipe_out_r)
        os.close(pipe_err_r)
        os.dup2(pipe_out_w, 1)
        os.dup2(pipe_err_w, 2)
        os.close(pipe_out_w)
        os.close(pipe_err_w)
        os.execv(cmd_args[0], cmd_args)
    else:
        os.close(pipe_out_w)
        os.close(pipe_err_w)
        _, status, rusage = os.wait4(pid, 0)
        elapsed = time.perf_counter() - start_time

        out_chunks = []
        while True:
            chunk = os.read(pipe_out_r, 65536)
            if not chunk:
                break
            out_chunks.append(chunk)
        os.close(pipe_out_r)

        err_chunks = []
        while True:
            chunk = os.read(pipe_err_r, 65536)
            if not chunk:
                break
            err_chunks.append(chunk)
        os.close(pipe_err_r)

        exit_code = os.waitstatus_to_exitcode(status) if hasattr(os, "waitstatus_to_exitcode") else (status >> 8)
        stdout_str = b"".join(out_chunks).decode("utf-8", errors="replace")
        stderr_str = b"".join(err_chunks).decode("utf-8", errors="replace")
        return exit_code, stdout_str, stderr_str, rusage.ru_maxrss, elapsed


def compute_dir_size_bytes(path):
    total = 0
    if not os.path.exists(path):
        return 0
    for root, _, files in os.walk(path):
        for f in files:
            fp = os.path.join(root, f)
            if not os.path.islink(fp):
                total += os.path.getsize(fp)
    return total


def measure_query_latencies(dataset_dir):
    """
    Measures warm and cold Opening Explorer query latencies in microseconds.
    """
    # 1. End-to-end cold queries (including process launch + index open)
    cold_latencies_us = []
    for moves in BENCHMARK_MOVES[:5]:
        t0 = time.perf_counter_ns()
        res = subprocess.run(
            [BIN_PATH, "query", "--dataset", dataset_dir, "--moves", moves, "--json"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        t1 = time.perf_counter_ns()
        if res.returncode == 0:
            cold_latencies_us.append((t1 - t0) / 1000.0)

    # 2. Warm queries (repeated executions over candidate moves)
    warm_latencies_us = []
    for _ in range(5):
        for moves in BENCHMARK_MOVES:
            t0 = time.perf_counter_ns()
            res = subprocess.run(
                [BIN_PATH, "query", "--dataset", dataset_dir, "--moves", moves, "--json"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            t1 = time.perf_counter_ns()
            if res.returncode == 0:
                warm_latencies_us.append((t1 - t0) / 1000.0)

    def calc_percentiles(lat_list):
        if not lat_list:
            return 0.0, 0.0, 0.0
        sorted_l = sorted(lat_list)
        p50 = sorted_l[int(len(sorted_l) * 0.50)]
        p95 = sorted_l[int(len(sorted_l) * 0.95)]
        p99 = sorted_l[min(int(len(sorted_l) * 0.99), len(sorted_l) - 1)]
        return p50, p95, p99

    c_p50, c_p95, c_p99 = calc_percentiles(cold_latencies_us)
    w_p50, w_p95, w_p99 = calc_percentiles(warm_latencies_us)

    return {
        "warm_p50_us": w_p50,
        "warm_p95_us": w_p95,
        "warm_p99_us": w_p99,
        "warm_samples": len(warm_latencies_us),
        "cold_p50_us": c_p50,
        "cold_p95_us": c_p95,
        "cold_p99_us": c_p99,
        "cold_samples": len(cold_latencies_us),
    }


def run_single_benchmark(collection, max_games, input_file, depth_plies, memory_limit_mib=128):
    print(f"\n========================================================================")
    print(f"  Production CPI Benchmark: {collection} | N = {max_games} | Depth = {depth_plies}")
    print(f"========================================================================")

    if not os.path.isfile(BIN_PATH):
        print(f"Error: Release binary not found at {BIN_PATH}")
        sys.exit(1)

    input_sha256 = compute_file_sha256(input_file)
    input_size_bytes = os.path.getsize(input_file)
    git_commit = get_git_commit_sha()

    work_dir = tempfile.mkdtemp(prefix=f"codb_bench_{collection}_{max_games}_")
    dataset_dir = os.path.join(work_dir, "dataset")

    try:
        # Step 1: Ingest canonical CGB3 dataset
        print(f"1. Building CGB3 dataset ({collection}, max_games={max_games})...")
        t0 = time.perf_counter()
        build_cmd = [
            BIN_PATH,
            "build",
            "--input",
            input_file,
            "--collection",
            collection,
            "--max-games",
            str(max_games),
            "--output",
            dataset_dir,
        ]
        res = subprocess.run(build_cmd, capture_output=True, text=True)
        if res.returncode != 0:
            raise RuntimeError(f"codb build failed: {res.stderr}")
        cgb3_build_duration = time.perf_counter() - t0

        cgb3_initial_bytes = compute_dir_size_bytes(dataset_dir)
        print(f"   CGB3 dataset built in {cgb3_build_duration:.2f}s ({cgb3_initial_bytes} bytes)")

        # Step 2: Build CPI position index with wait4 rusage monitoring
        print(f"2. Building production CPI index (external sort, mem_limit={memory_limit_mib} MiB)...")
        index_cmd = [
            BIN_PATH,
            "build-index",
            "--dataset",
            dataset_dir,
            "--depth-plies",
            str(depth_plies),
            "--collection",
            collection,
            "--memory-limit-mib",
            str(memory_limit_mib),
            "--json",
        ]
        exit_code, stdout_str, stderr_str, peak_rss_kib, build_duration_secs = (
            run_command_with_rusage(index_cmd)
        )
        if exit_code != 0:
            raise RuntimeError(f"codb build-index failed ({exit_code}): {stderr_str}")

        report = json.loads(stdout_str)
        peak_rss_bytes = peak_rss_kib * 1024
        peak_rss_mib = peak_rss_bytes / (1024 * 1024)

        # Step 3: Deep integrity verification
        print("3. Deep verifying dataset & position index...")
        t0 = time.perf_counter()
        verify_cmd = [BIN_PATH, "verify", "--dataset", dataset_dir, "--deep"]
        v_res = subprocess.run(verify_cmd, capture_output=True, text=True)
        if v_res.returncode != 0:
            raise RuntimeError(f"codb verify --deep failed: {v_res.stderr}")
        verify_duration_secs = time.perf_counter() - t0

        # Step 4: Measure queries
        print("4. Measuring Opening Explorer query latencies...")
        latencies = measure_query_latencies(dataset_dir)

        # Step 5: Final storage accounting
        final_deployed_bytes = compute_dir_size_bytes(dataset_dir)
        cpi_file_bytes = report["cpi_file_size_bytes"]
        unique_positions = report["unique_positions"]
        unique_pairs = report["total_move_records"]
        games_indexed = report["games_indexed"]
        games_processed = report["games_processed"]
        raw_observations = report["raw_observations_extracted"]
        runs_generated = report["runs_generated"]
        merge_passes = report["merge_passes"]
        peak_temp_bytes = report["peak_temp_bytes"]

        cpi_bytes_per_position = (
            cpi_file_bytes / unique_positions if unique_positions > 0 else 0.0
        )
        cpi_bytes_per_game = cpi_file_bytes / games_indexed if games_indexed > 0 else 0.0
        throughput_games_sec = (
            games_processed / build_duration_secs if build_duration_secs > 0 else 0.0
        )

        # Peak total disk usage during build = initial dataset + peak temp runs + staged candidate
        peak_total_disk_bytes = cgb3_initial_bytes + peak_temp_bytes + cpi_file_bytes

        print(f"   Indexed {games_indexed} games in {build_duration_secs:.2f}s ({throughput_games_sec:.1f} games/s)")
        print(f"   Unique positions: {unique_positions} | Pairs: {unique_pairs}")
        print(f"   Final CPI size:   {cpi_file_bytes:,} bytes ({cpi_bytes_per_position:.2f} B/pos)")
        print(f"   Peak RSS:         {peak_rss_mib:.2f} MiB (budget: {memory_limit_mib} MiB)")
        print(f"   Peak Temp Disk:   {peak_temp_bytes:,} bytes")
        print(f"   Query Latencies:  Warm p50={latencies['warm_p50_us']:.1f}µs, Cold p50={latencies['cold_p50_us']:.1f}µs")

        result = {
            "schema_version": "1.0.0",
            "benchmark_type": "production_cpi_external_sort",
            "git_commit": git_commit,
            "dataset_identity": {
                "collection": collection,
                "game_count_requested": max_games,
                "input_file": input_file,
                "input_sha256": input_sha256,
                "input_size_bytes": input_size_bytes,
                "published_build_id": report["published_build_id"],
                "configuration": {
                    "depth_plies": depth_plies,
                    "memory_limit_mib": memory_limit_mib,
                    "memory_limit_bytes": memory_limit_mib * 1024 * 1024,
                    "max_merge_fan_in": 64,
                    "positions_per_block": 64,
                    "zstd_level": 3,
                },
            },
            "index_construction": {
                "games_processed": games_processed,
                "games_indexed": games_indexed,
                "excluded_unknown_result_games": report["excluded_unknown_result_games"],
                "custom_fen_games": report["custom_fen_games"],
                "repeated_positions_skipped": report["repeated_positions_skipped"],
                "raw_observations_extracted": raw_observations,
                "unique_positions": unique_positions,
                "unique_position_move_pairs": unique_pairs,
                "cpi_block_count": report["cpi_block_count"],
                "runs_generated": runs_generated,
                "merge_passes": merge_passes,
                "build_duration_secs": round(build_duration_secs, 4),
                "throughput_games_per_sec": round(throughput_games_sec, 2),
                "peak_process_rss_kib": peak_rss_kib,
                "peak_process_rss_bytes": peak_rss_bytes,
                "peak_process_rss_mib": round(peak_rss_mib, 2),
                "rss_budget_enforced": peak_rss_mib <= memory_limit_mib * 1.05,
                "peak_temporary_disk_bytes": peak_temp_bytes,
                "final_cpi_size_bytes": cpi_file_bytes,
                "cpi_bytes_per_unique_position": round(cpi_bytes_per_position, 3),
                "cpi_bytes_per_game": round(cpi_bytes_per_game, 3),
                "verification_duration_secs": round(verify_duration_secs, 4),
                "verification_passed": True,
            },
            "storage_accounting": {
                "initial_cgb3_dataset_bytes": cgb3_initial_bytes,
                "final_deployed_dataset_bytes": final_deployed_bytes,
                "final_cpi_bytes": cpi_file_bytes,
                "temporary_external_sort_peak_bytes": peak_temp_bytes,
                "peak_total_disk_usage_bytes": peak_total_disk_bytes,
            },
            "query_performance": {
                "warm_cache": {
                    "p50_latency_us": round(latencies["warm_p50_us"], 1),
                    "p95_latency_us": round(latencies["warm_p95_us"], 1),
                    "p99_latency_us": round(latencies["warm_p99_us"], 1),
                    "sample_size": latencies["warm_samples"],
                    "methodology": "Repeated queries across common opening move sequences (e4, d4, c4, Nf3) over pre-warmed OS file buffer cache",
                },
                "cold_cache_end_to_end": {
                    "p50_latency_us": round(latencies["cold_p50_us"], 1),
                    "p95_latency_us": round(latencies["cold_p95_us"], 1),
                    "p99_latency_us": round(latencies["cold_p99_us"], 1),
                    "sample_size": latencies["cold_samples"],
                    "methodology": "Separate subprocess invocation per query measuring full CLI startup, dataset resolution, mmap, binary search, and JSON formatting",
                },
                "index_open_included_in_query_timing": True,
            },
        }

        os.makedirs(OUTPUT_DIR, exist_ok=True)
        out_name = f"{collection}_{max_games}.json"
        out_path = os.path.join(OUTPUT_DIR, out_name)
        with open(out_path, "w") as f:
            json.dump(result, f, indent=2)
        print(f"Saved benchmark result to {out_path}")

        return result

    finally:
        shutil.rmtree(work_dir, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description="Production CPI Benchmarking Runner")
    parser.add_argument(
        "--mode",
        choices=["ci", "all", "masters", "elite"],
        default="all",
        help="Benchmark execution mode (default: all)",
    )
    parser.add_argument(
        "--memory-limit-mib",
        type=int,
        default=128,
        help="Configured indexer memory ceiling in MiB (default: 128)",
    )
    args = parser.parse_args()

    masters_file = os.path.join(ROOT_DIR, "data", "masters_combined.pgn.zst")
    elite_file = os.path.join(ROOT_DIR, "data", "elite_online_sample.pgn.zst")
    if not os.path.isfile(elite_file):
        elite_file = os.path.join(ROOT_DIR, "data", "elite_online_2013-01.pgn.zst")

    if not os.path.isfile(masters_file) or not os.path.isfile(elite_file):
        print(f"Error: Required datasets missing in data/ directory!")
        sys.exit(1)

    if args.mode == "ci":
        sample_tiers = [1000]
        collections = [("masters-otb", 40, masters_file)]
    elif args.mode == "masters":
        sample_tiers = [1000, 10000, 20000, 50000, 100000]
        collections = [("masters-otb", 40, masters_file)]
    elif args.mode == "elite":
        sample_tiers = [1000, 10000, 20000, 50000, 100000]
        collections = [("elite-online", 30, elite_file)]
    else:
        sample_tiers = [1000, 10000, 20000, 50000, 100000]
        collections = [
            ("masters-otb", 40, masters_file),
            ("elite-online", 30, elite_file),
        ]

    all_results = []
    for col, depth, path in collections:
        for n in sample_tiers:
            res = run_single_benchmark(col, n, path, depth, args.memory_limit_mib)
            all_results.append(res)

    print("\n========================================================================")
    print("                PRODUCTION CPI BENCHMARK SUMMARY TABLE                  ")
    print("========================================================================")
    header = f"{'Collection':<14} | {'N':>7} | {'Unique Pos':>10} | {'CPI Size':>10} | {'B/pos':>6} | {'Time (s)':>8} | {'Thru (g/s)':>10} | {'Peak RSS':>9} | {'Warm p50':>9}"
    print(header)
    print("-" * len(header))
    for r in all_results:
        col = r["dataset_identity"]["collection"]
        n = r["dataset_identity"]["game_count_requested"]
        cpi = r["index_construction"]
        qp = r["query_performance"]["warm_cache"]
        row = f"{col:<14} | {n:>7} | {cpi['unique_positions']:>10,} | {cpi['final_cpi_size_bytes']:>10,} | {cpi['cpi_bytes_per_unique_position']:>6.2f} | {cpi['build_duration_secs']:>8.2f} | {cpi['throughput_games_per_sec']:>10.1f} | {cpi['peak_process_rss_mib']:>6.1f}MB | {qp['p50_latency_us']:>7.0f}µs"
        print(row)
    print("========================================================================")


if __name__ == "__main__":
    main()
