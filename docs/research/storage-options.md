---
summary: "Historical trade-off study evaluating storage candidates (PGN, RocksDB, SQLite, CGB3)."
read_when: "Reviewing the rationale for rejecting raw PGN, RocksDB, and SQLite in favor of CGB3."
status: historical
---

# Research Track D: Compact Game Storage & Layout Architecture

## 1. Storage Evaluation & Baselines

To determine whether millions of chess games can fit within ~10 GB while providing full-game reconstruction and interactive random access, we evaluate four candidate game-storage representations:

```
┌────────────────────────────────────────────────────────┐
│ Candidate 1: Raw Plaintext PGN                         │
│ • Uncompressed ASCII text with full headers & SAN      │
│ • ~850 - 950 bytes / game                              │
└────────────────────────────────────────────────────────┘
                           │ 4.2× compression
┌──────────────────────────▼─────────────────────────────┐
│ Candidate 2: Monolithic Stream-Compressed (PGN.zst)    │
│ • Monolithic Zstandard stream                          │
│ • ~140 - 180 bytes / game                              │
│ • Flaw: ZERO random access (O(N) full decompression)   │
└────────────────────────────────────────────────────────┘
                           │ Structured binary packing
┌──────────────────────────▼─────────────────────────────┐
│ Candidate 3: Uncompressed Binary Packed                │
│ • 16-bit moves + fixed header offsets                  │
│ • ~350 - 450 bytes / game                              │
│ • Direct O(1) byte offset indexing                     │
└────────────────────────────────────────────────────────┘
                           │ 4.2× block-compressed container
┌──────────────────────────▼─────────────────────────────┐
│ Candidate 4: Block-Compressed Container (CGB3)         │
│ • Blocks of 512 games with localized string dicts      │
│ • Block-level Zstandard (Level 3) + Sparse Index       │
│ • ~205.5 - 211.3 bytes / game (Supported-Field Fidelity│
│ • Sub-millisecond random access (O(1) block seek)      │
└────────────────────────────────────────────────────────┘
```

---

## 2. Compact 16-bit Move Specification

### 2.1 Sufficiency Analysis for Standard Chess
Can all standard chess moves (including captures, double pawn pushes, en passant, castling, and pawn promotions) be unambiguously represented in exactly 16 bits?

**Yes.** Standard chess requires tracking:
1. Origin square (`from`): 64 possibilities $\to 6$ bits ($2^6 = 64$).
2. Destination square (`to`): 64 possibilities $\to 6$ bits ($2^6 = 64$).
3. Promotion piece: None, Knight, Bishop, Rook, Queen $\to 3$ bits ($2^3 = 8$).
4. Total bits required: $6 + 6 + 3 = 15$ bits.
5. Exact 16-bit word alignment leaves 1 reserved bit.

### 2.2 Exact Bit Layout

```
 15  14  13  12  11  10   9   8   7   6   5   4   3   2   1   0
┌───┬───┬───┬───┬───┬───┬───┬───┬───┬───┬───┬───┬───┬───┬───┬───┐
│ R │  PROMOTION│        TO SQUARE      │      FROM SQUARE      │
└───┴───┴───┴───┴───┴───┴───┴───┴───┴───┴───┴───┴───┴───┴───┴───┘
```

- **Bits 0–5 (6 bits):** From-Square index (`0..63`). `A1 = 0`, `H8 = 63`.
- **Bits 6–11 (6 bits):** To-Square index (`0..63`).
- **Bits 12–14 (3 bits):** Promotion Piece:
  - `000` (`0`): Normal move / Non-promotion.
  - `001` (`1`): Promote to Knight ($\text{N}$).
  - `010` (`2`): Promote to Bishop ($\text{B}$).
  - `011` (`3`): Promote to Rook ($\text{R}$).
  - `100` (`4`): Promote to Queen ($\text{Q}$).
- **Bit 15 (1 bit):** Reserved (`0`).

### 2.3 Special Move Handling Under Legal Board Replay
- **Castling:** King moves 2 squares toward rook (e.g. `e1 -> g1` or `e1 -> c1`). Legal board replay executes rook repositioning automatically.
- **En Passant:** Pawn moves diagonally to destination square (`e5 -> d6`). Captured opposing pawn on `d5` is removed by chess rules engine.
- **Disambiguation:** Pure coordinate moves `(from, to, promo)` are inherently unambiguous; standard SAN notation (e.g. `Nbd7` vs `Nfd7`) is generated deterministically upon export.

---

## 3. CODB-v3 Block Container Specification (`.cgb`, Magic `CGB3`)

To achieve high compression while preserving 100% round-trip fidelity across all supported canonical and provenance fields without silent fixed-width truncation, games are packed into immutable blocks of $N = 512$ games.

### 3.1 PGN Metadata Classification Matrix (Empirical Audit of Lichess Broadcast & Rated Archives)

Based on inspecting all 48,940 games in `masters_otb_2026-08.pgn.zst` and 121,332 games in `elite_online_2013-01.pgn.zst`:

