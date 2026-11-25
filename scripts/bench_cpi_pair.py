#!/usr/bin/env python3
"""Alternate native CPI lookup benchmarks on the same canonical datasets."""

import argparse
import hashlib
import json
import os
from pathlib import Path
from statistics import median
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--masters", type=Path, required=True)
    parser.add_argument("--elite", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--games", type=int, default=10000)
    parser.add_argument("--pairs", type=int, default=7)
    args = parser.parse_args()
    if args.pairs < 3 or args.games < 1:
        parser.error("require at least three pairs and one sampled game")

    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    report = {"games_requested": args.games, "cpu_affinity": sorted(os.sched_getaffinity(0)),
              "rounds": args.pairs, "binaries": {}, "datasets": {}}
    for name, path in binaries.items():
        with path.open("rb") as handle:
            report["binaries"][name] = hashlib.file_digest(handle, "sha256").hexdigest()

    for collection, dataset in (("elite-online", args.elite), ("masters-otb", args.masters)):
        samples = {name: [] for name in binaries}
        for trial in range(args.pairs):
            order = list(binaries) if trial % 2 == 0 else list(reversed(binaries))
            for variant in order:
                result = subprocess.check_output([
                    binaries[variant], "bench-lookup", "--dataset", dataset, "--games", str(args.games),
                ], text=True)
                samples[variant].append(json.loads(result))
        comparison = {}
        for probe in ("hit_warm_pass", "miss_warm_pass"):
            for quantile in ("p50_us", "p95_us", "p99_us"):
                before, after = (median(run[probe][quantile] for run in samples[variant])
                                 for variant in binaries)
                comparison[f"{probe}:{quantile}"] = {
                    "baseline": before, "candidate": after, "relative_change": (after - before) / before,
                }
        report["datasets"][collection] = {"samples": samples, "median_comparison": comparison}
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
