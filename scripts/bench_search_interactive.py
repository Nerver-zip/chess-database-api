#!/usr/bin/env python3
"""Exercise staggered users and superseded board navigation on a local API."""

import argparse
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import time
from urllib.parse import urlparse

from bench_http import compute_percentiles
from bench_legacy_api import request
from bench_search_api import ClosingConnection, cases_for, get, with_query


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--collection", required=True, choices=("masters-otb", "elite-online"))
    parser.add_argument("--users", type=int, default=4)
    parser.add_argument("--rounds", type=int, default=8)
    parser.add_argument("--think-ms", type=int, default=125)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if min(args.users, args.rounds, args.think_ms) < 1:
        parser.error("users, rounds and think time must be positive")
    origin = urlparse(args.url)
    if origin.scheme != "http" or not origin.hostname or origin.path not in ("", "/"):
        parser.error("--url must be an HTTP origin")

    def connect():
        return ClosingConnection(origin.hostname, origin.port, timeout=15)

    datasets = get(connect, "/v1/collections")["collections"]
    dataset = next(item for item in datasets if item["collection"] == args.collection)
    if not dataset["capabilities"]["game_search"]:
        parser.error("the selected collection must advertise game_search")
    depth = dataset["capabilities"]["position_search_coverage"].get("max_ply", 40)
    legacy, search = cases_for(connect, args.collection, depth)
    games = f"/v1/collections/{args.collection}/games"
    cases = {
        "autocomplete": search["player_prefix"],
        "metadata": search["metadata_exact_player"],
        "position": search["position_common"],
        "filtered_selective": search["filtered_selective"],
        "filtered_rating": search["filtered_broad"],
        "filtered_date": with_query("/v1/explorer/position/filtered", collection=args.collection,
                                    moves="e4,c5", from_year=2000),
        "legacy_opening": legacy["legacy_opening"],
        "legacy_pgn": legacy["legacy_pgn"],
        "navigation_latest": with_query(games, moves="e4,c5,Nf3", limit=5),
    }

    def user(index):
        rows = []
        cancelled = 0
        labels = list(cases)
        think = args.think_ms / 1000
        time.sleep(index * think / args.users)
        with connect() as connection:
            for turn in range(args.rounds):
                offset = (index + turn) % len(labels)
                for label in labels[offset:] + labels[:offset]:
                    if label == "navigation_latest":
                        # Issue a position query then abandon it as the current board changes.
                        with connect() as prior:
                            prior.request("GET", search["position_start"])
                            time.sleep(0.010)
                        cancelled += 1
                    status, _, _, elapsed = request(connection, cases[label])
                    rows.append((label, str(status), elapsed))
                    time.sleep(think)
        return rows, cancelled

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=args.users) as workers:
        observations = list(workers.map(user, range(args.users)))
    elapsed = time.perf_counter() - start
    per_case = defaultdict(list)
    for rows, _ in observations:
        for label, status, latency in rows:
            per_case[label].append((status, latency))
    result = {}
    for label, rows in per_case.items():
        accepted = [latency for status, latency in rows if status == "200"]
        result[label] = {
            "attempted": len(rows), "accepted": len(accepted),
            "acceptance_rate": len(accepted) / len(rows),
            "statuses": dict(Counter(status for status, _ in rows)),
            **compute_percentiles(accepted),
        }
    report = {
        "dataset_version": dataset["dataset_version"], "collection": args.collection,
        "users": args.users, "rounds": args.rounds, "think_ms": args.think_ms,
        "duration_seconds": elapsed, "workload": cases, "cases": result,
        "client_abandoned_navigation": sum(count for _, count in observations),
    }
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
