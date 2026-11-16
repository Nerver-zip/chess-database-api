#!/usr/bin/env python3
"""Offline tests for the pure parts of scripts/release_pgn_verify.py.

The CGB3 encoder below is written from the on-disk format description in
crates/storage/src/format.rs, independently of the verifier's decoder, so a round trip
checks the decoder against the format rather than against itself. Tests that need
zstandard or python-chess skip when those packages are not installed; the full
verification runs in the scratch environment documented in docs/DATASET_RELEASES.md.

Run:  python3 -m unittest scripts/test_release_pgn_verify.py
"""

from __future__ import annotations

import hashlib
import io
import struct
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import release_pgn_verify as rpv  # noqa: E402

try:
    import zstandard  # noqa: F401
    HAVE_ZSTD = True
except ImportError:
    HAVE_ZSTD = False

HAVE_CHESS = rpv.chess is not None

RESULT_BYTE = {"*": 0, "1-0": 1, "1/2-1/2": 2, "0-1": 3}
NONE_INDEX = 0xFFFF


def encode_cgb3(games: list[dict]) -> bytes:
    """Encode canonical records in the CGB3 layout (mirror of GameBlock::compress_with_stats)."""
    dictionaries: list[list[str]] = [[] for _ in range(10)]
    maps: list[dict[str, int]] = [{} for _ in range(10)]

    def intern(dictionary: int, value: str | None) -> int:
        if value is None:
            return NONE_INDEX
        if value not in maps[dictionary]:
            maps[dictionary][value] = len(dictionaries[dictionary])
            dictionaries[dictionary].append(value)
        return maps[dictionary][value]

    rows = []
    for game in games:
        rows.append((
            game["id"],
            intern(0, game["white"]), intern(0, game["black"]),
            intern(1, game.get("event")), intern(2, game.get("site")), intern(3, game.get("round")),
            intern(4, game.get("date")), intern(5, game.get("eco")), intern(6, game.get("time_control")),
            intern(7, game.get("source_id")), intern(8, game.get("broadcast_url")), intern(9, game.get("fen")),
            game.get("white_elo") or 0, game.get("black_elo") or 0,
            RESULT_BYTE[game["result"]], game["moves"],
        ))
    out = bytearray(b"CGB3")
    out += struct.pack(">I", len(games))
    for dictionary in dictionaries:
        out += struct.pack(">H", len(dictionary))
        for item in dictionary:
            encoded = item.encode("utf-8")
            out += struct.pack(">H", len(encoded)) + encoded
    for (gid, w, b, ev, st, rd, dt, eco, tc, src, burl, fen, we, be, res, moves) in rows:
        out += struct.pack(">QHHHHHHHHHHHHHBH", gid, w, b, ev, st, rd, dt, eco, tc, src, burl, fen, we, be,
                           res, len(moves))
        for move in moves:
            out += struct.pack(">H", move)
    return bytes(out)


def packed(*moves: tuple[int, int, int]) -> list[int]:
    """(from, to, promotion code) to CompactMove words."""
    return [from_sq | (to_sq << 6) | (promo << 12) for from_sq, to_sq, promo in moves]


E4_E5 = packed((12, 28, 0), (52, 36, 0))
SIMPLE_GAME = {
    "id": 4242, "white": "Carlsen, Magnus", "black": "Nakamura, Hikaru", "white_elo": 2850,
    "black_elo": None, "event": "Tata Steel", "site": None, "round": "5", "date": "2024.01.15",
    "eco": "C65", "time_control": None, "source_id": "https://example.org/g/1",
    "broadcast_url": None, "fen": None, "result": "1/2-1/2", "moves": E4_E5,
}


