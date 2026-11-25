#!/usr/bin/env python3
"""Measure search-index construction from a versioned canonical dataset."""

import argparse
import json
import os
from pathlib import Path
import resource
import subprocess
import time


def snapshot_inodes(dataset):
    snapshot = (dataset / "current").resolve()
    inodes = set()
    for path in snapshot.rglob("*"):
        if path.is_file():
            stat = path.stat()
            inodes.add((stat.st_dev, stat.st_ino))
    return inodes


def disk_state(dataset, temp_directory, pid, original):
    versions = dataset / "versions"
    files = {}
    def record(path):
        try:
            stat = path.stat()
        except FileNotFoundError:
            return
        if not path.is_file():
            return
        key = (stat.st_dev, stat.st_ino)
        if key not in original:
            files[key] = stat.st_blocks * 512

    for root in (versions.glob(".staging_search_*"), temp_directory.glob("search-runs-*")):
        for path in root:
            for child in path.rglob("*"):
                record(child)

    # The codec and SQLite can unlink scratch files while continuing to use them.
    # Count their allocated blocks through the process descriptors.
    for descriptor in Path(f"/proc/{pid}/fd").glob("*"):
        try:
            target = os.readlink(descriptor)
        except FileNotFoundError:
            continue
        if target.endswith(" (deleted)"):
            record(descriptor)
    return sum(files.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--temp-dir", type=Path, required=True)
    parser.add_argument("--memory-limit-mib", type=int, default=128)
    parser.add_argument("--output", type=Path, required=True)
    coverage = parser.add_mutually_exclusive_group()
    coverage.add_argument("--depth-plies", type=int)
    coverage.add_argument("--full", action="store_true")
    args = parser.parse_args()
    args.dataset = args.dataset.resolve(strict=True)
    args.temp_dir = args.temp_dir.resolve(strict=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)

    command = [
        args.binary.resolve(strict=True), "build-search-index", "--dataset", args.dataset,
        "--temp-dir", args.temp_dir, "--memory-limit-mib", str(args.memory_limit_mib),
    ]
    if args.depth_plies is not None:
        command.extend(["--depth-plies", str(args.depth_plies)])
    if args.full:
        command.append("--full")

    environment = dict(os.environ, RUST_LOG="info", SQLITE_TMPDIR=str(args.temp_dir))
    raw_report = args.output.with_suffix(".builder.json")
    log = args.output.with_suffix(".log")
    peak_allocated_bytes = 0
    original = snapshot_inodes(args.dataset)
    start = time.perf_counter()
    with raw_report.open("w") as output, log.open("w") as stderr:
        with subprocess.Popen(command, env=environment, stdout=output, stderr=stderr) as process:
            while process.poll() is None:
                peak_allocated_bytes = max(
                    peak_allocated_bytes, disk_state(args.dataset, args.temp_dir, process.pid, original)
                )
                time.sleep(0.25)
            if process.returncode != 0:
                raise SystemExit(f"Search build failed with status {process.returncode}. See {log}.")

    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    build = json.loads(raw_report.read_text())
    index = build["index"]
    artifact_bytes = sum(item["file_size_bytes"] for item in index["artifacts"].values())
    report = {
        "build": build,
        "wall_seconds": time.perf_counter() - start,
        "user_cpu_seconds": usage.ru_utime,
        "system_cpu_seconds": usage.ru_stime,
        "peak_rss_kib": usage.ru_maxrss,
        "peak_sampled_staging_allocated_bytes": peak_allocated_bytes,
        "disk_note": "Allocated blocks in the build stage, spill directory and unlinked scratch files. "
                     "Shared legacy artifacts are excluded. Samples are taken every 250 ms.",
        "artifact_bytes": artifact_bytes,
        "artifact_bytes_per_game": artifact_bytes / index["game_count"] if index["game_count"] else None,
        "postings_bytes_per_position": index["artifacts"]["positions.postings"]["file_size_bytes"] / index["position_count"]
                                        if index["position_count"] else None,
        "postings_bytes_per_occurrence": index["artifacts"]["positions.postings"]["file_size_bytes"] / index["posting_count"]
                                          if index["posting_count"] else None,
    }
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
