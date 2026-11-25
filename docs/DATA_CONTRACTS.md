---
summary: "Canonical data contracts for binary formats (CGB3, CPI), domain models, CLI JSON, and Opening Explorer HTTP API."
read_when: "Implementing or integrating storage formats, position index structures, domain models, or HTTP API endpoints."
status: current
---

# Data Contracts Specification

## 1. Domain Models (`opening-explorer-core`)

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

The current manifest also records `dataset_build_id`, `build_timestamp`, `format_version`, `schema_version`, `source_archives`, `curation_policy`, `deduplication_policy_version`, `accounting`, `total_games`, `total_physical_bytes`, and `is_complete`. `position_index` and `source_attribution` are optional fields with backward-compatible defaults. In `source_archives`, `checksum_verified` means the file matched the recorded or locally computed SHA-256; it does not, by itself, identify the hash as publisher-authenticated.

Gigabase-qualified builds may include this optional registration:

```json
{
  "source_attribution": {
    "filename": "source-attribution.jsonl",
    "file_size_bytes": 12345,
    "sha256": "<64 lowercase hexadecimal characters>",
    "entry_count": 1
  }
}
```

The referenced UTF-8 JSONL file contains one line per retained game, in ascending `game_id` order. Each line has `schema_version: 1`, `game_id`, and a non-empty `source_attributions` array of records containing the captured `Source`, `SourceQuality`, FIDE IDs, identity-evidence tags, and `Mode`. Deep verification checks the sidecar's regular-file type, size, hash, schema, sorted IDs, and one-to-one correspondence with `games.idx`. CPI publication preserves the same sidecar checksum. The sidecar is outside CGB3 and does not change any persisted binary layout.

---

The optional `search_index` entry describes a derived metadata catalog and positional occurrence index. Its artifact sizes contribute to `total_physical_bytes` on enriched snapshots. The [search index format and verification rules](SEARCH_INDEX.md) define coverage, identity binding and compatibility. Existing manifests without this entry continue to load.

## 5. CLI JSON Contracts

### 5.1 `opening-explorer query --json`
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

### 5.1.1 `opening-explorer build --report-json <path>` and `opening-explorer report`

`opening-explorer build --report-json` writes the build accounting after finalization:
```json
{
  "collection": "masters-otb",
  "dataset_build_id": "codb-masters-otb-...",
  "total_games": 42612,
  "accounting": { "total_encountered": 0, "accepted_unique": 0, "confirmed_duplicates": 0, "superseded_prior": 0, "rejected_by_policy": 0, "rejected_variant": 0, "rejected_illegal": 0, "rejected_malformed": 0, "quarantined": 0, "completion_status": "CompleteEof" },
  "is_reconciled": true,
  "retained_matches_accepted": true,
  "policy_rejection_reasons": { "below_rating_threshold": 0, "missing_rating": 0 },
  "quarantine_reasons": { "stable_game_id_collision": 0 },
  "total_physical_bytes": 0,
  "build_duration_secs": 0.0,
  "source_archives": []
}
```
`policy_rejection_reasons` sums to `accounting.rejected_by_policy`. Category keys are defined in [CORPUS_AND_DEDUPLICATION.md](CORPUS_AND_DEDUPLICATION.md#31-machine-readable-rejection-categories).

`quarantine_reasons` breaks down `accounting.quarantined`. The opt-in Gigabase source policy records stable game ID conflicts as `stable_game_id_collision`; other collections retain their existing fatal collision behavior.

`opening-explorer report --dataset <dir> [--output <json>]` iterates every stored game and emits `results`, `unknown_result_games`, `years`, `months`, `min_player_rating_bins` (100-point bins of the lower rating; FIDE Elo for `masters-otb`, Glicko-2 for `elite-online`, never pooled), `plies_bins`, `speed_classes` (`elite-online` only), `time_control_raw_top20` (`masters-otb` only), `distinct_players_hash_estimate`, `distinct_events_hash_estimate`, and `missing_metadata` counts. Neither command changes any persisted binary format.

`opening-explorer bench-lookup --dataset <dir> [--games N] [--output <json>]` replays stored games to sample positions and reports in-process `DiskPositionIndex::query` latency (`hit_first_pass`, `hit_warm_pass`, `miss_warm_pass`: `count`, `hits`, `p50_us`, `p95_us`, `p99_us`, `max_us`). It reads only and does not modify the dataset.

### 5.2 `opening-explorer inspect --json`
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

## 6. Opening Explorer HTTP API Contract

The HTTP contract (routes, parameters, response fields, error envelope, caching, CORS, and configuration) is specified in [API_REFERENCE.md](API_REFERENCE.md). That document is normative; it is not repeated here so the two cannot diverge. Behavior is exercised by the API crate's integration tests and by `scripts/test_docs_examples.py` against recorded responses in `fixtures/api/`.