class CgbDecoderTests(unittest.TestCase):
    def test_round_trip_matches_the_encoded_records(self) -> None:
        fen_game = dict(SIMPLE_GAME, id=7, fen="rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1",
                        result="*", moves=packed((52, 36, 0)), white="Zed, A", black="Yon, B")
        raw = encode_cgb3([SIMPLE_GAME, fen_game])
        decoded = list(rpv.parse_cgb3(raw))
        self.assertEqual([record["id"] for record in decoded], [4242, 7])
        first = decoded[0]
        self.assertEqual(first["white"], "Carlsen, Magnus")
        self.assertEqual(first["white_elo"], 2850)
        self.assertIsNone(first["black_elo"])
        self.assertIsNone(first["site"])
        self.assertEqual(first["result"], "1/2-1/2")
        self.assertEqual(first["moves"], struct.pack(">2H", *E4_E5))
        self.assertIsNone(first["fen"])
        self.assertEqual(decoded[1]["fen"], fen_game["fen"])
        self.assertEqual(decoded[1]["result"], "*")

    def test_trailing_bytes_are_rejected(self) -> None:
        raw = encode_cgb3([SIMPLE_GAME]) + b"\x00"
        with self.assertRaisesRegex(rpv.VerificationError, "trailing"):
            list(rpv.parse_cgb3(raw))

    def test_bad_magic_is_rejected(self) -> None:
        raw = b"CGB2" + encode_cgb3([SIMPLE_GAME])[4:]
        with self.assertRaisesRegex(rpv.VerificationError, "CGB3"):
            list(rpv.parse_cgb3(raw))


class CanonicalRuleTests(unittest.TestCase):
    def _record(self, **overrides: object) -> dict:
        record = next(iter(rpv.parse_cgb3(encode_cgb3([dict(SIMPLE_GAME, **overrides)]))))
        return record

    def test_missing_fields_use_pgn_placeholders(self) -> None:
        text, moves = rpv.expected_core(self._record())
        fields = text.split("\x1f")
        self.assertEqual(fields[0], "Carlsen, Magnus")
        self.assertEqual(fields[2], "2850")
        self.assertEqual(fields[3], "")          # no black Elo
        self.assertEqual(fields[6], "?")         # missing site
        self.assertEqual(fields[12], "")         # no broadcast URL
        self.assertEqual(moves, struct.pack(">2H", *E4_E5))

    def test_missing_date_uses_the_unknown_date_placeholder(self) -> None:
        text, _ = rpv.expected_core(self._record(date=None))
        self.assertEqual(text.split("\x1f")[4], "????.??.??")

    def test_sidecar_tags_follow_the_distinct_value_rules(self) -> None:
        attributions = [
            {"source_tag": "Legacy Software Dump", "white_title": "GM", "black_title": None,
             "white_fide_id": "1001", "black_fide_id": None, "mode": None},
            {"source_tag": "Britbase", "white_title": "IM", "black_title": "GM",
             "white_fide_id": "1001", "black_fide_id": "2002", "mode": None},
        ]
        tags = rpv.expected_sidecar_tags(attributions)
        self.assertEqual(tags["Source"], "Britbase; Legacy Software Dump")
        self.assertEqual(tags["WhiteTitle"], "", "conflicting titles are omitted")
        self.assertEqual(tags["BlackTitle"], "GM", "a single agreed value is kept")
        self.assertEqual(tags["WhiteFideId"], "1001")
        self.assertEqual(tags["BlackFideId"], "2002")
        self.assertEqual(tags["Mode"], "")

    def test_sidecar_sources_ignore_empty_tags(self) -> None:
        tags = rpv.expected_sidecar_tags([{"source_tag": ""}, {"source_tag": None}])
        self.assertEqual(tags["Source"], "")


class SidecarLoadTests(unittest.TestCase):
    def test_duplicate_or_miscounted_sidecars_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "source-attribution.jsonl"
            line = '{"schema_version":1,"game_id":%d,"source_attributions":[{"source_tag":"Britbase"}]}\n'
            path.write_text(line % 1 + line % 1)
            with self.assertRaisesRegex(rpv.VerificationError, "more than once"):
                rpv.load_sidecar(path, 2)
            path.write_text(line % 1 + line % 2)
            with self.assertRaisesRegex(rpv.VerificationError, "manifest declares"):
                rpv.load_sidecar(path, 3)
            digests = rpv.load_sidecar(path, 2)
            self.assertEqual(set(digests), {1, 2})


