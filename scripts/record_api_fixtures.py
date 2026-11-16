#!/usr/bin/env python3
"""Record real HTTP responses of a running Opening Explorer server as test fixtures.

The offline documentation tests (scripts/test_docs_examples.py) replay these
responses through a local fake server, so the client examples can run without a
dataset or the Rust toolchain. Re-record them whenever the response contract
changes.

Recommended provenance: serve synthetic datasets built from the public sample
fixture, not a real corpus release. For example:

  codb build --input fixtures/stage_a_validation.pgn --collection elite-online --output $D/elite
  codb build-index --dataset $D/elite --collection elite-online
  codb build --input fixtures/stage_a_validation.pgn --collection masters-otb --output $D/masters
  codb build-index --dataset $D/masters --collection masters-otb --depth-plies 40
  codb serve --masters-dataset $D/masters --elite-dataset $D/elite --port 18091 \
      --admin-token <test-token> --allowed-origins http://localhost:3000
  codb serve --elite-dataset $D/elite --port 18090     # for collection_unavailable

Usage:
  python3 scripts/record_api_fixtures.py \
      --url http://127.0.0.1:18091 --admin-token <test-token> \
      --elite-only-url http://127.0.0.1:18090 \
      --out fixtures/api

The admin token is only used for the reload sample and is never written to disk.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone

MOVES = "e4,e5,Nf3"
FEN_AFTER_NF3 = "rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2"
KEPT_HEADERS = ("content-type", "cache-control", "etag")


def request(base: str, method: str, path: str, params: dict[str, str] | None = None,
            headers: dict[str, str] | None = None) -> tuple[int, dict[str, str], bytes]:
    url = base.rstrip("/") + path
    if params:
        url += "?" + urllib.parse.urlencode(params, quote_via=urllib.parse.quote)
    req = urllib.request.Request(url, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            return response.status, dict(response.headers), response.read()
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers), error.read()


def sample(name: str, status: int, headers: dict[str, str], body: bytes) -> dict:
    lowered = {k.lower(): v for k, v in headers.items()}
    kept = {k: lowered[k] for k in KEPT_HEADERS if k in lowered}
    text = body.decode("utf-8", errors="replace")
    if not text:
        payload = {"text": ""}
    else:
        try:
            payload = {"json": json.loads(text)}
        except json.JSONDecodeError:
            payload = {"text": text}
    return {"name": name, "status": status, "headers": kept, **payload}


def record(args: argparse.Namespace) -> list[dict]:
    base = args.url
    samples = []

    def add(name, method, path, params=None, headers=None, target=None):
        status, hdrs, body = request(target or base, method, path, params, headers)
        samples.append(sample(name, status, hdrs, body))
        return status, hdrs, body

    add("health", "GET", "/health")
    add("ready", "GET", "/ready")
    add("collections", "GET", "/v1/collections")
    add("position_masters_moves", "GET", "/v1/explorer/position",
        {"collection": "masters-otb", "moves": MOVES})
    add("position_elite_moves", "GET", "/v1/explorer/position",
        {"collection": "elite-online", "moves": MOVES})
    add("position_elite_fen", "GET", "/v1/explorer/position",
        {"collection": "elite-online", "fen": FEN_AFTER_NF3})
    add("position_masters_fen", "GET", "/v1/explorer/position",
        {"collection": "masters-otb", "fen": FEN_AFTER_NF3})
    add("error_invalid_collection", "GET", "/v1/explorer/position",
        {"collection": "bogus", "moves": MOVES})
    add("error_missing_position", "GET", "/v1/explorer/position", {"collection": "masters-otb"})
    add("error_ambiguous_position", "GET", "/v1/explorer/position",
        {"collection": "masters-otb", "moves": "e4", "fen": FEN_AFTER_NF3})
    add("error_invalid_moves", "GET", "/v1/explorer/position",
        {"collection": "masters-otb", "moves": "e4,e4"})
    add("error_invalid_fen", "GET", "/v1/explorer/position",
        {"collection": "masters-otb", "fen": "8/8/8/8/8/8/8/8 w - - 0 1"})
    add("error_unsupported_parameter", "GET", "/v1/explorer/position",
        {"collection": "masters-otb", "moves": "e4", "since_year": "2020"})
    add("error_move_sequence_too_long", "GET", "/v1/explorer/position",
        {"collection": "masters-otb", "moves": ",".join(["e4"] * 201)})

    game_id = args.sample_game_id
    if game_id:
        add("game_elite", "GET", f"/v1/collections/elite-online/games/{game_id}")
        add("error_game_not_found", "GET", "/v1/collections/elite-online/games/1")
        add("error_game_collection_mismatch", "GET",
            f"/v1/collections/masters-otb/games/{game_id}")
    add("error_game_invalid_id", "GET", "/v1/collections/elite-online/games/12x")
    # A repeated parameter cannot be expressed as a dict, so it is written into the path.
    add("error_invalid_query", "GET",
        "/v1/explorer/position?collection=masters-otb&moves=e4&moves=d4")
    add("error_not_found", "GET", "/v1/no-such-route")
    add("error_method_not_allowed", "POST", "/v1/explorer/position",
        {"collection": "masters-otb", "moves": "e4"})

    if args.admin_token:
        add("admin_unauthorized", "POST", "/v1/admin/reload",
            headers={"Authorization": "Bearer wrong-token"})
        add("admin_reload_ok", "POST", "/v1/admin/reload",
            headers={"Authorization": f"Bearer {args.admin_token}"})

    if args.elite_only_url:
        add("error_collection_unavailable", "GET", "/v1/explorer/position",
            {"collection": "masters-otb", "moves": MOVES}, target=args.elite_only_url)
        add("admin_disabled", "POST", "/v1/admin/reload",
            headers={"Authorization": "Bearer any"}, target=args.elite_only_url)

    return samples


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--url", default=os.environ.get("OPENING_EXPLORER_URL", "http://127.0.0.1:8080"),
                        help="server with both collections loaded")
    parser.add_argument("--elite-only-url", help="server with only elite-online loaded")
    parser.add_argument("--admin-token", default=os.environ.get("OPENING_EXPLORER_ADMIN_TOKEN"),
                        help="admin token of the --url server (reload samples are skipped if absent)")
    parser.add_argument("--sample-game-id", help="a game ID present in elite-online on --url")
    parser.add_argument("--out", required=True, help="output directory (created if missing)")
    args = parser.parse_args(argv)

    if not re.fullmatch(r"[0-9]*", args.sample_game_id or ""):
        parser.error("--sample-game-id must be decimal digits")

    samples = record(args)
    os.makedirs(args.out, exist_ok=True)
    for item in samples:
        path = os.path.join(args.out, f"{item['name']}.json")
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(item, handle, indent=2, ensure_ascii=False)
            handle.write("\n")

    manifest = {
        "recorded_at_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "source": "synthetic sample datasets built from fixtures/stage_a_validation.pgn; not a corpus release",
        "sample_count": len(samples),
        "sample_game_id": args.sample_game_id or None,
        "samples": sorted(item["name"] for item in samples),
    }
    with open(os.path.join(args.out, "manifest.json"), "w", encoding="utf-8") as handle:
        json.dump(manifest, handle, indent=2)
        handle.write("\n")
    print(f"recorded {len(samples)} samples in {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