| Category | PGN Tags | Internal Representation & Precedence Rules |
| :--- | :--- | :--- |
| **Required Canonical Metadata** | `White`, `Black`, `Result` | `metadata.white`, `metadata.black`, `metadata.result` (`1-0`, `0-1`, `1/2-1/2`, `*`). |
| **Optional Canonical Metadata (Preserved)** | `Event`, `Site`, `Date`, `UTCDate`, `Round`, `WhiteElo`, `BlackElo`, `ECO`, `TimeControl`, `FEN` | Interned in `CGB3` dictionaries without fixed-width truncation. `Date` and `UTCDate` are resolved by completeness rank (`YYYY.MM.DD` > `YYYY.MM.??` > `YYYY.??.??` > `????.??.??`); when both are complete, `Date` takes precedence. |
| **Provenance Metadata (Preserved)** | `GameURL`, `GameId`, `BroadcastURL`, `Site` (when URL) | `metadata.source_id` stores the game-specific identifier (`GameURL`, `GameId`, or game-specific `Site` URL). `metadata.broadcast_url` stores tournament/broadcast-level provenance (`BroadcastURL` or broadcast `Site` URL). Tournament URLs are never conflated with per-game `source_id`. |
| **Validated Then Discarded** | `Variant`, `SetUp` | `Variant` is verified to be `"Standard"`/`"Chess"` during ingestion; non-standard variants or non-standard `FEN` setups are rejected. |
| **Deliberately Discarded (Out of Scope)** | `UTCTime`, `BroadcastName`, `Opening`, `WhiteTitle`, `BlackTitle`, `WhiteFideId`, `BlackFideId`, `WhiteTeam`, `BlackTeam`, `WhiteCountry`, `BlackCountry`, `Board`, `Termination`, `WhiteRatingDiff`, `BlackRatingDiff`, `WhiteClock`, `BlackClock`, `Annotator`, movetext `{ [%eval] [%clk] }` | Excluded to keep game storage near ~208 B/game; `Opening` is redundant with `ECO` and position tree lookup, and clock/eval annotations are analysis artifacts rather than canonical game moves. |

### 3.2 Header & Dictionary Section
Each block begins with localized string tables (interning strings repeated across games in the block, with explicit `u16` bounds checking):
- Magic: `b"CGB3"` (4 bytes; legacy `CGB1`/`CGB2` blocks are deterministically rejected with `StorageError::InvalidMagic`)
- Game Count: `u32` (up to 512 per standard block)
- Localized String Dictionaries (`[count: u16, [byte_len: u16, utf8_bytes]]`):
  - Players Dictionary (`White` and `Black`)
  - Events Dictionary (`Event`)
  - Sites Dictionary (`Site`)
  - Rounds Dictionary (`Round`)
  - Dates Dictionary (resolved `Date` / `UTCDate`, variable length, no 10-byte truncation)
  - ECOs Dictionary (`ECO`, variable length, supports sub-ECOs like `"B90a"`, no 3-byte truncation)
  - TimeControls Dictionary (`TimeControl`)
  - SourceIds Dictionary (`GameURL` / per-game `source_id`)
  - BroadcastURLs Dictionary (`BroadcastURL`)
  - Starting FENs Dictionary (`FEN`)

### 3.3 Game Record Layout
For each game within the block (`u16::MAX` represents `None` for optional dictionary indices):
- `game_id`: `u64` (8 bytes, deterministic stable identifier)
- `white_dict_idx`: `u16` (2 bytes)
- `black_dict_idx`: `u16` (2 bytes)
- `event_dict_idx`: `u16` (2 bytes)
- `site_dict_idx`: `u16` (2 bytes)
- `round_dict_idx`: `u16` (2 bytes)
- `date_dict_idx`: `u16` (2 bytes)
- `eco_dict_idx`: `u16` (2 bytes)
- `tc_dict_idx`: `u16` (2 bytes)
- `source_id_dict_idx`: `u16` (2 bytes)
- `broadcast_url_dict_idx`: `u16` (2 bytes)
- `fen_start_dict_idx`: `u16` (2 bytes)
- `white_elo`: `u16` (2 bytes, 0 = unrated)
- `black_elo`: `u16` (2 bytes, 0 = unrated)
- `result`: `u8` (1 byte: 1=White, 2=Draw, 3=Black, 0=Star)
- `move_count`: `u16` (2 bytes)
- `moves`: `[CompactMove; move_count]` (2 bytes per ply)

---

## 4. Measured Empirical Size & Round-Trip Validation

Measurements on physical reference datasets (`masters_20k`, $N = 20{,}000$ accepted games) using the release build:

| Format Representation | Total Size for 20,000 Games | Bytes / Game | Random Access Latency | Supported-Field Fidelity |
| :--- | :--- | :--- | :--- | :--- |
| **Exported Canonical PGN (incl. `GameURL`/`BroadcastURL`)** | 21.49 MB | 1,074.7 bytes | Sequential scan only | 100% |
| **Monolithic PGN.zst (Level 3)** | 3.65 MB | 182.5 bytes | None (Full decompression) | 100% |
| **Uncompressed Binary Packed** | 7.92 MB | 396.0 bytes | Direct offset $O(1)$ | 100% |
| **CODB-v3 (`CGB3`) Block-Zstd (Level 3)** | **4.16 MB** (+1.18 KB dir) | **208.2 bytes** | **Sub-ms ($< 0.5$ ms block)** | **100% across all supported fields** |

### 4.1 Storage Growth Projections for Full Game Retrieval
At **208.2 bytes / game** (measured at $N = 20{,}000$):
- **1,000,000 games:** **~208 MB**
- **5,000,000 games:** **~1.04 GB**
- **10,000,000 games:** **~2.08 GB**
- **15,000,000 games (Combined 5M Masters + 10M Elite):** **~3.12 GB** core blocks (+ **~0.18 GB** estimated global `game_id` lookup index)

> [!IMPORTANT]
> Full game retrieval with complete supported-field retention for 15 million games consumes **~3.12 GB** of `.cgb` blocks (~3.30 GB including the global `game_id` lookup table), leaving **~6.7 GB of the 10 GB budget for the position Opening Explorer index and deployment reserves**.
