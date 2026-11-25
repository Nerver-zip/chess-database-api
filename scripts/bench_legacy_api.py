#!/usr/bin/env python3
"""Compare the legacy API on pinned, already-indexed production snapshots."""

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import hashlib
import http.client
import json
import os
from pathlib import Path
import platform
import socket
import struct
import subprocess
import time
from urllib.parse import urlencode

from bench_http import MemorySampler, compute_percentiles

START = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
COLLECTIONS = ("masters-otb", "elite-online")
LONG_LINE = (
    "e4 e5 Nf3 Nc6 Bb5 a6 Ba4 Nf6 O-O Be7 Re1 b5 Bb3 d6 c3 O-O h3 Nb8 d4 Nbd7 "
    "c4 c6 Nc3 Bb7 a3 Qc7 cxb5 axb5 Be3 Rfe8 Rc1 Qb8 Ng5 Rf8 dxe5 dxe5 Be6 Qe8 Ba2 h6 Nf3 b4"
).split()


def position(collection, **params):
    return "/v1/explorer/position?" + urlencode(dict(collection=collection, **params))


def request(connection, path, method="GET", headers=None):
    start = time.perf_counter()
    try:
        connection.request(method, path, headers=headers or {})
        response = connection.getresponse()
        body = response.read()
        return response.status, response.getheader("ETag"), body, (time.perf_counter() - start) * 1e6
    except (TimeoutError, OSError, http.client.HTTPException) as exc:
        connection.close()
        return type(exc).__name__, None, b"", (time.perf_counter() - start) * 1e6


def sample_game(path, index, entries):
    with path.open("rb") as file:
        file.seek(32 + index * (entries - 1) // 7 * 16)
        return struct.unpack(">Q", file.read(8))[0]


def cases_for(collection, snapshot, manifest):
    games = f"/v1/collections/{collection}/games"
    depth = manifest["position_index"]["indexed_depth_plies"]
    cases = {
        "start": position(collection, fen=START),
        "common": position(collection, moves="e4,c5,Nf3"),
        "less_common": position(collection, moves="a3,h5,a4,h4,Nf3"),
        "custom_fen": position(collection, fen="4k3/8/8/8/8/8/8/4K3 w - - 0 1"),
        "transposition_a": position(collection, moves="Nf3,Nf6,g3,g6"),
        "transposition_b": position(collection, moves="g3,g6,Nf3,Nf6"),
        "beyond_depth": position(collection, moves=",".join(LONG_LINE[:depth + 1])),
    }
    for i in range(8):
        game_id = sample_game(snapshot / "games.idx", i, manifest["total_games"])
        cases[f"pgn_{i}"] = f"{games}/{game_id}"
    return cases


def evict_snapshot_pages(snapshots):
    for snapshot in snapshots:
        for path in snapshot.rglob("*"):
            if path.is_file() and not path.is_symlink():
                with path.open("rb") as file:
                    os.posix_fadvise(file.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)


def wait_ready(process, connection, start):
    while time.perf_counter() - start < 600:
        if process.poll() is not None:
            raise RuntimeError(f"Server exited before readiness with status {process.returncode}")
        status, _, _, _ = request(connection, "/ready")
        if status == 200:
            return time.perf_counter() - start
        time.sleep(0.01)
    raise TimeoutError("Server did not become ready")


def cpu_seconds(pid):
    # The executable name is enclosed in parentheses and may contain spaces.
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def measure(port, paths, rounds, workers, pid):
    def worker(worker_index):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=20)
        try:
            rows = []
            for _ in range(worker_index, rounds, workers):
                for label, path, headers in paths:
                    status, _, _, latency = request(connection, path, headers=headers)
                    rows.append((label, status, latency))
            return rows
        finally:
            connection.close()

    start_cpu = cpu_seconds(pid)
    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        rows = [row for batch in pool.map(worker, range(workers)) for row in batch]
    elapsed = time.perf_counter() - start
    result = {}
    for label, _, headers in paths:
        selected = [(status, latency) for name, status, latency in rows if label == name]
        expected = 304 if headers else 200
        result[label] = {
            **compute_percentiles([latency for status, latency in selected if status == expected]),
            "statuses": Counter(str(status) for status, _ in selected),
            "unexpected_responses": sum(status != expected for status, _ in selected),
        }
    return {
        **compute_percentiles([latency for _, status, latency in rows if status in (200, 304)]),
        "cases": result,
        "requests": len(rows),
        "unexpected_responses": sum(group["unexpected_responses"] for group in result.values()),
        "requests_per_second": len(rows) / elapsed,
        "wall_seconds": elapsed,
        "server_cpu_seconds": cpu_seconds(pid) - start_cpu,
    }


