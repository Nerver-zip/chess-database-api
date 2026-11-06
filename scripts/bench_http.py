#!/usr/bin/env python3
"""
scripts/bench_http.py — HTTP Opening Explorer API Performance & Concurrency Benchmark Harness.

Measures:
1. Server startup time and initial memory RSS.
2. Single-client latencies (p50, p95, p99) for common, rare, absent positions and game retrieval.
3. Multi-client concurrent throughput (req/s) and latency under scaling worker concurrency (1, 4, 8, 16).
4. Zero-downtime snapshot replacement latency impact and error rate during live concurrent traffic.
5. Peak process RSS memory scaling during sustained query workloads.
"""

import http.client
import json
import os
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import urlencode


def read_sample_game_ids(dataset_dir: str, count: int = 10) -> list[int]:
    """Reads sample valid 64-bit game IDs from games.idx."""
    idx_path = os.path.join(dataset_dir, "games.idx")
    if not os.path.exists(idx_path):
        current_sym = os.path.join(dataset_dir, "current")
        if os.path.exists(current_sym):
            idx_path = os.path.join(current_sym, "games.idx")
    ids = []
    if os.path.exists(idx_path):
        with open(idx_path, "rb") as f:
            f.seek(32)  # Skip 32-byte header
            while len(ids) < count:
                buf = f.read(16)
                if len(buf) < 16:
                    break
                game_id = struct.unpack(">Q", buf[:8])[0]
                ids.append(game_id)
    return ids


def read_process_rss_kb(pid: int) -> int:
    """Reads VmRSS in KB from /proc/<pid>/status on Linux."""
    try:
        with open(f"/proc/{pid}/status", "r") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    parts = line.split()
                    return int(parts[1])
    except Exception:
        pass
    return 0


def compute_percentiles(times_us: list[float]) -> dict:
    if not times_us:
        return {"p50_us": 0.0, "p95_us": 0.0, "p99_us": 0.0, "min_us": 0.0, "max_us": 0.0}
    s = sorted(times_us)
    n = len(s)
    p50 = s[int(n * 0.50)]
    p95 = s[min(int(n * 0.95), n - 1)]
    p99 = s[min(int(n * 0.99), n - 1)]
    return {
        "p50_us": round(p50, 2),
        "p95_us": round(p95, 2),
        "p99_us": round(p99, 2),
        "min_us": round(s[0], 2),
        "max_us": round(s[-1], 2),
    }


def make_http_request(host: str, port: int, method: str, path: str) -> tuple[int, float, bytes]:
    t0 = time.perf_counter()
    conn = http.client.HTTPConnection(host, port, timeout=10)
    try:
        conn.request(method, path)
        resp = conn.getresponse()
        data = resp.read()
        elapsed_us = (time.perf_counter() - t0) * 1_000_000.0
        return resp.status, elapsed_us, data
    finally:
        conn.close()


