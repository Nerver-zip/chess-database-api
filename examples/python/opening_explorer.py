#!/usr/bin/env python3
"""Standard-library client and batch exporter for the Opening Explorer API.

Requires Python 3.9 or later and no third-party packages.

Examples:
  # One position, printed as JSON
  python3 examples/python/opening_explorer.py position --collection masters-otb --moves "e4,e5,Nf3"

  # Many positions from a text file (one move list per line, blank lines and
  # lines starting with "#" are ignored), written as CSV for analysis
  python3 examples/python/opening_explorer.py batch --collection elite-online \
      --input openings.txt --output stats.csv

Set OPENING_EXPLORER_URL to point at another server (default http://127.0.0.1:8080).
Results are keyed by position_key, so transpositions reached in different
move orders produce the same row in the output.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

COLLECTIONS = ("masters-otb", "elite-online")
DEFAULT_URL = "http://127.0.0.1:8080"


class ApiError(Exception):
    """A non-2xx response or a network failure, with the API's error code."""

    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(f"HTTP {status} {code}: {message}")
        self.status = status
        self.code = code
        self.message = message


class OpeningExplorer:
    def __init__(self, base_url: str = DEFAULT_URL, timeout: float = 10.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def _get(self, path: str, params: dict[str, str] | None = None) -> tuple[int, bytes]:
        url = self.base_url + path
        if params:
            # quote_via=quote encodes spaces as %20, which every server accepts.
            url += "?" + urllib.parse.urlencode(params, quote_via=urllib.parse.quote)
        request = urllib.request.Request(url, headers={"Accept": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            body = error.read()
            code, message = "http_error", error.reason
            try:
                envelope = json.loads(body.decode("utf-8")).get("error", {})
                code = envelope.get("code", code)
                message = envelope.get("message", message)
            except (ValueError, AttributeError):
                pass  # Empty or non-JSON body (for example an unknown route).
            raise ApiError(error.code, code, message) from None
        except urllib.error.URLError as error:
            raise ApiError(0, "network_error", str(error.reason)) from None

    def collections(self) -> list[dict]:
        _, body = self._get("/v1/collections")
        return json.loads(body)["collections"]

    def position(self, collection: str, *, moves: str | None = None, fen: str | None = None) -> dict:
        if collection not in COLLECTIONS:
            raise ValueError(f"collection must be one of {COLLECTIONS}")
        if (moves is None) == (fen is None):
            raise ValueError("pass exactly one of moves or fen")
        params = {"collection": collection}
        params["moves" if moves is not None else "fen"] = moves if moves is not None else fen
        _, body = self._get("/v1/explorer/position", params)
        return json.loads(body)

    def game_pgn(self, collection: str, game_id: str) -> str:
        if not game_id.isdigit() or int(game_id) > 2**64 - 1:
            raise ValueError("game_id must be an unsigned 64-bit decimal integer")
        _, body = self._get(f"/v1/collections/{collection}/games/{game_id}")
        return body.decode("utf-8")


CSV_FIELDS = [
    "collection", "input_moves", "position_key", "fen", "total_games",
    "white_wins", "draws", "black_wins", "white_win_pct", "draw_pct", "black_win_pct",
    "top_move_san", "top_move_games", "dataset_version",
]


def summarize(collection: str, moves: str, result: dict) -> dict:
    top = result["moves"][0] if result["moves"] else None
    return {
        "collection": collection,
        "input_moves": moves,
        "position_key": result["position_key"],
        "fen": result["fen"],
        "total_games": result["total_games"],
        "white_wins": result["white_wins"],
        "draws": result["draws"],
        "black_wins": result["black_wins"],
        "white_win_pct": result["white_win_pct"],
        "draw_pct": result["draw_pct"],
        "black_win_pct": result["black_win_pct"],
        "top_move_san": top["san"] if top else "",
        "top_move_games": top["total_games"] if top else 0,
        "dataset_version": result["dataset_version"],
    }


def read_lines(path: str) -> list[str]:
    with open(path, encoding="utf-8") as handle:
        lines = [line.strip() for line in handle]
    return [line for line in lines if line and not line.startswith("#")]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--url", default=os.environ.get("OPENING_EXPLORER_URL", DEFAULT_URL))
    sub = parser.add_subparsers(dest="command", required=True)

    p_position = sub.add_parser("position", help="print one position as JSON")
    p_position.add_argument("--collection", required=True, choices=COLLECTIONS)
    group = p_position.add_mutually_exclusive_group(required=True)
    group.add_argument("--moves", help='SAN or UCI list, e.g. "e4,e5,Nf3" (empty string = start)')
    group.add_argument("--fen")

    p_batch = sub.add_parser("batch", help="query many move lists and write CSV")
    p_batch.add_argument("--collection", required=True, choices=COLLECTIONS)
    p_batch.add_argument("--input", required=True, help="text file, one move list per line")
    p_batch.add_argument("--output", required=True, help="CSV path to write")

    sub.add_parser("collections", help="print loaded collections as JSON")

    args = parser.parse_args(argv)
    client = OpeningExplorer(args.url)

    try:
        if args.command == "collections":
            print(json.dumps(client.collections(), indent=2))
        elif args.command == "position":
            result = client.position(args.collection, moves=args.moves, fen=args.fen)
            print(json.dumps(result, indent=2))
        elif args.command == "batch":
            rows = []
            seen_keys: set[str] = set()
            for moves in read_lines(args.input):
                result = client.position(args.collection, moves=moves)
                # Duplicate position keys (same position, different move order) are
                # written once, under the first move list that reached them.
                if result["position_key"] in seen_keys:
                    continue
                seen_keys.add(result["position_key"])
                rows.append(summarize(args.collection, moves, result))
            with open(args.output, "w", newline="", encoding="utf-8") as handle:
                writer = csv.DictWriter(handle, fieldnames=CSV_FIELDS)
                writer.writeheader()
                writer.writerows(rows)
            print(f"wrote {len(rows)} rows to {args.output}", file=sys.stderr)
    except ApiError as error:
        print(f"API error {error.status} ({error.code}): {error.message}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
