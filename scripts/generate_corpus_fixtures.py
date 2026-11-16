#!/usr/bin/env python3
"""Generate small deterministic PGN fixtures for the offline corpus tests.

The files are intentionally tiny and synthetic. They exercise the real `opening-explorer`
pipeline (policy, deduplication, accounting) without any network access and are
never presented as real corpus data.
"""
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "fixtures" / "corpus"

LINES = [
    "e4 e5 Nf3 Nc6 Bb5 a6 Ba4 Nf6 O-O Be7 Re1 b5",
    "d4 Nf6 c4 e6 Nc3 Bb4 e3 O-O Bd3 d5 Nf3 c5",
    "e4 c5 Nf3 d6 d4 cxd4 Nxd4 Nf6 Nc3 a6 Be2 e5",
    "c4 e5 Nc3 Nf6 Nf3 Nc6 g3 d5 cxd5 Nxd5 Bg2 Nb6",
    "d4 d5 c4 e6 Nc3 Nf6 Bg5 Be7 e3 O-O Nf3 h6",
    "e4 e6 d4 d5 Nc3 Bb4 e5 c5 a3 Bxc3+ bxc3 Ne7",
    "Nf3 d5 g3 Nf6 Bg2 e6 O-O Be7 d3 O-O Nbd2 c5",
    "e4 c6 d4 d5 Nc3 dxe4 Nxe4 Bf5 Ng3 Bg6 h4 h6",
]
RESULTS = ["1-0", "0-1", "1/2-1/2"]


def movetext(san: str, result: str) -> str:
    toks = san.split()
    out = []
    for i, t in enumerate(toks):
        if i % 2 == 0:
            out.append(f"{i // 2 + 1}.")
        out.append(t)
    return " ".join(out) + f" {result}"


def rec(tags: list[tuple[str, str]], san: str, result: str) -> str:
    head = "\n".join(f'[{k} "{v}"]' for k, v in tags)
    return f"{head}\n\n{movetext(san, result)}\n\n"


def broadcast() -> str:
    out = []
    n = 0

    def otb(i: int, event: str, site: str, elo=(2510, 2455), line=None, date="2026.09.01", extra=None, result=None):
        nonlocal n
        n += 1
        slug = event.lower().replace(" ", "-")
        tags = [
            ("Event", event), ("Site", site), ("Date", date), ("Round", f"{i % 9 + 1}.{i}"),
            ("White", f"Master W{n}"), ("Black", f"Master B{n}"), ("Result", result or RESULTS[i % 3]),
            ("WhiteElo", str(elo[0])), ("BlackElo", str(elo[1])), ("Variant", "Standard"),
            ("BroadcastURL", f"https://lichess.org/broadcast/{slug}/round-1/AAAA{n:04d}"),
            ("GameURL", f"https://lichess.org/broadcast/{slug}/round-1/AAAA{n:04d}/G{n:06d}"),
        ] + (extra or [])
        out.append(rec(tags, line or LINES[i % len(LINES)], result or RESULTS[i % 3]))

    for i in range(14):
        otb(i, "Reykjavik Open 2026", "Reykjavik ISL")
    for i in range(3):
        otb(i, "Titled Arena Online 2026", "Lichess")
    otb(3, "World Cup Qualifier", "Chess.com")
    for i in range(3):
        otb(i, "Club Championship 2026", "Oslo NOR", elo=(1820, 1710))
    for i in range(2):
        otb(i, "Open Weekend 2026", "Bergen NOR", elo=(0, 0))
    # missing rating tags entirely
    out.append(rec([("Event", "Unrated Open"), ("Site", "Oslo NOR"), ("Date", "2026.09.02"),
                    ("White", "No Elo A"), ("Black", "No Elo B"), ("Result", "1-0")],
                   LINES[0], "1-0"))
    # Chess960 variant
    otb(1, "Freestyle Festival", "Weissenhaus GER", extra=[("Variant", "Chess960")])
    # illegal move (Nf6 impossible for white on move 2)
    out.append(rec([("Event", "Broken Open"), ("Site", "Oslo NOR"), ("Date", "2026.09.03"), ("Round", "1"),
                    ("White", "Illegal A"), ("Black", "Illegal B"), ("Result", "1-0"),
                    ("WhiteElo", "2500"), ("BlackElo", "2500")], "e4 e5 Nf6 Nc6 Bb5 a6", "1-0"))
    # too short
    otb(2, "Short Draw Open", "Oslo NOR", line="e4 e5")
    # exact duplicate of game 1 (same GameURL, same moves)
    n_dup = 1
    slug = "reykjavik-open-2026"
    out.append(rec([
        ("Event", "Reykjavik Open 2026"), ("Site", "Reykjavik ISL"), ("Date", "2026.09.01"), ("Round", "1.0"),
        ("White", "Master W1"), ("Black", "Master B1"), ("Result", "1-0"), ("WhiteElo", "2510"),
        ("BlackElo", "2455"), ("Variant", "Standard"),
        ("BroadcastURL", f"https://lichess.org/broadcast/{slug}/round-1/AAAA{n_dup:04d}"),
        ("GameURL", f"https://lichess.org/broadcast/{slug}/round-1/AAAA{n_dup:04d}/G{n_dup:06d}")],
        LINES[0], "1-0"))
    # prefix supersession: a live (shorter) record followed by the finished game
    prefix_tags = [("Event", "Live Classic 2026"), ("Site", "Prague CZE"), ("Date", "2026.09.04"),
                   ("Round", "5"), ("White", "Live White"), ("Black", "Live Black"),
                   ("WhiteElo", "2600"), ("BlackElo", "2590"), ("Variant", "Standard"),
                   ("GameURL", "https://lichess.org/broadcast/live-classic-2026/round-5/ZZZZ0001/LIVE0001")]
    out.append(rec(prefix_tags + [("Result", "*")], "e4 e5 Nf3 Nc6 Bb5 a6", "*"))
    out.append(rec(prefix_tags + [("Result", "1-0")], "e4 e5 Nf3 Nc6 Bb5 a6 Ba4 Nf6 O-O Be7", "1-0"))
    # historical: documented (accepted) and bare (rejected)
    out.append(rec([("Event", "AVRO 1938"), ("Site", "Netherlands"), ("Date", "1938.11.06"), ("Round", "3"),
                    ("White", "Keres, Paul"), ("Black", "Fine, Reuben"), ("Result", "1-0")],
                   LINES[1], "1-0"))
    out.append(rec([("Event", "?"), ("Site", "?"), ("Date", "1938.11.07"), ("Round", "?"),
                    ("White", "Unknown A"), ("Black", "Unknown B"), ("Result", "1-0")],
                   LINES[2], "1-0"))
    # malformed record: unterminated tag
    out.append('[Event "Broken header\n[Site "x"]\n\n1. e4 e5 1-0\n\n')
    return "".join(out)


