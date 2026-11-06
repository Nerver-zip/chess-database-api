---
summary: "Canonical data contracts for binary formats (CGB3, CPI), domain models, CLI JSON, and future API."
read_when: "Implementing or integrating storage formats, position index structures, domain models, or API payloads."
status: current
---

# Data Contracts Specification

## 1. Domain Models (`codb-core`)

All core domain models are defined in [`crates/core/src/models.rs`](../crates/core/src/models.rs).

### 1.1 `Collection`
Represents the strictly isolated database collections:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Collection {
    #[serde(rename = "masters-otb")]
    MastersOtb,
    #[serde(rename = "elite-online")]
    EliteOnline,
}
```
- `masters-otb`: Verified over-the-board master tournaments (classical/rapid/blitz), preserving FIDE Elo.
- `elite-online`: Verified top-tier online games (2200+ rating, bullet excluded), preserving Glicko-2 ratings.
- **Contract Rule:** Ratings and game statistics from these collections are never combined or conflated.

### 1.2 `CompactMove` (16-bit Packed Move Representation)
Moves are bit-packed into a single `u16` without ambiguous SAN notation:
- **Bits 0..5 (6 bits):** Source square index ($0 \dots 63$).
- **Bits 6..11 (6 bits):** Destination square index ($0 \dots 63$).
- **Bits 12..15 (4 bits):** Promotion piece flag:
  - `0`: None / Standard Move
  - `1`: Knight (`N`)
  - `2`: Bishop (`B`)
  - `3`: Rook (`R`)
  - `4`: Queen (`Q`)

Replaying against an orthodox legal board state resolves captures, castling rights, and en passant captures deterministically with zero information loss.

### 1.3 `PositionKey` (128-bit Canonical Position Hash)
Represents a unique legal chess position:
- Encodes piece placement across all 64 squares, side to move, castling rights, and active en passant target square.
- Hexadecimal representation: 32 lowercase hex characters (`u128` serialized big-endian).

### 1.4 `GameResult`
Canonical outcome enum:
- `WhiteWins` (`"1-0"`)
- `Draw` (`"1/2-1/2"`)
- `BlackWins` (`"0-1"`)
- `Star` (`"*"`, unknown or unfinished outcome; verified for legal moves but excluded from W/D/L counters).

### 1.5 `StableGameId`
A deterministic 64-bit unsigned integer (`u64`) calculated using 64-bit FNV-1a over the canonical metadata key and normalized move sequence. Identical games across different source archives produce the exact same ID.

---

## 2. Persistent Storage Container (`CGB3` Container & Sharding)

Defined in [`crates/storage/src/format.rs`](../crates/storage/src/format.rs) and [`crates/storage/src/shard.rs`](../crates/storage/src/shard.rs).

### 2.1 Shard Container Header (`shard_XXXX.cgb3`)
Every shard begins with a fixed 32-byte header:

| Offset | Length | Type | Description |
| :--- | :--- | :--- | :--- |
| `0..4` | 4 bytes | ASCII | Magic identifier: `b"CGCS"` (`0x43474353`) |
| `4..8` | 4 bytes | `u32` (BE) | Shard format version (currently `1`) |
| `8..10` | 2 bytes | `u16` (BE) | Shard index / ID |
| `10..11`| 1 byte  | `u8` | Collection ID (`1` = `MastersOtb`, `2` = `EliteOnline`) |
| `11..32`| 21 bytes| Zero-fill | Reserved padding |

Followed by sequential Zstandard-compressed CGB3 game blocks, and closed by a 20-byte footer:
- **Magic Footer (4 bytes):** `b"CGCE"` (`0x43474345`)
- **Block Count (4 bytes):** `u32` (BE)
- **Total Shard Games (8 bytes):** `u64` (BE)
- **Footer Checksum (4 bytes):** `u32` (BE)

### 2.2 CGB3 Block Format (`GameBlock`)
Each block holds $N = 512$ games and is compressed with Zstandard:
1. **Magic Header (4 bytes):** `b"CGB3"`
2. **Game Count (2 bytes):** `u16` (BE)
3. **Interned String Dictionaries:**
   - Players dictionary (UTF-8, length-prefixed)
   - Events dictionary
   - Sites dictionary
   - Rounds dictionary
   - Dates dictionary (`Date` and `UTCDate`)
   - ECO codes dictionary
   - Time control dictionary
   - Game-level source URL dictionary
   - Starting FEN dictionary (orthodox custom FENs)
4. **Game Metadata Records:** Array of metadata structs indexing dictionary offsets.
5. **Move Sequences:** Bit-packed `CompactMove` sequences preceded by ply lengths.

Average physical density: **~208 bytes/game** for Masters OTB; **~178–181 bytes/game** for Elite Online.

### 2.3 Global Game Lookup (`games.idx`)
A fixed-record, memory-mapped binary search index locating any game across shards by stable ID in sub-millisecond time:
- **Record Size:** Exactly 16 bytes:
  - `game_id`: `u64` (BE)
  - `shard_id`: `u16` (BE)
  - `block_index`: `u32` (BE)
  - `game_index_in_block`: `u16` (BE)
- **Ordering:** Strictly sorted in ascending order of `game_id`.

---

## 3. Production Compact Position Index (`CPI v2`)

Defined in [`crates/index/src/position.rs`](../crates/index/src/position.rs) and [`crates/index/src/external.rs`](../crates/index/src/external.rs).

### 3.1 Header Layout (`positions.cpi`)
Fixed 64-byte binary header:

| Offset | Length | Type | Field Name | Description |
| :--- | :--- | :--- | :--- | :--- |
| `0..8` | 8 bytes | ASCII | `magic` | `b"CODB_CPI"` |
| `8..12` | 4 bytes | `u32` (BE) | `version` | CPI version (currently `2`) |
| `12..16`| 4 bytes | `u32` (BE) | `positions_per_block`| Target positions per compressed block (default 256) |
| `16..24`| 8 bytes | `u64` (BE) | `total_positions` | Total unique canonical positions in index |
| `24..32`| 8 bytes | `u64` (BE) | `total_move_pairs`| Total distinct `(position, move)` candidate pairs |
| `32..36`| 4 bytes | `u32` (BE) | `block_count` | Total compressed blocks written |
| `36..40`| 4 bytes | `u32` (BE) | `reserved` | Reserved padding (0) |
| `40..48`| 8 bytes | `u64` (BE) | `sparse_index_offset`| Byte offset to sparse index table |
| `48..56`| 8 bytes | `u64` (BE) | `sparse_index_length`| Byte length of sparse index table |
| `56..64`| 8 bytes | `u64` (BE) | `flags` | Reserved bitflags (0) |

### 3.2 Compressed Position Blocks
Each block contains up to `positions_per_block` unique positions, compressed with Zstandard:
- **Header:** `num_positions: u16` (BE).
- **Position Entries:**
  - `position_key: [u8; 16]` (128-bit key)
  - `move_count: u8`
  - Array of Candidate Moves (14 bytes per move):
    - `compact_move: u16` (BE)
    - `white_wins: u32` (BE)
    - `draws: u32` (BE)
    - `black_wins: u32` (BE)

### 3.3 Sparse Memory-Mapped Index
Stored at `sparse_index_offset` at the end of `positions.cpi`:
- **Record Size:** Exactly 34 bytes:
  - `first_key: u128` (BE) — lowest key in the referenced block
  - `file_offset: u64` (BE) — byte offset to block start
  - `compressed_len: u32` (BE)
  - `uncompressed_len: u32` (BE)
  - `num_positions: u16` (BE)
- **Lookup Guarantee:** Binary search over the mmap sparse index resolves the block offset in $O(\log B)$ with zero heap allocation.

---

## 4. Dataset Manifest Contract (`manifest.json`)

Stored in the root of every dataset snapshot (`versions/vX/manifest.json`):

```json
{
  "manifest_version": 1,
  "collection": "masters-otb",
  "created_at_utc": "2026-11-04T12:00:00Z",
  "dataset_id": "masters-otb-20k",
  "total_canonical_games": 20000,
  "total_shards": 1,
  "shards": [
    {
      "shard_id": 0,
      "filename": "shard_0000.cgb3",
      "game_count": 20000,
      "file_size_bytes": 4164000,
      "sha256": "abcdef..."
    }
  ],
  "lookup_index": {
    "filename": "games.idx",
    "record_count": 20000,
    "file_size_bytes": 320000,
    "sha256": "123456..."
  },
  "position_index": {
    "filename": "positions.cpi",
    "version": 2,
    "depth_plies": 40,
    "unique_positions": 493211,
    "file_size_bytes": 12850400,
    "sha256": "7890ab..."
  }
}
```

---

## 5. CLI JSON Contracts

### 5.1 `codb query --json`
```json
{
  "position_key": "463b96181691fc9c0000000000000000",
  "total_games": 1420,
  "white_wins": 568,
  "draws": 596,
  "black_wins": 256,
  "moves": [
    {
      "compact_move": 1254,
      "uci": "e2e4",
      "white_wins": 340,
      "draws": 350,
      "black_wins": 160,
      "total_games": 850
    }
  ]
}
```

### 5.2 `codb inspect --json`
```json
{
  "dataset_version": "v1",
  "collection": "masters-otb",
  "total_canonical_games": 20000,
  "total_shards": 1,
  "shards": [
    {
      "shard_id": 0,
      "game_count": 20000,
      "compressed_size_bytes": 4164000,
      "bytes_per_game": 208.2
    }
  ],
  "cpi_index": {
    "version": 2,
    "unique_positions": 493211,
    "total_move_pairs": 1120400,
    "compressed_bytes": 12850400,
    "bytes_per_position": 26.05
  }
}
```

---

## 6. Future HTTP Opening Explorer API Contract

Promoted from initial research specifications for the future standalone service.

### 6.1 `GET /v1/explorer/position`

#### Query Parameters
- `fen` (string, optional): Orthodox legal FEN string.
- `moves` (string, optional): Comma- or space-separated SAN or UCI move sequence from starting position.
- `collection` (enum, default: `masters-otb`): `masters-otb` or `elite-online`.

#### Response Payload (`application/json`)
```json
{
  "collection": "masters-otb",
  "position_key": "463b96181691fc9c0000000000000000",
  "total_games": 24510,
  "white_wins": 9804,
  "draws": 10294,
  "black_wins": 4412,
  "white_win_pct": 40.0,
  "draw_pct": 42.0,
  "black_win_pct": 18.0,
  "moves": [
    {
      "uci": "f1b5",
      "san": "Bb5",
      "white_wins": 5420,
      "draws": 5610,
      "black_wins": 2170,
      "total_games": 13200
    }
  ]
}
```

### 6.2 `GET /v1/explorer/game/{id}`
Returns the full canonical game by 64-bit ID in sub-millisecond retrieval time:
```json
{
  "id": 1048220,
  "collection": "masters-otb",
  "white": "Carlsen, Magnus",
  "black": "Caruana, Fabiano",
  "white_elo": 2835,
  "black_elo": 2832,
  "date": "2018.11.09",
  "event": "World Championship Match",
  "site": "London ENG",
  "round": "1",
  "result": "1/2-1/2",
  "eco": "B31",
  "moves_san": ["e4", "c5", "Nf3", "Nc6", "Bb5", "g6"]
}
```
