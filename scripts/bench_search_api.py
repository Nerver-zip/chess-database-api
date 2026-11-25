#!/usr/bin/env python3
"""Measure search and mixed traffic on an isolated Chess Database API instance."""

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import http.client
import json
from pathlib import Path
import time
from urllib.parse import urlencode, urlparse

from bench_http import compute_percentiles
from bench_legacy_api import LONG_LINE, START, request


def get(connect, path):
    with connect() as conn:
        status, _, body, _ = request(conn, path)
    if status != 200:
        raise RuntimeError(f"{path} returned {status}: {body[:200]!r}")
    return json.loads(body)


def with_query(path, **params):
    return f"{path}?{urlencode(params)}"


def cases_for(connect, collection, depth):
    root = f"/v1/collections/{collection}"
    games = f"{root}/games"
    filtered = "/v1/explorer/position/filtered"
    legacy = "/v1/explorer/position"
    seed = get(connect, with_query(games, limit=12))["games"]
    games_with_event = [game for game in seed if game["event"]]
    if not seed:
        raise RuntimeError(f"{collection} has no search results")
    sample = seed[0]
    other = next((item for item in seed if item["id"] != sample["id"]), sample)
    white_name = sample["white"]
    prefix = white_name[:min(4, len(white_name))]
    legacy_cases = {
        "legacy_start": with_query(legacy, collection=collection, fen=START),
        "legacy_opening": with_query(legacy, collection=collection, moves="e4,c5,Nf3"),
        "legacy_pgn": f"{games}/{sample['id']}",
    }
    search = {
        "metadata_broad": with_query(games, sort="date_desc", limit=25),
        "metadata_exact_player": with_query(games, player=white_name),
        "metadata_other_player": with_query(games, player=other["black"]),
        "metadata_matchup": with_query(games, player=white_name, opponent=sample["black"]),
        "metadata_ratings": with_query(games, min_both_ratings=2400, sort="average_rating"),
        "metadata_years": with_query(games, from_year=2000, through_year=2015, sort="date_desc"),
        "metadata_combined": with_query(games, player=white_name, min_both_ratings=2200,
                                        from_year=1990, sort="game_length"),
        "game_details": f"{games}/{sample['id']}/metadata",
        "position_start": with_query(games, fen=START),
        "position_common": with_query(games, moves="e4,c5,Nf3"),
        "position_medium": with_query(games, moves="e4,e5,Nf3,Nc6,Bb5,a6,Ba4,Nf6"),
        "position_deep": with_query(games, moves=",".join(LONG_LINE[:depth])),
        "position_missing": with_query(games, fen="4k3/8/8/8/8/8/8/4K3 w - - 0 1"),
        "position_transposition_a": with_query(games, moves="Nf3,Nf6,g3,g6"),
        "position_transposition_b": with_query(games, moves="g3,g6,Nf3,Nf6"),
        "position_and_player": with_query(games, fen=START, player=white_name),
        "position_and_rating": with_query(games, moves="e4,c5", min_both_ratings=2400),
        "position_continuation": with_query(games, fen=START, continuation="Nf3"),
        "filtered_selective": with_query(filtered, collection=collection, fen=START, player=white_name),
        "filtered_broad": with_query(filtered, collection=collection, fen=START, min_both_ratings=2200),
        "filtered_common": with_query(filtered, collection=collection, moves="e4,c5", min_both_ratings=2400),
        "filtered_rare": with_query(filtered, collection=collection, moves=",".join(LONG_LINE[:20]),
                                    min_plies=0),
        "filtered_no_match": with_query(filtered, collection=collection, moves="d4,Nf6", player="benchmark unknown player"),
        "filtered_combination": with_query(filtered, collection=collection, fen=START, player=white_name,
                                           from_year=1990, min_both_ratings=2200),
    }
    if len(prefix) >= 2:
        search["player_prefix"] = with_query(f"{root}/players", prefix=prefix)
    search["player_exact"] = with_query(f"{root}/players", name=white_name)
    if games_with_event:
        name = games_with_event[0]["event"]
        search["event_exact"] = with_query(f"{root}/events", name=name)
        if len(name) >= 2:
            search["event_prefix"] = with_query(f"{root}/events", prefix=name[:4])
        search["metadata_event"] = with_query(games, event=name, sort="date_desc")

    page = get(connect, with_query(games, sort="date_desc", limit=25))
    if page["next_cursor"]:
        search["metadata_second_page"] = with_query(games, sort="date_desc", limit=25, cursor=page["next_cursor"])
    return legacy_cases, search


def measure(connect, cases, repetitions, workers, mixed=False):
    paths = list(cases.items())

    def worker(index):
        rows = []
        with connect() as conn:
            for repeat in range(repetitions):
                for offset in range(len(paths)):
                    label, path = paths[(offset + index + repeat) % len(paths)]
                    status, _, _, latency = request(conn, path)
                    rows.append((label, status, latency))
                    if mixed and label.startswith("legacy"):
                        # Give the Explorer additional probes during search saturation.
                        for _ in range(4):
                            status, _, _, latency = request(conn, path)
                            rows.append((label, status, latency))
        return rows

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        rows = [item for result in pool.map(worker, range(workers)) for item in result]
    elapsed = time.perf_counter() - start
    result = {}
    for label in cases:
        results = [(status, latency) for name, status, latency in rows if name == label]
        result[label] = {
            "statuses": Counter(str(status) for status, _ in results),
            **compute_percentiles([latency for status, latency in results if status == 200]),
        }
    return {"seconds": elapsed, "requests": len(rows), "requests_per_second": len(rows) / elapsed,
            "cases": result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8080")
    parser.add_argument("--collection", choices=("elite-online", "masters-otb"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--workers", type=int, default=4)
    args = parser.parse_args()
    if min(args.repetitions, args.workers) < 1:
        parser.error("workers and repetitions must be positive")
    url = urlparse(args.url)
    if url.scheme != "http" or not url.hostname or url.path not in ("", "/"):
        parser.error("--url must be an HTTP origin")

    def connect():
        # HTTPConnection does not implement a context manager.
        return ClosingConnection(url.hostname, url.port, timeout=20)

    collections = get(connect, "/v1/collections")["collections"]
    dataset = next(item for item in collections if item["collection"] == args.collection)
    if not dataset["capabilities"]["game_search"]:
        raise RuntimeError(f"Search is disabled for {args.collection}")
    depth = dataset["capabilities"]["position_search_coverage"].get("max_ply", len(LONG_LINE))
    legacy, search = cases_for(connect, args.collection, depth)
    measure(connect, legacy, 5, 1)
    report = {
        "dataset": dataset,
        "workload": {**legacy, **search},
        "search": measure(connect, search, args.repetitions, 1),
        "legacy": measure(connect, legacy, args.repetitions * 15, args.workers),
        "mixed": measure(connect, {**legacy, **search}, args.repetitions, args.workers, mixed=True),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


class ClosingConnection(http.client.HTTPConnection):
    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


if __name__ == "__main__":
    main()
