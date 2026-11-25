#!/usr/bin/env python3
"""Alternate baseline and candidate runs on the same canonical datasets."""

import argparse
import json
from pathlib import Path
import subprocess
import sys

from compare_legacy_api import compare


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for variant in ("baseline", "candidate"):
        parser.add_argument(f"--{variant}", type=Path, required=True)
        parser.add_argument(f"--{variant}-revision", required=True)
    parser.add_argument("--masters", type=Path, required=True)
    parser.add_argument("--elite", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=500)
    parser.add_argument("--evict", action="store_true")
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    combined = {}
    for trial in range(1, 4):
        order = ("baseline", "candidate") if trial % 2 else ("candidate", "baseline")
        for variant in order:
            result = args.output_dir / f"{variant}-{trial}.json"
            command = [
                sys.executable, Path(__file__).with_name("bench_legacy_api.py"),
                "--binary", getattr(args, variant),
                "--revision", getattr(args, f"{variant}_revision"),
                "--masters", args.masters, "--elite", args.elite,
                "--output", result, "--rounds", str(args.rounds),
                "--repetitions", "1", "--skip-reload",
            ]
            if args.evict:
                command.append("--evict")
            subprocess.run(command, check=True)
            report = json.loads(result.read_text())
            if variant in combined:
                combined[variant]["runs"].extend(report["runs"])
            else:
                combined[variant] = report
            output = args.output_dir / f"{variant}.json"
            output.write_text(json.dumps(combined[variant], indent=2, sort_keys=True) + "\n")
    comparison = compare(combined["baseline"], combined["candidate"])
    (args.output_dir / "comparison.json").write_text(json.dumps(comparison, indent=2, sort_keys=True) + "\n")
    if not comparison["within_tolerance"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
