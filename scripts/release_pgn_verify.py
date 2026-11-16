#!/usr/bin/env python3
"""Independent verification of the filtered PGN export against its canonical snapshot.

This checker shares no code with the Rust exporter or the Rust dataset reader. It:

* decodes the CGB3 shards directly from the on-disk format (shard footer and block
  directory, zstd block payloads, string dictionaries, fixed-width game records);
* reads source-attribution.jsonl itself and applies the published tag rules
  (distinct non-empty Source values joined by "; "; titles, FIDE IDs, and Mode are
  written only when every attribution agrees);
* checks EVERY PGN game at the header level (no board replay): the PGN-visible metadata
  against the canonical record, the sidecar-derived tags, the Result token, PlyCount,
  SetUp/FEN consistency, duplicate tags, and the number of SAN tokens against the
  canonical ply count;
* checks a deterministic SAMPLE of games (every Nth game, default 1 = all) with
  python-chess: legality of every mainline move, mainline result, and the complete
  mainline (packed as CompactMove words) compared byte-for-byte with CGB3;
* optionally (full sampling only) replays probe move sequences over the PGN, using
  position identity = board, side to move, castling rights, and legal en passant square,
  and compares next-move and win/draw/loss counts with `opening-explorer query --json`.

Full python-chess replay of every game is CPU-bound (about 3 to 5 hours of CPU for the
3,284,429-game Masters release on a two-core host), so the default run is the header
check for all games plus a sampled legality check. The sample is reported as such.

Use the scratch environment (python-chess and zstandard are not repository dependencies):

    data/work/dist-eval/venv/bin/python -I scripts/release_pgn_verify.py \
        --pgn <file.pgn.zst> --snapshot <version dir> --sample-every 8 \
        --output <evidence.json>
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import multiprocessing
import os
import platform
import re
import struct
import sys
import time
from pathlib import Path
from typing import Any, Iterable, Iterator

try:  # Optional at import time so the pure decoding helpers stay unit-testable.
    import chess
    import chess.pgn
except ImportError:  # pragma: no cover - exercised only without the scratch venv
    chess = None  # type: ignore[assignment]

try:
    import zstandard
except ImportError:  # pragma: no cover
    zstandard = None  # type: ignore[assignment]

CGB3_MAGIC = b"CGB3"
SHARD_HEADER_MAGIC = b"CGCS"
SHARD_FOOTER_MAGIC = b"CGCE"
SHARD_FOOTER_SIZE = 20
SHARD_DIRECTORY_ENTRY = struct.Struct(">QIIIQ")
GAME_RECORD = struct.Struct(">QHHHHHHHHHHHHHBH")
NO_INDEX = 0xFFFF
RESULT_BY_BYTE = {0: "*", 1: "1-0", 2: "1/2-1/2", 3: "0-1"}
RESULT_TOKENS = {"1-0", "0-1", "1/2-1/2", "*"}
MOVE_NUMBER = re.compile(r"^\d+\.+$")
PROMOTION_CODE = {"n": 1, "b": 2, "r": 3, "q": 4}
UNKNOWN_TEXT = "?"
UNKNOWN_DATE = "????.??.??"
MOVE_PROBE_DEPTH = 40
READ_CHUNK = 1 << 22
BATCH_GAMES = 400
TAG_LINE = re.compile(r'^\[([A-Za-z0-9_]+) "((?:[^"\\]|\\.)*)"\]$')
TAG_ESCAPE = re.compile(r"\\(.)")
ATTRIBUTION_TAGS = ("Source", "WhiteTitle", "BlackTitle", "WhiteFideId", "BlackFideId", "Mode")


class VerificationError(RuntimeError):
    """A fail-closed verification error."""


def require_runtime(full: bool) -> None:
    if zstandard is None:
        raise VerificationError("zstandard is required; see docs/DATASET_RELEASES.md")
    if full and chess is None:
        raise VerificationError("python-chess is required for the full check; see docs/DATASET_RELEASES.md")


def blake(data: bytes) -> bytes:
    return hashlib.blake2b(data, digest_size=16).digest()


# --------------------------------------------------------------------------- #
# Canonical side: direct CGB3 decoding and sidecar rules
# --------------------------------------------------------------------------- #
def parse_cgb3(raw: bytes) -> Iterator[dict[str, Any]]:
    """Yield canonical records from one decompressed CGB3 block payload."""
    if raw[:4] != CGB3_MAGIC:
        raise VerificationError("block payload does not start with CGB3")
    (count,) = struct.unpack_from(">I", raw, 4)
    position = 8
    dictionaries: list[list[str]] = []
    for _ in range(10):
        (entries,) = struct.unpack_from(">H", raw, position)
        position += 2
        items: list[str] = []
        for _ in range(entries):
            (length,) = struct.unpack_from(">H", raw, position)
            position += 2
            items.append(raw[position: position + length].decode("utf-8"))
            position += length
        dictionaries.append(items)
    players, events, sites, rounds, dates, ecos, time_controls, sources, broadcasts, fens = dictionaries

    def pick(items: list[str], index: int) -> str | None:
        return None if index == NO_INDEX else items[index]

    for _ in range(count):
        (game_id, w_idx, b_idx, ev, st, rd, dt, eco, tc, src, burl, fen, w_elo, b_elo, result, move_count) = (
            GAME_RECORD.unpack_from(raw, position)
        )
        position += GAME_RECORD.size
        move_bytes = raw[position: position + 2 * move_count]
        position += 2 * move_count
        if len(move_bytes) != 2 * move_count:
            raise VerificationError("truncated move list in CGB3 block")
        if result not in RESULT_BY_BYTE:
            raise VerificationError(f"invalid result byte {result} in CGB3 block")
        yield {
            "id": game_id,
            "white": players[w_idx],
            "black": players[b_idx],
            "white_elo": w_elo or None,
            "black_elo": b_elo or None,
            "event": pick(events, ev),
            "site": pick(sites, st),
            "round": pick(rounds, rd),
            "date": pick(dates, dt),
            "eco": pick(ecos, eco),
            "time_control": pick(time_controls, tc),
            "source_id": pick(sources, src),
            "broadcast_url": pick(broadcasts, burl),
            "fen": pick(fens, fen),
            "result": RESULT_BY_BYTE[result],
            "moves": move_bytes,
        }
    if position != len(raw):
        raise VerificationError("unexpected trailing bytes after the last CGB3 game")


def iter_shard_blocks(shard_name: str, data: bytes, expected_blocks: int, expected_games: int) -> Iterator[bytes]:
    if data[:4] != SHARD_HEADER_MAGIC or data[-4:] != SHARD_FOOTER_MAGIC:
        raise VerificationError(f"{shard_name} is not a CGCS/CGCE shard")
    directory_offset, block_count = struct.unpack_from(">QI", data, len(data) - SHARD_FOOTER_SIZE)
    if block_count != expected_blocks:
        raise VerificationError(f"{shard_name} block count {block_count} != manifest {expected_blocks}")
    decompressor = zstandard.ZstdDecompressor()
    games = 0
    for block in range(block_count):
        offset, compressed, uncompressed, count, _first = SHARD_DIRECTORY_ENTRY.unpack_from(
            data, directory_offset + block * SHARD_DIRECTORY_ENTRY.size
        )
        raw = decompressor.decompress(data[offset: offset + compressed], max_output_size=uncompressed)
        if len(raw) != uncompressed:
            raise VerificationError(f"{shard_name} block {block} decompressed to the wrong size")
        games += count
        yield raw
    if games != expected_games:
        raise VerificationError(f"{shard_name} holds {games} games, manifest declares {expected_games}")


def load_sidecar(path: Path, expected_entries: int) -> dict[int, bytes]:
    """Map each game ID to the digest of its expected sidecar-derived tags."""
    digests: dict[int, bytes] = {}
    with path.open("rb") as stream:
        for raw_line in stream:
            record = json.loads(raw_line)
            if record.get("schema_version") != 1:
                raise VerificationError("unsupported sidecar schema version")
            game_id = record["game_id"]
            if game_id in digests:
                raise VerificationError("sidecar lists a game ID more than once")
            digests[game_id] = blake(sidecar_text(record["source_attributions"]).encode("utf-8"))
    if len(digests) != expected_entries:
        raise VerificationError(f"sidecar has {len(digests)} entries, manifest declares {expected_entries}")
    return digests


def distinct_values(values: Iterable[Any]) -> list[str]:
    return sorted({value for value in values if isinstance(value, str) and value})


def single_or_empty(values: Iterable[Any]) -> str:
    unique = distinct_values(values)
    return unique[0] if len(unique) == 1 else ""


def expected_sidecar_tags(attributions: list[dict[str, Any]]) -> dict[str, str]:
    return {
        "Source": "; ".join(distinct_values(a.get("source_tag") for a in attributions)),
        "WhiteTitle": single_or_empty(a.get("white_title") for a in attributions),
        "BlackTitle": single_or_empty(a.get("black_title") for a in attributions),
        "WhiteFideId": single_or_empty(a.get("white_fide_id") for a in attributions),
        "BlackFideId": single_or_empty(a.get("black_fide_id") for a in attributions),
        "Mode": single_or_empty(a.get("mode") for a in attributions),
    }


def sidecar_text(attributions: list[dict[str, Any]]) -> str:
    tags = expected_sidecar_tags(attributions)
    return "\x1f".join(tags[name] for name in ATTRIBUTION_TAGS)


def expected_core(record: dict[str, Any]) -> tuple[str, bytes]:
    """PGN-visible core fields for a canonical record (without moves) and the raw move words."""
    fields = [
        record["white"] or UNKNOWN_TEXT,
        record["black"] or UNKNOWN_TEXT,
        str(record["white_elo"]) if record["white_elo"] else "",
        str(record["black_elo"]) if record["black_elo"] else "",
        UNKNOWN_DATE if record["date"] is None else record["date"],
        (record["event"] or UNKNOWN_TEXT),
        (record["site"] or UNKNOWN_TEXT),
        (record["round"] or UNKNOWN_TEXT),
        record["result"],
        record["eco"] or "",
        record["time_control"] or "",
        record["source_id"] or "",
        record["broadcast_url"] or "",
        record["fen"] or "",
    ]
    return "\x1f".join(fields), record["moves"]


def canonical_digests(snapshot: Path, manifest: dict[str, Any],
                      sidecar: dict[int, bytes]) -> Iterator[tuple[bytes, bytes, bytes, int]]:
    """Yield (header digest, sidecar digest, full digest, game ID) in physical order."""
    shards = sorted(manifest["shards"], key=lambda item: item["shard_id"])
    seen = 0
    for shard in shards:
        data = (snapshot / "shards" / shard["filename"]).read_bytes()
        if hashlib.sha256(data).hexdigest() != shard["sha256"]:
            raise VerificationError(f"shard {shard['filename']} SHA-256 differs from the manifest")
        for raw in iter_shard_blocks(shard["filename"], data, shard["block_count"], shard["game_count"]):
            for record in parse_cgb3(raw):
                text, moves = expected_core(record)
                side = sidecar.get(record["id"])
                if side is None:
                    raise VerificationError("canonical game has no sidecar entry")
                plies = len(moves) // 2
                header = blake(f"{text}\x1e{plies}".encode("utf-8"))
                full = blake(text.encode("utf-8") + b"\x1e" + moves)
                yield header, side, full, record["id"]
                seen += 1
    if seen != manifest["total_games"]:
        raise VerificationError(f"decoded {seen} canonical games, manifest declares {manifest['total_games']}")


# --------------------------------------------------------------------------- #
# PGN side
# --------------------------------------------------------------------------- #
def unescape_tag(value: str) -> str:
    return TAG_ESCAPE.sub(lambda match: match.group(1), value)


def split_pgn_text(text: str) -> tuple[dict[str, str], list[str], list[str]]:
    """Return (tags, problems, movetext tokens) for one exported game, without python-chess."""
    problems: list[str] = []
    header_lines: list[str] = []
    movetext_lines: list[str] = []
    in_movetext = False
    for line in text.split("\n"):
        if not in_movetext and line.startswith("["):
            header_lines.append(line)
        elif not in_movetext and line.strip() == "":
            continue
        else:
            in_movetext = True
            movetext_lines.append(line)
    tags: dict[str, str] = {}
    for line in header_lines:
        match = TAG_LINE.match(line)
        if match is None:
            problems.append("malformed tag line")
            continue
        name, value = match.group(1), unescape_tag(match.group(2))
        if name in tags:
            problems.append(f"duplicate tag {name}")
        tags[name] = value
    tokens = "\n".join(movetext_lines).split()
    if not tokens or tokens[-1] != tags.get("Result"):
        problems.append("movetext does not end with the Result tag")
    return tags, problems, tokens


def core_text(tags: dict[str, str]) -> str:
    return "\x1f".join([
        tags.get("White", ""), tags.get("Black", ""), tags.get("WhiteElo", ""), tags.get("BlackElo", ""),
        tags.get("Date", ""), tags.get("Event", ""), tags.get("Site", ""), tags.get("Round", ""),
        tags.get("Result", ""), tags.get("ECO", ""), tags.get("TimeControl", ""), tags.get("GameURL", ""),
        tags.get("BroadcastURL", ""), tags.get("FEN", ""),
    ])


def side_text(tags: dict[str, str]) -> str:
    return "\x1f".join(tags.get(name, "") for name in ATTRIBUTION_TAGS)


def parse_headers(text: str) -> dict[str, Any]:
    """Header-level check for one game: metadata, sidecar tags, and SAN token count (no board)."""
    tags, problems, tokens = split_pgn_text(text)
    san_tokens = [token for token in tokens if not MOVE_NUMBER.match(token) and token not in RESULT_TOKENS]
    if str(len(san_tokens)) != tags.get("PlyCount", ""):
        problems.append("PlyCount does not equal the SAN token count")
    if ("FEN" in tags) != (tags.get("SetUp") == "1"):
        problems.append("SetUp and FEN tags are inconsistent")
    plies = len(san_tokens)
    return {
        "problems": problems,
        "header_digest": blake(f"{core_text(tags)}\x1e{plies}".encode("utf-8")),
        "side_digest": blake(side_text(tags).encode("utf-8")),
        "tags": tags,
        "plies": plies,
    }


def parse_pgn_game(text: str) -> dict[str, Any]:
    """Full check for one game: python-chess legality, mainline, and move-word digest."""
    tags, problems, _tokens = split_pgn_text(text)
    game = chess.pgn.read_game(io.StringIO(text))
    if game is None:
        problems.append("python-chess found no game")
        return {"problems": problems}
    if game.errors:
        problems.append(f"python-chess reported {len(game.errors)} error(s)")
    moves = list(game.mainline_moves())
    if str(len(moves)) != tags.get("PlyCount", ""):
        problems.append("PlyCount does not equal the mainline length")
    packed = []
    for move in moves:
        promotion = PROMOTION_CODE.get(chess.piece_symbol(move.promotion).lower(), 0) if move.promotion else 0
        packed.append(move.from_square | (move.to_square << 6) | (promotion << 12))
    move_bytes = struct.pack(f">{len(packed)}H", *packed)
    core = core_text(tags)
    return {
        "problems": problems,
        "header_digest": blake(f"{core}\x1e{len(moves)}".encode("utf-8")),
        "full_digest": blake(core.encode("utf-8") + b"\x1e" + move_bytes),
        "side_digest": blake(side_text(tags).encode("utf-8")),
        "board_moves": moves,
        "result": tags.get("Result", ""),
        "start_fen": tags.get("FEN"),
    }


def position_key(board: Any) -> tuple[Any, ...]:
    en_passant = board.ep_square if board.has_legal_en_passant() else None
    return (board.board_fen(), board.turn, board.castling_rights, en_passant)


_PROBES: dict[tuple[Any, ...], str] = {}
_FULL_CHECK = False
_SAMPLE_EVERY = 1


def _worker_init(probe_specs: list[tuple[str, str]], full_check: bool, sample_every: int) -> None:
    global _FULL_CHECK, _SAMPLE_EVERY
    _FULL_CHECK = full_check
    _SAMPLE_EVERY = sample_every
    _PROBES.clear()
    if chess is None:
        return
    for probe_id, moves in probe_specs:
        board = chess.Board()
        for token in moves.split():
            board.push_uci(token)
        _PROBES[position_key(board)] = probe_id


def process_batch(items: list[tuple[int, str]]) -> dict[str, Any]:
    results: list[tuple[bytes, bytes, bytes | None]] = []
    problems: list[dict[str, Any]] = []
    probe_totals: dict[str, list[int]] = {}
    probe_moves: dict[str, dict[str, list[int]]] = {}
    for global_index, text in items:
        sampled = _FULL_CHECK and global_index % _SAMPLE_EVERY == 0
        if not sampled:
            header = parse_headers(text)
            if header["problems"]:
                problems.append({"game_index": global_index, "problems": header["problems"]})
            results.append((header["header_digest"] if not header["problems"] else b"",
                            header["side_digest"], None))
            continue
        parsed = parse_pgn_game(text)
        if parsed["problems"] or "full_digest" not in parsed:
            problems.append({"game_index": global_index, "problems": parsed["problems"]})
            results.append((b"", b"", None))
            continue
        results.append((parsed["header_digest"], parsed["side_digest"], parsed["full_digest"]))
        if not _PROBES:
            continue
        outcome = {"1-0": 1, "1/2-1/2": 2, "0-1": 3}.get(parsed["result"], 0)
        board = chess.Board(parsed["start_fen"]) if parsed["start_fen"] else chess.Board()
        moves = parsed["board_moves"]
        seen: set[str] = set()
        for ply in range(0, min(len(moves), MOVE_PROBE_DEPTH) + 1):
            key = position_key(board)
            probe_id = _PROBES.get(key)
            if probe_id is not None and probe_id not in seen:
                seen.add(probe_id)
                totals = probe_totals.setdefault(probe_id, [0, 0, 0, 0])
                totals[0] += 1
                totals[outcome] += 1 if outcome else 0
                if ply < len(moves):
                    stats = probe_moves.setdefault(probe_id, {}).setdefault(moves[ply].uci(), [0, 0, 0, 0])
                    stats[0] += 1
                    stats[outcome] += 1 if outcome else 0
            if ply < len(moves):
                board.push(moves[ply])
    return {
        "results": results,
        "problems": problems,
        "probe_totals": probe_totals,
        "probe_moves": probe_moves,
    }


def iter_pgn_games(pgn: Path, digest_sink: Any) -> Iterator[str]:
    """Stream decompressed PGN games, hashing every plain byte exactly once."""
    decompressor = zstandard.ZstdDecompressor()

    def plain_lines() -> Iterator[bytes]:
        tail = b""
        with pgn.open("rb") as raw, decompressor.stream_reader(raw) as stream:
            while True:
                chunk = stream.read(READ_CHUNK)
                if not chunk:
                    break
                digest_sink.update(chunk)
                pieces = (tail + chunk).split(b"\n")
                tail = pieces.pop()
                yield from pieces
        if tail:
            yield tail

    current: list[bytes] = []
    in_movetext = False
    for line in plain_lines():
        if line.startswith(b"[") and in_movetext:
            yield b"\n".join(current).decode("utf-8")
            current, in_movetext = [], False
        if not line.startswith(b"[") and line.strip():
            in_movetext = True
        if not current and not line.strip():
            continue
        current.append(line)
    if current:
        yield b"\n".join(current).decode("utf-8")


def batched(games: Iterable[str], size: int) -> Iterator[list[tuple[int, str]]]:
    batch: list[tuple[int, str]] = []
    for index, text in enumerate(games):
        batch.append((index, text))
        if len(batch) == size:
            yield batch
            batch = []
    if batch:
        yield batch


# --------------------------------------------------------------------------- #
# Probes against the installed snapshot
# --------------------------------------------------------------------------- #
def codb_probe(codb: Path, root: Path, moves: str) -> dict[str, Any]:
    import subprocess

    args = [str(codb), "query", "--dataset", str(root), "--json"]
    if moves:
        args += ["--moves", moves]
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode != 0:
        raise VerificationError(f"codb query failed for '{moves}': {result.stderr.strip()}")
    return json.loads(result.stdout)


def python_probe_expected(probe_totals: dict[str, list[int]], probe_moves: dict[str, dict[str, list[int]]],
                          probe_id: str) -> dict[str, Any]:
    totals = probe_totals.get(probe_id, [0, 0, 0, 0])
    moves = probe_moves.get(probe_id, {})
    return {
        "total_games": totals[0],
        "white_wins": totals[1],
        "draws": totals[2],
        "black_wins": totals[3],
        "moves": {uci: {"total": v[0], "white_wins": v[1], "draws": v[2], "black_wins": v[3]}
                  for uci, v in sorted(moves.items())},
    }


def compare_probe(expected: dict[str, Any], codb_output: dict[str, Any]) -> list[str]:
    differences = []
    for field in ("total_games", "white_wins", "draws", "black_wins"):
        if expected[field] != codb_output.get(field):
            differences.append(f"{field}: python {expected[field]} != codb {codb_output.get(field)}")
    codb_moves = {
        item["uci"]: {"total": item["total"], "white_wins": item["white_wins"],
                      "draws": item["draws"], "black_wins": item["black_wins"]}
        for item in codb_output.get("moves", [])
    }
    if codb_moves != expected["moves"]:
        differences.append("per-move next-move counts differ")
    return differences


# --------------------------------------------------------------------------- #
# Driver
# --------------------------------------------------------------------------- #
def verify(args: argparse.Namespace) -> dict[str, Any]:
    if args.sample_every < 1:
        raise VerificationError("--sample-every must be at least 1")
    full_check = args.sample_every >= 0 and not args.headers_only
    require_runtime(full=full_check)
    started = time.time()
    snapshot = Path(args.snapshot).resolve()
    manifest = json.loads((snapshot / "manifest.json").read_text(encoding="utf-8"))
    expected_games = manifest["total_games"]
    meta = manifest.get("source_attribution")
    if not meta:
        raise VerificationError("manifest has no source-attribution sidecar")
    sidecar_path = snapshot / meta["filename"]
    if hashlib.sha256(sidecar_path.read_bytes()).hexdigest() != meta["sha256"]:
        raise VerificationError("sidecar SHA-256 differs from the manifest")
    sidecar = load_sidecar(sidecar_path, meta["entry_count"])
    sidecar_seconds = time.time() - started

    # Probes only make sense over every game, because CODB counts all games.
    run_probes = bool(args.probes) and full_check and args.sample_every == 1
    probes = json.loads(Path(args.probes).read_text(encoding="utf-8")) if run_probes else []
    probe_specs = [(probe["id"], probe["moves"]) for probe in probes]

    pgn_path = Path(args.pgn).resolve()
    plain_hasher = hashlib.sha256()
    compressed_sha = hashlib.sha256(pgn_path.read_bytes()).hexdigest()
    canonical = canonical_digests(snapshot, manifest, sidecar)

    counters = {
        "pgn_games": 0,
        "canonical_games": 0,
        "header_checked": 0,
        "full_checked": 0,
        "parse_problem_games": 0,
        "header_mismatch": 0,
        "sidecar_mismatch": 0,
        "full_mismatch": 0,
    }
    problem_examples: list[dict[str, Any]] = []
    probe_totals: dict[str, list[int]] = {}
    probe_moves: dict[str, dict[str, list[int]]] = {}
    mismatch_examples: list[dict[str, Any]] = []

    games = iter_pgn_games(pgn_path, plain_hasher)
    context = multiprocessing.get_context("fork")
    with context.Pool(processes=max(1, os.cpu_count() or 1), initializer=_worker_init,
                      initargs=(probe_specs, full_check, args.sample_every)) as pool:
        for batch_result in pool.imap(process_batch, batched(games, BATCH_GAMES)):
            for header_digest, side_digest, full_digest in batch_result["results"]:
                index = counters["pgn_games"]
                counters["pgn_games"] += 1
                try:
                    canon_header, canon_side, canon_full, _canon_id = next(canonical)
                except StopIteration:
                    raise VerificationError("PGN contains more games than the canonical snapshot")
                counters["canonical_games"] += 1
                if not header_digest:
                    counters["parse_problem_games"] += 1
                    continue
                counters["header_checked"] += 1
                if header_digest != canon_header:
                    counters["header_mismatch"] += 1
                    if len(mismatch_examples) < 20:
                        mismatch_examples.append({"pgn_game_index": index, "kind": "header"})
                if side_digest != canon_side:
                    counters["sidecar_mismatch"] += 1
                    if len(mismatch_examples) < 20:
                        mismatch_examples.append({"pgn_game_index": index, "kind": "sidecar"})
                if full_digest is not None:
                    counters["full_checked"] += 1
                    if full_digest != canon_full:
                        counters["full_mismatch"] += 1
                        if len(mismatch_examples) < 20:
                            mismatch_examples.append({"pgn_game_index": index, "kind": "moves"})
            for problem in batch_result["problems"]:
                if len(problem_examples) < 20:
                    problem_examples.append(problem)
            for probe_id, totals in batch_result["probe_totals"].items():
                merged = probe_totals.setdefault(probe_id, [0, 0, 0, 0])
                for i, value in enumerate(totals):
                    merged[i] += value
            for probe_id, by_move in batch_result["probe_moves"].items():
                merged_moves = probe_moves.setdefault(probe_id, {})
                for uci, values in by_move.items():
                    target = merged_moves.setdefault(uci, [0, 0, 0, 0])
                    for i, value in enumerate(values):
                        target[i] += value
    if next(canonical, None) is not None:
        raise VerificationError("canonical snapshot contains more games than the PGN")
    if counters["pgn_games"] != expected_games:
        raise VerificationError(f"PGN contains {counters['pgn_games']} games, expected {expected_games}")

    probe_results = []
    probe_failures = 0
    if run_probes and args.codb and args.installed_root:
        for probe in probes:
            expected = python_probe_expected(probe_totals, probe_moves, probe["id"])
            codb_output = codb_probe(Path(args.codb), Path(args.installed_root), probe["moves"])
            differences = compare_probe(expected, codb_output)
            probe_failures += 1 if differences else 0
            probe_results.append({
                "id": probe["id"],
                "moves": probe["moves"],
                "python": {k: expected[k] for k in ("total_games", "white_wins", "draws", "black_wins")},
                "codb": {k: codb_output.get(k) for k in ("total_games", "white_wins", "draws", "black_wins")},
                "match": not differences,
                "differences": differences,
            })

    failed_checks = [name for name, value in (
        ("parse_problem_games", counters["parse_problem_games"]),
        ("header_mismatch", counters["header_mismatch"]),
        ("sidecar_mismatch", counters["sidecar_mismatch"]),
        ("full_mismatch", counters["full_mismatch"]),
        ("probe_mismatch", probe_failures),
    ) if value]
    evidence = {
        "schema_version": 2,
        "verifier": "scripts/release_pgn_verify.py",
        "independence": (
            "CGB3 and sidecar decoded here from the on-disk format; PGN parsed here with "
            "python-chess for the sampled full check; no Rust export or DatasetReader code is used"
        ),
        "result": "FAIL" if failed_checks else "PASS",
        "failed_checks": failed_checks,
        "coverage": {
            "headers_all_games": counters["pgn_games"] == expected_games,
            "full_legality_sample_every": args.sample_every if full_check else None,
            "full_checked_games": counters["full_checked"],
            "probes_full_replay": bool(probe_results),
        },
        "pgn": {
            "file_name": pgn_path.name,
            "compressed_bytes": pgn_path.stat().st_size,
            "compressed_sha256": compressed_sha,
            "plain_sha256": plain_hasher.hexdigest(),
            "games": counters["pgn_games"],
            "expected_games": expected_games,
        },
        "canonical": {
            "dataset_build_id": manifest["dataset_build_id"],
            "shards": len(manifest["shards"]),
            "games": counters["canonical_games"],
            "sidecar_entries": meta["entry_count"],
            "sidecar_sha256": meta["sha256"],
        },
        "checks": {
            "games_header_checked": counters["header_checked"],
            "games_full_checked": counters["full_checked"],
            "games_with_parse_or_tag_problems": counters["parse_problem_games"],
            "games_header_mismatch": counters["header_mismatch"],
            "games_sidecar_tag_mismatch": counters["sidecar_mismatch"],
            "games_move_mismatch_in_full_sample": counters["full_mismatch"],
            "header_check_scope": (
                "every game: all PGN-visible metadata, sidecar tags, Result token, PlyCount, and SAN "
                "token count against the canonical ply count"
            ),
            "full_check_scope": (
                "sampled games (every Nth): python-chess legality of each mainline move, python-chess "
                "error count, and the packed CompactMove mainline compared byte-for-byte with CGB3"
            ),
            "problem_examples": problem_examples[:20],
            "mismatch_examples": mismatch_examples,
        },
        "probes": {
            "count": len(probe_results),
            "mismatches": probe_failures,
            "results": probe_results,
        },
        "timing_seconds": {"sidecar_load": round(sidecar_seconds, 1), "total": round(time.time() - started, 1)},
        "environment": {
            "python": platform.python_version(),
            "python_chess": getattr(chess, "__version__", "not used") if chess else "not installed",
            "zstandard": getattr(zstandard, "__version__", "unknown"),
            "cpu_count": os.cpu_count(),
        },
    }
    return evidence


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pgn", required=True, help="filtered PGN (.pgn or .pgn.zst)")
    parser.add_argument("--snapshot", required=True, help="immutable CODB version directory")
    parser.add_argument("--sample-every", type=int, default=1,
                        help="full python-chess check on every Nth game (1 = every game)")
    parser.add_argument("--headers-only", action="store_true",
                        help="skip python-chess entirely (header check for every game)")
    parser.add_argument("--probes", help="JSON list of {id, moves} probe sequences (full replay, sample 1)")
    parser.add_argument("--codb", help="opening-explorer binary for probe queries")
    parser.add_argument("--installed-root", help="collection root that codb query reads")
    parser.add_argument("--output", help="evidence JSON path (stdout when omitted)")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    evidence = verify(args)
    text = json.dumps(evidence, indent=2, ensure_ascii=False) + "\n"
    if args.output:
        target = Path(args.output)
        if target.exists():
            raise VerificationError(f"refusing to overwrite {target}")
        target.write_text(text, encoding="utf-8")
    else:
        sys.stdout.write(text)
    print(f"[verify-pgn] {evidence['result']}: {evidence['pgn']['games']} games; "
          f"header-checked {evidence['checks']['games_header_checked']}, "
          f"full-checked {evidence['checks']['games_full_checked']}; "
          f"failed checks: {evidence['failed_checks'] or 'none'}", file=sys.stderr)
    return 0 if evidence["result"] == "PASS" else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, VerificationError, json.JSONDecodeError) as error:
        print(f"verification failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
