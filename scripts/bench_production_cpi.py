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
        → Opening Explorer queries (warm & fresh-process latencies)

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
import threading
import time

ROOT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# Work directory for datasets, temporary runs and published candidates. Defaults to a repo-local directory on
# the real filesystem: /tmp is frequently tmpfs, where file contents and mmap'd pages are RAM-backed (shmem)
# and would distort both RSS and "temporary disk" measurements.
WORK_BASE = os.environ.get("CODB_BENCH_WORKDIR", os.path.join(ROOT_DIR, "target", "bench_work"))
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


def _ensure_work_base():
    os.makedirs(WORK_BASE, exist_ok=True)
    return WORK_BASE


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


def _sample_proc_status(pid, peaks, stop_event):
    """Polls /proc/<pid>/status (~every 5 ms) tracking the peak of each RSS component in KiB.

    RssAnon  = anonymous memory (heap/stack): what the indexer allocates and what --memory-limit-mib governs.
    RssFile  = resident pages of file-backed mappings (mmap'd CPI / CGB3 files): reclaimable page cache.
    """
    keys = {"VmRSS:": "vm_rss", "RssAnon:": "rss_anon", "RssFile:": "rss_file", "RssShmem:": "rss_shmem"}
    while not stop_event.is_set():
        try:
            with open(f"/proc/{pid}/status") as f:
                for line in f:
                    k = line.split(None, 1)[0]
                    if k in keys:
                        v = int(line.split()[1])
                        if v > peaks.get(keys[k], 0):
                            peaks[keys[k]] = v
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            break
        time.sleep(0.005)


def run_command_with_rusage(cmd_args):
    """
    Spawns cmd_args and captures peak RSS (ru_maxrss in KiB) using os.wait4, plus sampled peaks of
    RssAnon / RssFile from /proc/<pid>/status.
    Returns (exit_code, stdout_str, stderr_str, ru_maxrss_kib, elapsed_secs, proc_peaks_kib).
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
        peaks = {}
        stop_event = threading.Event()
        sampler = threading.Thread(target=_sample_proc_status, args=(pid, peaks, stop_event), daemon=True)
        sampler.start()
        _, status, rusage = os.wait4(pid, 0)
        elapsed = time.perf_counter() - start_time
        stop_event.set()
        sampler.join(timeout=2)

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
        return exit_code, stdout_str, stderr_str, rusage.ru_maxrss, elapsed, peaks


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
    Measures warm-cache and fresh-process Opening Explorer query latencies in microseconds.

    NOTE: the "fresh-process" series launches a new `codb query` subprocess per query (so index
    open/mmap/parse is included) but does NOT invalidate the OS page cache. It is therefore a
    cold-start latency, not a cold-cache latency. Genuine cold-cache measurement needs privileged
    `drop_caches` and is intentionally not attempted.
    """
    # 1. End-to-end fresh-process queries (including process launch + index open)
    fresh_latencies_us = []
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
            fresh_latencies_us.append((t1 - t0) / 1000.0)

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

    c_p50, c_p95, c_p99 = calc_percentiles(fresh_latencies_us)
    w_p50, w_p95, w_p99 = calc_percentiles(warm_latencies_us)

    return {
        "warm_p50_us": w_p50,
        "warm_p95_us": w_p95,
        "warm_p99_us": w_p99,
        "warm_samples": len(warm_latencies_us),
        "fresh_p50_us": c_p50,
        "fresh_p95_us": c_p95,
        "fresh_p99_us": c_p99,
        "fresh_samples": len(fresh_latencies_us),
    }


def prepare_dataset(collection, max_games, input_file, work_root):
    """Builds the canonical CGB3 dataset once; index configurations operate on copies."""
    dataset_dir = os.path.join(work_root, f"{collection}_{max_games}_base")
    print(f"1. Building CGB3 dataset ({collection}, max_games={max_games})...")
    t0 = time.perf_counter()
    build_cmd = [
        BIN_PATH, "build", "--input", input_file, "--collection", collection,
        "--max-games", str(max_games), "--output", dataset_dir,
    ]
    res = subprocess.run(build_cmd, capture_output=True, text=True)
    if res.returncode != 0:
        raise RuntimeError(f"codb build failed: {res.stderr}")
    dur = time.perf_counter() - t0
    size = compute_dir_size_bytes(dataset_dir)
    print(f"   CGB3 dataset built in {dur:.2f}s ({size} bytes)")
    return dataset_dir, size, dur


