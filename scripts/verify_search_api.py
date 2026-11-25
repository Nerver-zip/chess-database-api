#!/usr/bin/env python3
"""Verify the Chess Database API discovery-to-PGN workflow on a running service."""

import argparse
import json
import time
from urllib.parse import urlencode, urlparse

from bench_legacy_api import request
from bench_search_api import ClosingConnection

START = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8080")
    parser.add_argument("--collection", choices=("elite-online", "masters-otb"), required=True)
    parser.add_argument("--mode", choices=("enriched", "legacy", "disabled"), default="enriched")
    args = parser.parse_args()
    target = urlparse(args.url)
    if target.scheme != "http" or not target.hostname:
        parser.error("the URL must be an HTTP origin")
    cases = {}
    start = time.perf_counter()

    with ClosingConnection(target.hostname, target.port, timeout=30) as connection:
        def probe(label, path, expected=200, headers=None, **params):
            if params:
                path += "?" + urlencode(params)
            status, etag, body, elapsed = request(connection, path, headers=headers)
            cases[label] = {"status": status, "latency_us": elapsed}
            if status != expected:
                raise RuntimeError(f"{label}: {status}, expected {expected}: {body[:300]!r}")
            return etag, body

        def fetch(label, path, **params):
            return json.loads(probe(label, path, **params)[1])

        probe("ready", "/ready")
        collections = fetch("collections", "/v1/collections")["collections"]
        snapshot = next(item for item in collections if item["collection"] == args.collection)
        version = snapshot["dataset_version"]
        root = f"/v1/collections/{args.collection}"
        explorer_path = "/v1/explorer/position"
        etag, body = probe("legacy_explorer", explorer_path, collection=args.collection, moves="e4")
        legacy = json.loads(body)
        assert legacy["dataset_version"] == version
        assert legacy["total_games"] > 0
        probe("legacy_conditional", explorer_path, expected=304, headers={"If-None-Match": etag},
              collection=args.collection, moves="e4")
        if args.mode in ("legacy", "disabled"):
            assert not snapshot.get("capabilities", {}).get("game_search")
            expected_status, expected_error = (
                (503, "search_unavailable") if args.mode == "legacy" else (403, "search_disabled")
            )
            response = json.loads(probe("games_disabled", f"{root}/games", expected=expected_status)[1])
            assert response["error"]["code"] == expected_error
        else:
            assert snapshot["capabilities"]["position_search"]
            first = fetch("first_games", f"{root}/games", fen=START, limit=2)
            assert first["position_coverage"] == snapshot["capabilities"]["position_search_coverage"]
            assert first["has_more"] and len(first["games"]) == 2
            second = fetch("second_games", f"{root}/games", fen=START, limit=2,
                           cursor=first["next_cursor"])
            assert not set(game["id"] for game in first["games"]) & set(game["id"] for game in second["games"])
            game = first["games"][0]
            assert game["position_match"]["ply"] == 0
            assert isinstance(game["id"], str) and 0 <= int(game["id"]) < 2**64
            games = fetch("continuation", f"{root}/games", fen=START,
                          continuation=game["position_match"]["continuation"], limit=2)
            assert all(item["position_match"]["continuation"] == game["position_match"]["continuation"]
                       for item in games["games"])
            players = fetch("players", f"{root}/players", name=game["white"], limit=20)["players"]
            player = next(item for item in players if item["id"] == game["white_player_id"])
            selected = fetch("player_detail", f"{root}/players/{player['id']}", version=version)
            assert selected["player"] == player
            player_games = fetch("player_position_games", f"{root}/games", player_id=player["id"],
                                 player_side="white", fen=START, version=version)
            assert player_games["games"]
            assert all(row["white_player_id"] == player["id"] for row in player_games["games"])
            if game["event_id"]:
                event = fetch("event_detail", f"{root}/events/{game['event_id']}", version=version)["event"]
                assert event["id"] == game["event_id"]
                event_games = fetch("event_games", f"{root}/games", event_id=event["id"], version=version)
                assert event_games["games"] and all(row["event_id"] == event["id"]
                                                    for row in event_games["games"])
            if snapshot["capabilities"]["game_metadata"]:
                details = fetch("game_metadata", f"{root}/games/{game['id']}/metadata")
                assert details["game"]["id"] == game["id"]
                assert details["game"]["white"] == game["white"]
            if snapshot["capabilities"]["game_retrieval"]:
                _, pgn = probe("game_pgn", f"{root}/games/{game['id']}")
                assert pgn.startswith(b"[Event ")
            if snapshot["capabilities"]["filtered_explorer"]:
                filtered_path = "/v1/explorer/position/filtered"
                unfiltered = fetch("unfiltered_compatibility", filtered_path,
                                   collection=args.collection, moves="e4")
                assert unfiltered == legacy
                filters = {"collection": args.collection, "fen": START,
                           "player_id": player["id"], "player_side": "white", "version": version}
                filtered = fetch("filtered_player", filtered_path, **filters)
                assert filtered["total_games"] == filtered["white_wins"] + filtered["draws"] + filtered["black_wins"]
                assert filtered["total_games"] == sum(move["total_games"] for move in filtered["moves"])

    print(json.dumps({"collection": args.collection, "dataset_version": version,
                      "mode": args.mode, "seconds": time.perf_counter() - start,
                      "cases": cases}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