class ShardLayoutTests(unittest.TestCase):
    """Shard footer and block directory parsing, with zstd only when available."""

    @unittest.skipUnless(HAVE_ZSTD, "zstandard is not installed")
    def test_blocks_are_read_through_the_footer_and_directory(self) -> None:
        import zstandard

        payloads = [encode_cgb3([SIMPLE_GAME]), encode_cgb3([dict(SIMPLE_GAME, id=9)])]
        compressed = [zstandard.ZstdCompressor(level=3).compress(p) for p in payloads]
        body = bytearray(b"CGCS" + b"\x00" * 28)
        offsets = []
        for blob in compressed:
            offsets.append(len(body))
            body += blob
        directory_offset = len(body)
        for offset, blob, payload in zip(offsets, compressed, payloads):
            body += struct.pack(">QIIIQ", offset, len(blob), len(payload), 1, 4242)
        body += struct.pack(">QII", directory_offset, len(compressed), 0) + b"CGCE"
        with tempfile.TemporaryDirectory() as tmp:
            shard = Path(tmp) / "shard_0000.cgb3"
            shard.write_bytes(bytes(body))
            blocks = list(rpv.iter_shard_blocks(shard, shard.read_bytes(), 2, 2))
        self.assertEqual(blocks, payloads)

    @unittest.skipUnless(HAVE_ZSTD, "zstandard is not installed")
    def test_miscounted_block_directory_is_rejected(self) -> None:
        body = b"CGCS" + b"\x00" * 28 + struct.pack(">QII", 32, 0, 0) + b"CGCE"
        with self.assertRaisesRegex(rpv.VerificationError, "block count"):
            list(rpv.iter_shard_blocks(Path("shard.cgb3"), body, 1, 0))


class PgnStreamTests(unittest.TestCase):
    @unittest.skipUnless(HAVE_ZSTD, "zstandard is not installed")
    def test_games_are_split_and_counted_exactly(self) -> None:
        import zstandard

        games = []
        for i in range(3):
            games.append(
                '[Event "E"]\n[Site "?"]\n[Date "2024.01.01"]\n[Round "?"]\n'
                f'[White "P{i}"]\n[Black "Q"]\n[Result "*"]\n\n1. e4 e5 *\n'
            )
        text = "\n".join(games) + "\n"
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "games.pgn.zst"
            path.write_bytes(zstandard.ZstdCompressor(level=3).compress(text.encode("utf-8")))
            sink = hashlib.sha256()
            found = list(rpv.iter_pgn_games(path, sink))
        self.assertEqual(len(found), 3)
        self.assertIn('[White "P1"]', found[1])
        self.assertNotIn('[White "P0"]', found[1])
        self.assertEqual(sink.hexdigest(), hashlib.sha256(text.encode("utf-8")).hexdigest())

    def test_probe_positions_are_keyed_by_legal_board_state(self) -> None:
        if not HAVE_CHESS:
            self.skipTest("python-chess is not installed")
        import chess

        start = chess.Board()
        after_e4 = chess.Board()
        after_e4.push_uci("e2e4")
        self.assertNotEqual(rpv.position_key(start), rpv.position_key(after_e4))
        transposed_a = chess.Board()
        transposed_b = chess.Board()
        for move in ("g1f3", "g8f6", "b1c3", "b8c6"):
            transposed_a.push_uci(move)
        for move in ("b1c3", "b8c6", "g1f3", "g8f6"):
            transposed_b.push_uci(move)
        self.assertEqual(rpv.position_key(transposed_a), rpv.position_key(transposed_b))