def run_single_benchmark(
    collection, max_games, input_file, depth_plies, base_dataset, cgb3_initial_bytes,
    memory_limit_mib=128, fan_in=64, scenario="default", source_description="",
):
    print(f"\n------------------------------------------------------------------------")
    print(f"  {collection} | requested N={max_games} | depth={depth_plies} | mem={memory_limit_mib} MiB | fan-in={fan_in} | {scenario}")
    print(f"------------------------------------------------------------------------")

    input_sha256 = compute_file_sha256(input_file)
    input_size_bytes = os.path.getsize(input_file)
    git_commit = get_git_commit_sha()

    work_dir = tempfile.mkdtemp(prefix=f"codb_bench_{collection}_{max_games}_", dir=_ensure_work_base())
    dataset_dir = os.path.join(work_dir, "dataset")
    try:
        shutil.copytree(base_dataset, dataset_dir, symlinks=True)

        print(f"2. Building production CPI index (external sort, mem_limit={memory_limit_mib} MiB, fan-in={fan_in})...")
        index_cmd = [
            BIN_PATH, "build-index", "--dataset", dataset_dir,
            "--depth-plies", str(depth_plies), "--collection", collection,
            "--memory-limit-mib", str(memory_limit_mib),
            "--max-merge-fan-in", str(fan_in), "--json",
        ]
        exit_code, stdout_str, stderr_str, peak_rss_kib, build_duration_secs, proc_peaks = (
            run_command_with_rusage(index_cmd)
        )
        if exit_code != 0:
            raise RuntimeError(f"codb build-index failed ({exit_code}): {stderr_str}")

        report = json.loads(stdout_str)
        peak_rss_bytes = peak_rss_kib * 1024
        peak_rss_mib = peak_rss_bytes / (1024 * 1024)
        peak_anon_mib = proc_peaks.get("rss_anon", 0) / 1024
        peak_file_mib = proc_peaks.get("rss_file", 0) / 1024

        print("3. Deep verifying dataset & position index...")
        t0 = time.perf_counter()
        v_res = subprocess.run(
            [BIN_PATH, "verify", "--dataset", dataset_dir, "--deep"],
            capture_output=True, text=True,
        )
        if v_res.returncode != 0:
            raise RuntimeError(f"codb verify --deep failed: {v_res.stderr}")
        verify_duration_secs = time.perf_counter() - t0

        print("4. Measuring Opening Explorer query latencies...")
        latencies = measure_query_latencies(dataset_dir)

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

        cpi_bytes_per_position = cpi_file_bytes / unique_positions if unique_positions > 0 else 0.0
        cpi_bytes_per_game = cpi_file_bytes / games_indexed if games_indexed > 0 else 0.0
        throughput_games_sec = games_processed / build_duration_secs if build_duration_secs > 0 else 0.0
        peak_total_disk_bytes = cgb3_initial_bytes + peak_temp_bytes + cpi_file_bytes

        scale_status = "target_reached" if games_processed >= max_games else "capped_by_source"

        print(f"   Processed {games_processed} of {max_games} requested ({scale_status}); indexed {games_indexed}")
        print(f"   Runs: {runs_generated} | Merge passes: {merge_passes}")
        print(f"   Build: {build_duration_secs:.2f}s ({throughput_games_sec:.1f} games/s)")
        print(f"   Unique positions: {unique_positions} | Pairs: {unique_pairs}")
        print(f"   Final CPI size:   {cpi_file_bytes:,} bytes ({cpi_bytes_per_position:.2f} B/pos)")
        print(f"   Peak RSS:         {peak_rss_mib:.2f} MiB total (ru_maxrss) | anon {peak_anon_mib:.2f} MiB | file-backed {peak_file_mib:.2f} MiB (configured limit: {memory_limit_mib} MiB)")
        print(f"   Peak Temp Disk:   {peak_temp_bytes:,} bytes")
        print(f"   Query Latencies:  Warm p50={latencies['warm_p50_us']:.1f}us, Fresh-process p50={latencies['fresh_p50_us']:.1f}us")

        result = {
            "schema_version": "2.0.0",
            "benchmark_type": "production_cpi_external_sort",
            "scenario": scenario,
            "git_commit": git_commit,
            "dataset_identity": {
                "collection": collection,
                "game_count_requested": max_games,
                "game_count_processed": games_processed,
                "scale_status": scale_status,
                "input_file": os.path.relpath(input_file, ROOT_DIR),
                "input_sha256": input_sha256,
                "input_size_bytes": input_size_bytes,
                "source_description": source_description,
                "published_build_id": report["published_build_id"],
                "configuration": {
                    "depth_plies": depth_plies,
                    "memory_limit_mib": memory_limit_mib,
                    "memory_limit_bytes": memory_limit_mib * 1024 * 1024,
                    "max_merge_fan_in": fan_in,
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
                "peak_rss_to_limit_ratio": round(peak_rss_mib / memory_limit_mib, 3),
                "rss_within_configured_limit": peak_rss_mib <= memory_limit_mib,
                "sampled_peak_rss_anon_mib": round(peak_anon_mib, 2),
                "sampled_peak_rss_file_mib": round(peak_file_mib, 2),
                "sampled_peak_rss_anon_kib": proc_peaks.get("rss_anon", 0),
                "sampled_peak_rss_file_kib": proc_peaks.get("rss_file", 0),
                "rss_sampling_note": "ru_maxrss is the kernel high-water mark of total RSS (anon + file-backed + shmem). Anon/file peaks are sampled from /proc/<pid>/status every ~5 ms and are lower bounds of the true component peaks.",
                "anon_within_configured_limit": peak_anon_mib <= memory_limit_mib,
                "peak_temporary_disk_bytes": peak_temp_bytes,
                "final_cpi_size_bytes": cpi_file_bytes,
                "cpi_sha256": report["cpi_sha256"],
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
                    "methodology": "Repeated `codb query` subprocess invocations over common opening move sequences (e4, d4, c4, Nf3) after the CPI file was recently written/read (OS page cache warm). Includes process launch and index open.",
                },
                "fresh_process_cold_start": {
                    "p50_latency_us": round(latencies["fresh_p50_us"], 1),
                    "p95_latency_us": round(latencies["fresh_p95_us"], 1),
                    "p99_latency_us": round(latencies["fresh_p99_us"], 1),
                    "sample_size": latencies["fresh_samples"],
                    "methodology": "First five queries, each in a new subprocess (full CLI startup, dataset resolution, mmap, binary search, JSON formatting). The OS page cache is NOT invalidated, so this is cold-start / fresh-process latency, not cold-cache latency.",
                },
                "os_page_cache_invalidated": False,
                "index_open_included_in_query_timing": True,
            },
        }

        os.makedirs(OUTPUT_DIR, exist_ok=True)
        out_name = f"{collection}_{max_games}_m{memory_limit_mib}_f{fan_in}.json"
        out_path = os.path.join(OUTPUT_DIR, out_name)
        with open(out_path, "w") as f:
            json.dump(result, f, indent=2)
            f.write("\n")
        print(f"Saved benchmark result to {out_path}")
        return result
    finally:
        shutil.rmtree(work_dir, ignore_errors=True)


