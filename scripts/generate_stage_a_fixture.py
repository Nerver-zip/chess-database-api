#!/usr/bin/env python3
"""
Generates deterministic Stage A validation corpus of ~1,000 games
with diverse ECO openings, transpositions, edge cases, and known duplicates.
"""

import sys
from pathlib import Path

# Template games representing diverse opening systems and tactical motifs
TEMPLATES = [
    # 1. Ruy Lopez Closed (C84)
    ("Carlsen, Magnus", "Caruana, Fabiano", "World Championship", "2018", "1/2-1/2", 2835, 2832, "C84",
     "1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7 6. Re1 b5 7. Bb3 d6 8. c3 O-O 9. h3 Nb8 10. d4 Nbd7 11. Nbd2 Bb7 12. Bc2 Re8 13. Nf1 Bf8 14. Ng3 g6 15. a4 c5 16. d5 c4 17. Bg5 h6 18. Be3 Nc5 19. Qd2 h5 20. Bg5 Be7 1/2-1/2"),

    # 2. Sicilian Najdorf (B90)
    ("Kasparov, Garry", "Topalov, Veselin", "Wijk aan Zee", "1999", "1-0", 2812, 2700, "B90",
     "1. e4 c5 2. Nf3 d6 3. d4 cxd4 4. Nxd4 Nf6 5. Nc3 a6 6. Be3 e6 7. f3 b5 8. Qd2 Nbd7 9. g4 h6 10. O-O-O Bb7 11. h4 b4 12. Na4 d5 13. Bh3 dxe4 14. g5 hxg5 15. hxg5 exf3 16. gxf6 Nxf6 17. Qf2 Qa5 18. b3 Ne4 1-0"),

    # 3. French Defense Winawer (C18)
    ("Botvinnik, Mikhail", "Capablanca, Jose Raul", "AVRO", "1938", "1-0", 2720, 2725, "C18",
     "1. d4 Nf6 2. c4 e6 3. Nc3 Bb4 4. e3 d5 5. a3 Bxc3+ 6. bxc3 c5 7. cxd5 exd5 8. Bd3 O-O 9. Ne2 b6 10. O-O Ba6 11. Bxa6 Nxa6 12. Bb2 Qd7 13. a4 Rfe8 14. Qd3 c4 15. Qc2 Nb8 16. Rae1 Nc6 17. Ng3 Na5 18. f3 Nb3 1-0"),

    # 4. Queen's Gambit Declined (D37)
    ("Karpov, Anatoly", "Kasparov, Garry", "World Championship", "1985", "0-1", 2705, 2700, "D37",
     "1. d4 d5 2. c4 e6 3. Nc3 Be7 4. Nf3 Nf6 5. Bf4 O-O 6. e3 c5 7. dxc5 Bxc5 8. Qc2 Nc6 9. a3 Qa5 10. Rd1 Be7 11. Nd2 e5 12. Bg5 d4 13. Nb3 Qd8 14. Be2 a5 15. Bxf6 Bxf6 16. Nd5 a4 17. Nc5 Qa5+ 0-1"),

    # 5. King's Indian Defense (E97)
    ("Fischer, Robert J", "Spassky, Boris V", "World Championship", "1972", "1-0", 2785, 2660, "E97",
     "1. c4 e6 2. Nf3 d5 3. d4 Nf6 4. Nc3 Be7 5. Bg5 O-O 6. e3 h6 7. Bh4 b6 8. cxd5 Nxd5 9. Bxe7 Qxe7 10. Nxd5 exd5 11. Rc1 Be6 12. Qa4 c5 13. Qa3 Rc8 14. Bb5 a6 15. dxc5 bxc5 16. O-O Ra7 17. Be2 Nd7 1-0"),

    # 6. Caro-Kann Defense (B17)
    ("Anand, Viswanathan", "Kramnik, Vladimir", "World Championship", "2008", "1/2-1/2", 2783, 2772, "B17",
     "1. d4 d5 2. c4 c6 3. Nf3 Nf6 4. Nc3 e6 5. e3 Nbd7 6. Bd3 dxc4 7. Bxc4 b5 8. Bd3 a6 9. e4 c5 10. e5 cxd4 11. Nxb5 axb5 12. exf6 gxf6 13. O-O Qb6 14. Qe2 Bb7 15. Bxb5 Bd6 16. Rd1 Rg8 17. g3 Ke7 1/2-1/2"),

    # 7. Italian Game (C50)
    ("Morphy, Paul", "Duke of Brunswick", "Paris Opera", "1858", "1-0", 2600, 2200, "C50",
     "1. e4 e5 2. Nf3 d6 3. d4 Bg4 4. dxe5 Bxf3 5. Qxf3 dxe5 6. Bc4 Nf6 7. Qb3 Qe7 8. Nc3 c6 9. Bg5 b5 10. Nxb5 cxb5 11. Bxb5+ Nbd7 12. O-O-O Rd8 13. Rxd7 Rxd7 14. Rd1 Qe6 15. Bxd7+ Nxd7 16. Qb8+ Nxb8 17. Rd8# 1-0"),

    # 8. English Opening (A20)
    ("Ding, Liren", "Nepomniachtchi, Ian", "World Championship", "2023", "1-0", 2788, 2795, "A20",
     "1. c4 e5 2. Nc3 Nf6 3. Nf3 Nc6 4. e3 Bb4 5. Qc2 Bxc3 6. bxc3 d6 7. e4 O-O 8. Be2 Nh5 9. d3 Nf4 10. Bxf4 exf4 11. d4 Bg4 12. O-O Qf6 13. Rfe1 Rfe8 14. h3 Bh5 15. Nh2 Bg6 16. Bd3 Na5 17. Nf3 c5 1-0"),

    # 9. Grunfeld Defense (D85)
    ("Svidler, Peter", "Gelfand, Boris", "Candidates", "2013", "1/2-1/2", 2747, 2740, "D85",
     "1. d4 Nf6 2. c4 g6 3. Nc3 d5 4. cxd5 Nxd5 5. e4 Nxc3 6. bxc3 Bg7 7. Nf3 c5 8. Rb1 O-O 9. Be2 cxd4 10. cxd4 Qa5+ 11. Bd2 Qxa2 12. O-O Bg4 13. Bg5 h6 14. Be3 Nc6 15. d5 Na5 16. Bc5 b6 1/2-1/2"),

    # 10. Nimzo-Indian Defense (E20)
    ("Tal, Mikhail", "Larsen, Bent", "Bled Interzonal", "1965", "1-0", 2700, 2660, "E20",
     "1. e4 c5 2. Nf3 Nc6 3. d4 cxd4 4. Nxd4 e6 5. Nc3 d6 6. Be3 Nf6 7. f4 Be7 8. Qf3 O-O 9. O-O-O Qc7 10. Ndb5 Qb8 11. g4 a6 12. Nd4 Nxd4 13. Bxd4 b5 14. g5 Nd7 15. Bd3 b4 16. Nd5 exd5 17. exd5 1-0"),

    # Edge Case: Underpromotion (Knight promotion)
    ("PromotionMaster", "Defender", "Edge Case Promotion", "2024", "1-0", 2450, 2400, "C00",
     "1. e4 d5 2. exd5 c6 3. dxc6 Nf6 4. cxb7 e5 5. bxa8=N Bc5 6. Qf3 O-O 7. d3 Bg4 8. Qg3 Nbd7 9. Be2 Bxe2 10. Nxe2 Qxa8 1-0"),

    # Edge Case: En Passant chain
    ("PawnBreaker", "PawnShield", "Edge Case En Passant", "2024", "1/2-1/2", 2350, 2300, "B12",
     "1. e4 c6 2. e5 d5 3. exd6 exd6 4. d4 d5 5. c4 Nf6 6. Nc3 Be7 7. Nf3 O-O 8. Be2 dxc4 9. Bxc4 Nbd7 10. O-O Nb6 1/2-1/2"),

    # Transposition Pair A (English to Queen's Gambit)
    ("TransposerA", "OpponentA", "Transposition Event 1", "2023", "1/2-1/2", 2500, 2510, "A17",
     "1. c4 e6 2. d4 d5 3. Nf3 Nf6 4. Nc3 Be7 5. Bg5 O-O 6. e3 h6 7. Bh4 b6 1/2-1/2"),

    # Transposition Pair B (d4 move order to same position)
    ("TransposerB", "OpponentB", "Transposition Event 2", "2024", "1/2-1/2", 2520, 2490, "D58",
     "1. d4 Nf6 2. Nf3 e6 3. c4 d5 4. Nc3 Be7 5. Bg5 O-O 6. e3 h6 7. Bh4 b6 1/2-1/2"),
]