class PgnGameParsingTests(unittest.TestCase):
    @unittest.skipUnless(HAVE_CHESS, "python-chess is not installed")
    def test_legal_game_parses_with_matching_core_digest(self) -> None:
        text = (
            '[Event "Tata Steel"]\n[Site "Wijk aan Zee NED"]\n[Date "2024.01.15"]\n[Round "5"]\n'
            '[White "Carlsen, Magnus"]\n[Black "Nakamura, Hikaru"]\n[Result "1/2-1/2"]\n'
            '[WhiteElo "2850"]\n[ECO "C65"]\n[PlyCount "4"]\n\n1. e4 e5 2. Nf3 Nc6 1/2-1/2\n'
        )
        parsed = rpv.parse_pgn_game(text)
        self.assertEqual(parsed["problems"], [])
        fields = [
            "Carlsen, Magnus", "Nakamura, Hikaru", "2850", "", "2024.01.15", "Tata Steel",
            "Wijk aan Zee NED", "5", "1/2-1/2", "C65", "", "", "", "",
        ]
        expected_core = "\x1f".join(fields).encode("utf-8") + b"\x1e" + struct.pack(
            ">4H", 12 | (28 << 6), 52 | (36 << 6), 6 | (21 << 6), 57 | (42 << 6)
        )
        self.assertEqual(parsed["full_digest"], hashlib.blake2b(expected_core, digest_size=16).digest())

    @unittest.skipUnless(HAVE_CHESS, "python-chess is not installed")
    def test_illegal_move_and_bad_termination_are_reported(self) -> None:
        text = (
            '[Event "E"]\n[Site "?"]\n[Date "?"]\n[Round "?"]\n[White "A"]\n[Black "B"]\n'
            '[Result "1-0"]\n[PlyCount "2"]\n\n1. e5 e6 1/2-1/2\n'
        )
        problems = rpv.parse_pgn_game(text)["problems"]
        self.assertTrue(any("error" in item for item in problems), problems)
        self.assertTrue(any("Result" in item for item in problems), problems)

    def test_header_check_counts_san_tokens_without_a_board(self) -> None:
        text = (
            '[Event "E"]\n[Site "?"]\n[Date "2024.01.01"]\n[Round "?"]\n[White "A"]\n[Black "B"]\n'
            '[Result "1-0"]\n[PlyCount "4"]\n\n1. e4 e5 2. Nf3 Nc6 1-0\n'
        )
        parsed = rpv.parse_headers(text)
        self.assertEqual(parsed["problems"], [])
        self.assertEqual(parsed["plies"], 4)
        wrong = text.replace('[PlyCount "4"]', '[PlyCount "5"]')
        self.assertTrue(any("PlyCount" in item for item in rpv.parse_headers(wrong)["problems"]))
        mismatched_result = text.replace('[Result "1-0"]', '[Result "0-1"]')
        self.assertTrue(any("Result" in item for item in rpv.parse_headers(mismatched_result)["problems"]))

    def test_header_digest_is_sensitive_to_ply_count_and_metadata(self) -> None:
        base = ('[Event "E"]\n[Site "?"]\n[Date "2024.01.01"]\n[Round "?"]\n[White "A"]\n[Black "B"]\n'
                '[Result "*"]\n[PlyCount "2"]\n\n1. e4 e5 *\n')
        other = base.replace('[White "A"]', '[White "Z"]')
        shorter = base.replace("1. e4 e5 *", "1. e4 *").replace('[PlyCount "2"]', '[PlyCount "1"]')
        self.assertNotEqual(rpv.parse_headers(base)["header_digest"], rpv.parse_headers(other)["header_digest"])
        self.assertNotEqual(rpv.parse_headers(base)["header_digest"], rpv.parse_headers(shorter)["header_digest"])

    @unittest.skipUnless(HAVE_CHESS, "python-chess is not installed")
    def test_duplicate_tags_are_reported(self) -> None:
        text = '[Event "E"]\n[Event "F"]\n[Result "*"]\n\n*\n'
        self.assertIn("duplicate tag Event", rpv.parse_pgn_game(text)["problems"])


if __name__ == "__main__":
    unittest.main()