def run_benchmark(bin_path: str, fixture_pgn: str, output_json: str | None = None) -> dict:
    temp_dir = tempfile.mkdtemp(prefix="codb_http_bench_")
    dataset_dir = os.path.join(temp_dir, "masters_dataset")
    port = 18888
    host = "127.0.0.1"

    print("========================================================================")
    print("      CODB Opening Explorer HTTP API Production Benchmark Suite         ")
    print("========================================================================")

    # 1. Build persistent CGB3 dataset and CPI index
    print(f"[1/6] Building dataset from {fixture_pgn}...")
    subprocess.run(
        [bin_path, "build", "--input", fixture_pgn, "--collection", "masters-otb", "--output", dataset_dir],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    subprocess.run(
        [bin_path, "build-index", "--dataset", dataset_dir, "--depth-plies", "40", "--collection", "masters-otb"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    print("  Dataset and CPI index initialized successfully.")

    # 2. Launch HTTP server and measure startup time & RSS
    print(f"[2/6] Starting codb serve on {host}:{port}...")
    t_start = time.perf_counter()
    proc = subprocess.Popen(
        [
            bin_path,
            "serve",
            "--masters-dataset",
            dataset_dir,
            "--host",
            host,
            "--port",
            str(port),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    # Wait for /ready
    ready = False
    startup_ms = 0.0
    for _ in range(50):
        time.sleep(0.05)
        try:
            status, _, _ = make_http_request(host, port, "GET", "/ready")
            if status == 200:
                startup_ms = (time.perf_counter() - t_start) * 1000.0
                ready = True
                break
        except Exception:
            pass

    if not ready:
        proc.kill()
        shutil.rmtree(temp_dir, ignore_errors=True)
        raise RuntimeError("Server failed to become ready within timeout")

    initial_rss_kb = read_process_rss_kb(proc.pid)
    print(f"  Server ready in {startup_ms:.2f} ms (Initial RSS: {initial_rss_kb / 1024:.2f} MB)")

    peak_rss_kb = initial_rss_kb

    def update_rss():
        nonlocal peak_rss_kb
        cur = read_process_rss_kb(proc.pid)
        if cur > peak_rss_kb:
            peak_rss_kb = cur

    sample_ids = read_sample_game_ids(dataset_dir, 5)
    sample_id = sample_ids[0] if sample_ids else 1
    print(f"  Sample game ID for retrieval: {sample_id}")

    # 3. Single-client latency benchmark across representative query distribution
    print("[3/6] Measuring single-client baseline latencies (100 iterations per category)...")
    sc_categories = {
        "common_position_e4": "/v1/explorer/position?collection=masters-otb&moves=e4",
        "common_position_e4_e5": "/v1/explorer/position?collection=masters-otb&moves=e4,e5",
        "common_position_d4": "/v1/explorer/position?collection=masters-otb&moves=d4",
        "absent_position": "/v1/explorer/position?collection=masters-otb&moves=h4,a5,h5,h6,Rh3",
        "game_retrieval": f"/v1/collections/masters-otb/games/{sample_id}",
    }

    sc_results = {}
    for name, path in sc_categories.items():
        times = []
        for _ in range(100):
            st, elapsed, _ = make_http_request(host, port, "GET", path)
            assert st == 200, f"Expected 200, got {st} for {path}"
            times.append(elapsed)
        update_rss()
        sc_results[name] = compute_percentiles(times)
        print(f"  {name:25s} -> p50: {sc_results[name]['p50_us']:7.1f} µs | p95: {sc_results[name]['p95_us']:7.1f} µs | p99: {sc_results[name]['p99_us']:7.1f} µs")

    # 4. Multi-client concurrent throughput & scaling (1, 4, 8, 16 workers)
    print("[4/6] Measuring concurrent throughput scaling across worker threads (1, 4, 8, 16)...")
    workload_paths = [
        "/v1/explorer/position?collection=masters-otb&moves=e4",
        "/v1/explorer/position?collection=masters-otb&moves=e4,e5",
        "/v1/explorer/position?collection=masters-otb&moves=d4",
        "/v1/explorer/position?collection=masters-otb&moves=h4,a5,h5,h6,Rh3",
        f"/v1/collections/masters-otb/games/{sample_id}",
    ]

    concurrency_results = {}
    for workers in [1, 4, 8, 16]:
        req_count_per_worker = 150
        all_times = []
        errors = 0
        lock = threading.Lock()

        def worker_fn(w_id: int):
            nonlocal errors
            local_times = []
            for i in range(req_count_per_worker):
                path = workload_paths[(w_id + i) % len(workload_paths)]
                try:
                    st, elapsed, _ = make_http_request(host, port, "GET", path)
                    if st == 200:
                        local_times.append(elapsed)
                    else:
                        with lock:
                            errors += 1
                except Exception:
                    with lock:
                        errors += 1

            with lock:
                all_times.extend(local_times)

        t0 = time.perf_counter()
        threads = [threading.Thread(target=worker_fn, args=(w,)) for w in range(workers)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        total_time_s = time.perf_counter() - t0
        update_rss()

        total_reqs = len(all_times)
        rps = total_reqs / max(total_time_s, 0.001)
        pct = compute_percentiles(all_times)
        pct["concurrency"] = workers
        pct["total_requests"] = total_reqs
        pct["throughput_rps"] = round(rps, 1)
        pct["errors"] = errors
        concurrency_results[f"concurrency_{workers}"] = pct
        print(f"  Workers: {workers:2d} | Throughput: {rps:7.1f} req/s | p50: {pct['p50_us']:7.1f} µs | p95: {pct['p95_us']:7.1f} µs | Errors: {errors}")

    # 5. Snapshot replacement under live traffic
    print("[5/6] Measuring snapshot reload resilience during concurrent traffic...")
    live_traffic_active = True
    reload_errors = 0
    reload_times = []

    def background_client():
        nonlocal reload_errors
        while live_traffic_active:
            try:
                st, elapsed, _ = make_http_request(host, port, "GET", "/v1/explorer/position?collection=masters-otb&moves=e4")
                if st == 200:
                    reload_times.append(elapsed)
                else:
                    reload_errors += 1
            except Exception:
                reload_errors += 1
            time.sleep(0.001)

    traffic_threads = [threading.Thread(target=background_client) for _ in range(4)]
    for t in traffic_threads:
        t.start()

    time.sleep(0.1)
    # Trigger admin reload
    t_rel0 = time.perf_counter()
    rel_st, _, rel_bytes = make_http_request(host, port, "POST", "/v1/admin/reload")
    reload_duration_ms = (time.perf_counter() - t_rel0) * 1000.0
    time.sleep(0.2)

    live_traffic_active = False
    for t in traffic_threads:
        t.join()

    update_rss()
    reload_pct = compute_percentiles(reload_times)
    print(f"  Reload status: {rel_st} in {reload_duration_ms:.2f} ms")
    print(f"  Live queries during reload: {len(reload_times)} completed | Errors: {reload_errors} | p50: {reload_pct['p50_us']:.1f} µs")

    # 6. Clean shutdown
    print("[6/6] Shutting down server...")
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
        print("  Server cleanly terminated with SIGTERM.")
    except subprocess.TimeoutExpired:
        proc.kill()
        print("  Server killed after timeout.")

    final_rss_kb = read_process_rss_kb(proc.pid)
    shutil.rmtree(temp_dir, ignore_errors=True)

    report = {
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "startup_ms": round(startup_ms, 2),
        "initial_rss_kb": initial_rss_kb,
        "peak_rss_kb": peak_rss_kb,
        "peak_rss_mb": round(peak_rss_kb / 1024.0, 2),
        "single_client_latencies": sc_results,
        "concurrency_scaling": concurrency_results,
        "reload_test": {
            "reload_status": rel_st,
            "reload_duration_ms": round(reload_duration_ms, 2),
            "concurrent_requests_served": len(reload_times),
            "errors": reload_errors,
            "latencies": reload_pct,
        },
    }

    if output_json:
        os.makedirs(os.path.dirname(os.path.abspath(output_json)), exist_ok=True)
        with open(output_json, "w") as f:
            json.dump(report, f, indent=2)
        print(f"Report written to {output_json}")

    return report


if __name__ == "__main__":
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    target_bin = os.path.join(repo_root, "target", "release", "codb")
    if not os.path.exists(target_bin):
        target_bin = os.path.join(repo_root, "target", "debug", "codb")

    fixture = os.path.join(repo_root, "fixtures", "stage_a_validation.pgn")
    out_file = os.path.join(repo_root, "benchmarks", "results", "http_api_benchmark.json")

    report = run_benchmark(target_bin, fixture, out_file)
    print("\nBenchmark completed successfully.")