def pick_input(env_var, candidates):
    override = os.environ.get(env_var)
    if override:
        return override
    for c in candidates:
        p = os.path.join(ROOT_DIR, "data", c)
        if os.path.isfile(p):
            return p
    return None


def main():
    parser = argparse.ArgumentParser(description="Production CPI Benchmarking Runner")
    parser.add_argument(
        "--mode",
        choices=["ci", "scale", "stress", "all"],
        default="all",
        help="ci: 1k smoke test; scale: 20k/50k/100k at 128 MiB; stress: 100k at 16/32 MiB and fan-in 2; all: ci + scale + stress",
    )
    parser.add_argument("--masters-file", default=None)
    parser.add_argument("--elite-file", default=None)
    parser.add_argument("--only", choices=["masters-otb", "elite-online"], default=None)
    args = parser.parse_args()

    masters_file = args.masters_file or pick_input(
        "CODB_MASTERS_FILE", ["masters_broadcast_2025-01_2026-09.pgn.zst", "masters_combined.pgn.zst"]
    )
    elite_file = args.elite_file or pick_input(
        "CODB_ELITE_FILE", ["elite_online_2025-01_110k.pgn.zst", "elite_online_sample.pgn.zst"]
    )
    if not masters_file or not elite_file or not os.path.isfile(masters_file) or not os.path.isfile(elite_file):
        print("Error: required datasets missing in data/ (see docs/research/benchmark-reproduction.md section 6)")
        sys.exit(1)

    collections = [
        ("masters-otb", 40, masters_file, "Lichess official broadcasts (CC0), curated by MastersOtbPolicy"),
        ("elite-online", 30, elite_file, "Lichess standard rated 2025-01 (CC0), curated by EliteOnlinePolicy, exported as canonical PGN"),
    ]
    if args.only:
        collections = [c for c in collections if c[0] == args.only]

    # (requested N, memory MiB, fan-in, scenario)
    plan = []
    if args.mode in ("ci", "all"):
        plan.append((1000, 128, 64, "ci_smoke"))
    if args.mode in ("scale", "all"):
        for n in (20000, 50000, 100000):
            plan.append((n, 128, 64, "scale_default"))
    if args.mode in ("stress", "all"):
        plan.append((100000, 32, 64, "stress_mem32"))
        plan.append((100000, 16, 64, "stress_mem16"))
        plan.append((100000, 128, 2, "stress_fanin2"))
        plan.append((100000, 16, 2, "stress_mem16_fanin2"))

    work_root = tempfile.mkdtemp(prefix="codb_bench_root_", dir=_ensure_work_base())
    all_results = []
    try:
        for col, depth, path, desc in collections:
            bases = {}
            for n, mem, fan, scenario in plan:
                if n not in bases:
                    bases[n] = prepare_dataset(col, n, path, work_root)
                base_dir, base_bytes, _ = bases[n]
                all_results.append(
                    run_single_benchmark(col, n, path, depth, base_dir, base_bytes, mem, fan, scenario, desc)
                )
    finally:
        shutil.rmtree(work_root, ignore_errors=True)

    print("\n" + "=" * 118)
    print("PRODUCTION CPI BENCHMARK SUMMARY")
    print("=" * 118)
    header = f"{'Collection':<13}|{'Scenario':<20}|{'Req N':>7}|{'Proc N':>7}|{'Mem':>4}|{'Fan':>4}|{'Runs':>5}|{'Pass':>5}|{'UniqPos':>10}|{'CPI B/pos':>9}|{'RSS MiB':>8}|{'Temp MB':>8}|{'Time s':>7}"
    print(header)
    print("-" * len(header))
    for r in all_results:
        di = r["dataset_identity"]
        ic = r["index_construction"]
        cfg = di["configuration"]
        print(
            f"{di['collection']:<13}|{r['scenario']:<20}|{di['game_count_requested']:>7}|{di['game_count_processed']:>7}|"
            f"{cfg['memory_limit_mib']:>4}|{cfg['max_merge_fan_in']:>4}|{ic['runs_generated']:>5}|{ic['merge_passes']:>5}|"
            f"{ic['unique_positions']:>10,}|{ic['cpi_bytes_per_unique_position']:>9.2f}|{ic['peak_process_rss_mib']:>8.1f}|"
            f"{ic['peak_temporary_disk_bytes'] / 1e6:>8.1f}|{ic['build_duration_secs']:>7.2f}"
        )
    print("=" * 118)


if __name__ == "__main__":
    main()