def run_http(args, snapshots, cases, repetition):
    if args.evict:
        evict_snapshot_pages(snapshots)
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]

    token = "local-legacy-benchmark"
    command = [
        args.binary, "serve", "--host", "127.0.0.1", "--port", str(port),
        "--masters-dataset", snapshots[0], "--elite-dataset", snapshots[1],
        "--reader-pool-size", str(args.reader_pool), "--admin-token", token,
        "--enable-game-retrieval", "true",
    ]
    if args.deep:
        command.append("--deep-validate-on-startup")
    env = dict(os.environ, RUST_LOG="warn")
    with args.output.with_suffix(f".run-{repetition}.log").open("w") as log:
        start = time.perf_counter()
        with subprocess.Popen(command, env=env, stdout=log, stderr=log) as process:
            sampler = MemorySampler(process.pid, sample_interval_s=0.02)
            sampler.start()
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=120)
            try:
                startup = wait_ready(process, connection, start)
                paths = []
                first_requests = {}
                for label, path in cases.items():
                    status, etag, body, latency = request(connection, path)
                    if status != 200:
                        raise RuntimeError(f"{label} returned {status}: {body[:300]!r}")
                    normalized = body
                    if "/explorer/" in path:
                        parsed = json.loads(body)
                        del parsed["dataset_version"], parsed["dataset_freshness"]
                        normalized = json.dumps(parsed, sort_keys=True).encode()
                    first_requests[label] = {
                        "latency_us": latency,
                        "response_sha256": hashlib.sha256(normalized).hexdigest(),
                        "etag": etag,
                    }
                    paths.append((label, path, {}))
                    if label.endswith(":start"):
                        paths.append((f"{label}:304", path, {"If-None-Match": etag}))
                measure(port, paths, 10, 1, process.pid)
                sequential = measure(port, paths, args.rounds, 1, process.pid)
                concurrent = measure(port, paths, args.rounds, args.workers, process.pid)
                reload = None
                if not args.skip_reload:
                    status, _, _, reload_us = request(
                        connection, "/v1/admin/reload", "POST", {"Authorization": f"Bearer {token}"}
                    )
                    reload = {"status": status, "latency_us": reload_us}
                return {
                    "startup_seconds": startup,
                    "first_requests_after_startup": first_requests,
                    "sequential": sequential,
                    "concurrent": concurrent,
                    "same_snapshot_reload": reload,
                    "peak_sampled_rss_kib": sampler.stop(),
                }
            finally:
                connection.close()
                sampler.stop()
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--masters", type=Path, required=True)
    parser.add_argument("--elite", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--rounds", type=int, default=300)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--reader-pool", type=int, default=4)
    parser.add_argument("--evict", action="store_true", help="Advise eviction before each server launch")
    parser.add_argument("--deep", action="store_true", help="Enable deep verification at server startup")
    parser.add_argument("--skip-reload", action="store_true", help="Skip reload during paired query measurements")
    args = parser.parse_args()
    if min(args.repetitions, args.rounds, args.workers, args.reader_pool) < 1:
        parser.error("repetitions, rounds, workers and reader pool must all be positive")
    args.binary = args.binary.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    snapshots = [(root / "current").resolve() if (root / "current").is_symlink()
                 else root.resolve() for root in (args.masters, args.elite)]
    cases = {}
    manifests = {}
    for collection, snapshot in zip(COLLECTIONS, snapshots, strict=True):
        manifest = json.loads((snapshot / "manifest.json").read_text())
        if manifest["collection"] != collection:
            parser.error(f"{snapshot} belongs to {manifest['collection']}, expected {collection}")
        manifests[collection] = {key: manifest[key] for key in (
            "dataset_build_id", "total_games", "lookup_table", "position_index", "shards",
        )}
        cases.update({f"{collection}:{name}": path
                      for name, path in cases_for(collection, snapshot, manifest).items()})

    with args.binary.open("rb") as binary:
        binary_sha256 = hashlib.file_digest(binary, "sha256").hexdigest()
    result = {
        "schema_version": 1,
        "revision": args.revision,
        "binary_sha256": binary_sha256,
        "machine": platform.platform(),
        "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "datasets": manifests,
        "request_corpus": cases,
        "settings": {
            "workers": args.workers, "rounds": args.rounds, "reader_pool": args.reader_pool,
            "eviction_advised_before_startup": args.evict, "deep_verify_at_startup": args.deep,
            "skip_reload": args.skip_reload,
        },
        "runs": [],
    }
    for repetition in range(1, args.repetitions + 1):
        cpi = {}
        for collection, snapshot in zip(COLLECTIONS, snapshots, strict=True):
            cpi[collection] = json.loads(subprocess.check_output([
                args.binary, "bench-lookup", "--dataset", snapshot,
            ], text=True))
        result["runs"].append({
            "in_process_cpi": cpi, "http": run_http(args, snapshots, cases, repetition),
        })
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
        print(f"Completed repetition {repetition} of {args.repetitions}", flush=True)


if __name__ == "__main__":
    main()