def standard() -> str:
    out = []
    n = 0

    def game(event, tc, elo, line=None, variant="Standard", site_id=None, result=None):
        nonlocal n
        n += 1
        sid = site_id or f"fx{n:06d}"
        tags = [("Event", event), ("Site", f"https://lichess.org/{sid}"), ("Date", "2016.06.15"),
                ("White", f"elite_w{n}"), ("Black", f"elite_b{n}"), ("Result", result or RESULTS[n % 3]),
                ("UTCDate", "2016.06.15"), ("UTCTime", "12:00:00"), ("WhiteElo", str(elo[0])),
                ("BlackElo", str(elo[1])), ("TimeControl", tc), ("Variant", variant)]
        out.append(rec(tags, line or LINES[n % len(LINES)], result or RESULTS[n % 3]))

    tcs = ["180+0", "300+3", "600+0", "900+10", "1800+0"]
    for i in range(16):
        game("Rated Blitz game" if i % 2 else "Rated Rapid game", tcs[i % 5], (2310 + i, 2275))
    game("Rated Blitz game", "300+0", (2450, 2050))      # one >=2400 and other >=2000
    for _ in range(3):
        game("Rated Bullet game", "60+0", (2500, 2480))    # bullet
    game("Rated Bullet game", "180+0", (2500, 2480))       # bullet by event label
    game("Rated Blitz game", "180+0", (2150, 2100))        # below threshold
    game("Rated Blitz game", "180+0", (2399, 1999))        # just below
    game("Casual Blitz game", "180+0", (2500, 2500))       # casual
    game("Rated Crazyhouse game", "180+0", (2500, 2500), variant="Crazyhouse")
    game("Rated Blitz game", "180+0", (2500, 2500), site_id="fx000001")  # duplicate site id of the first
    out.append(rec([("Event", "Rated Blitz game"), ("Site", "https://lichess.org/fxillegal"), ("Date", "2016.06.15"),
                    ("White", "bad_w"), ("Black", "bad_b"), ("Result", "1-0"), ("WhiteElo", "2500"),
                    ("BlackElo", "2500"), ("TimeControl", "180+0"), ("Variant", "Standard")],
                   "e4 e5 Ke3 Nc6 Nf3 Nf6", "1-0"))
    return "".join(out)


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "broadcast_sample.pgn").write_text(broadcast())
    (OUT / "standard_sample.pgn").write_text(standard())
    print(f"wrote fixtures to {OUT}")