def generate_fixtures(output_path: Path, target_count: int = 1000):
    lines = []
    idx = 1

    # Base templates
    num_templates = len(TEMPLATES)
    
    while idx <= target_count:
        t = TEMPLATES[(idx - 1) % num_templates]
        white, black, event, year, result, w_elo, b_elo, eco, moves = t
        
        # Add slight variations in round, ratings, and site to simulate realistic tournament database
        round_num = ((idx - 1) % 9) + 1
        year_adj = int(year) + ((idx // num_templates) % 10)
        w_adj = w_elo + ((idx % 30) - 15)
        b_adj = b_elo + (((idx * 7) % 30) - 15)
        
        lines.append(f'[Event "{event} Stage A"]')
        lines.append(f'[Site "Venue {idx % 20}"]')
        lines.append(f'[Date "{year_adj}.0{((idx % 12) + 1):02d}.15"]')
        lines.append(f'[Round "{round_num}"]')
        lines.append(f'[White "{white}"]')
        lines.append(f'[Black "{black}"]')
        lines.append(f'[Result "{result}"]')
        lines.append(f'[WhiteElo "{w_adj}"]')
        lines.append(f'[BlackElo "{b_adj}"]')
        lines.append(f'[ECO "{eco}"]')
        lines.append('')
        lines.append(moves)
        lines.append('')
        idx += 1

    output_path.parent.mkdir(parents=True, exist_ok=True)
    with open(output_path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines))
    print(f"Generated {target_count} games in {output_path}")

if __name__ == "__main__":
    out_file = Path(__file__).resolve().parent.parent / "fixtures" / "stage_a_validation.pgn"
    count = int(sys.argv[1]) if len(sys.argv) > 1 else 1000
    generate_fixtures(out_file, count)
