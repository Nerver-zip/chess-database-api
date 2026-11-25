#!/usr/bin/env python3
"""Check matching legacy API workloads and compare native timing medians."""

import argparse
import json
from pathlib import Path
from statistics import median


def legacy_artifacts(report):
    return {
        collection: {
            "total_games": metadata["total_games"], "lookup_table": metadata["lookup_table"],
            "position_index": metadata["position_index"], "shards": metadata["shards"],
        }
        for collection, metadata in report["datasets"].items()
    }


def timing(label, baseline, candidate, tolerance, larger_is_better=False):
    before = median(baseline)
    after = median(candidate)
    delta = (after - before) / before
    regressed = -delta if larger_is_better else delta
    return {"metric": label, "baseline_median": before, "candidate_median": after,
            "relative_change": delta, "regression_tolerance": tolerance,
            "within_tolerance": regressed <= tolerance}


def compare(baseline, candidate):
    for setting in ("machine", "cpu_affinity", "settings", "request_corpus"):
        if baseline[setting] != candidate[setting]:
            raise ValueError(f"Workloads differ in {setting}")
    if legacy_artifacts(baseline) != legacy_artifacts(candidate):
        raise ValueError("Canonical lookup, CPI or shard artifacts differ")
    if min(len(baseline["runs"]), len(candidate["runs"])) < 3:
        raise ValueError("At least three independent server starts are required per binary")

    def runs(report):
        return [run["http"] for run in report["runs"]]

    def contracts(run):
        for phase in ("concurrent", "sequential"):
            if run[phase]["unexpected_responses"]:
                raise ValueError(f"Unexpected HTTP status in {phase}")
        if run["same_snapshot_reload"] is not None and run["same_snapshot_reload"]["status"] != 200:
            raise ValueError("Reload response failed")
        return {key: observation["response_sha256"]
                for key, observation in run["first_requests_after_startup"].items()}

    expected = contracts(runs(baseline)[0])
    for run in runs(baseline) + runs(candidate):
        if contracts(run) != expected:
            raise ValueError("Legacy HTTP response content changed")

    same_versions = all(
        baseline["datasets"][collection]["dataset_build_id"] == dataset["dataset_build_id"]
        for collection, dataset in candidate["datasets"].items()
    )
    if same_versions:
        expected_etags = {key: observation["etag"] for key, observation in
                         runs(baseline)[0]["first_requests_after_startup"].items()}
        for run in runs(baseline) + runs(candidate):
            if {key: observation["etag"] for key, observation in
                run["first_requests_after_startup"].items()} != expected_etags:
                raise ValueError("Legacy ETags changed on the same snapshot")

    results = []
    for phase in ("sequential", "concurrent"):
        for field, tolerance in (("p50_us", 0.05), ("p95_us", 0.05), ("p99_us", 0.10)):
            results.append(timing(
                f"{phase} {field}", [run[phase][field] for run in runs(baseline)],
                [run[phase][field] for run in runs(candidate)], tolerance,
            ))
    results.append(timing(
        "concurrent requests_per_second",
        [run["concurrent"]["requests_per_second"] for run in runs(baseline)],
        [run["concurrent"]["requests_per_second"] for run in runs(candidate)],
        0.05, larger_is_better=True,
    ))
    for collection in baseline["datasets"]:
        for probe in ("hit_warm_pass", "miss_warm_pass"):
            for field, tolerance in (("p50_us", 0.05), ("p95_us", 0.05), ("p99_us", 0.10)):
                results.append(timing(
                    f"{collection} CPI {probe} {field}",
                    [run["in_process_cpi"][collection][probe][field] for run in baseline["runs"]],
                    [run["in_process_cpi"][collection][probe][field] for run in candidate["runs"]],
                    tolerance,
                ))
    return {"canonical_artifacts_match": True, "responses_match": True,
            "same_snapshot_etags_match": True if same_versions else None, "timings": results,
            "within_tolerance": all(row["within_tolerance"] for row in results)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    args = parser.parse_args()
    report = compare(json.loads(args.baseline.read_text()), json.loads(args.candidate.read_text()))
    print(json.dumps(report, indent=2, sort_keys=True))
    if not report["within_tolerance"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
