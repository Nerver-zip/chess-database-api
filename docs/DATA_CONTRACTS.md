---
summary: "Canonical data contracts for binary formats (CGB3, CPI), domain models, CLI JSON, and Opening Explorer HTTP API."
read_when: "Implementing or integrating storage formats, position index structures, domain models, or HTTP API endpoints."
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

## 6. Opening Explorer HTTP API Contract

The Opening Explorer HTTP API (`codb-api`) exposes read-only access to persistent CGB3 game shards and CPI v2 position indexes.

### 6.1 `GET /v1/explorer/position`
Retrieves aggregated opening statistics and candidate legal follow-up moves for a target chess position.

#### Query Parameters
- `fen` (string, optional): Orthodox legal FEN string.
- `moves` (string, optional): Comma- or whitespace-separated SAN or UCI move sequence from the standard initial chess position (e.g. `"e4,c5,Nf3"` or `"e2e4 c7c5 g1f3"`).
- `collection` (string, optional, default: `"masters-otb"`): Target collection identifier (`"masters-otb"` or `"elite-online"`).
- `since_year` / `min_rating` (unsupported): If provided, the request fails with HTTP 422 (`unsupported_parameter`) to prevent silent semantic misinterpretation.

#### Validation Rules
1. `fen` and `moves` are mutually exclusive. Providing both returns HTTP 400 (`ambiguous_position`).
2. If neither `fen` nor `moves` is provided, the API returns HTTP 400 (`missing_position`).
3. FEN inputs must be orthodox and legal (fails with HTTP 400 `invalid_fen`).
4. Moves must be legal sequentially from the initial board state (fails with HTTP 400 `invalid_moves`).
5. Move sequences exceeding `--max-move-plies` (default 200 plies) return HTTP 400 (`move_sequence_too_long`).
6. Unsupported filter parameters (`since_year`, `min_rating`) return HTTP 400 (`unsupported_parameter`).

#### Response Payload (`application/json`, HTTP 200)
When the position is present in the CPI index:
```json
{
  "collection": "masters-otb",
  "dataset_version": "v1_build_20261106",
  "dataset_freshness": "2026-11-06T12:00:00Z",
  "position_key": "463b96181691fc9c0000000000000000",
  "fen": "r1bqkbnr/pppp1ppp/2n5/1B2p3/4P3/5N2/PPPP1PPP/RNBQK2R b KQkq - 3 3",
  "total_games": 24510,
  "white_wins": 9804,
  "draws": 10294,
  "black_wins": 4412,
  "white_win_pct": 40.0,
  "draw_pct": 42.0,
  "black_win_pct": 18.0,
  "moves": [
    {
      "uci": "a7a6",
      "san": "a6",
      "white_wins": 5420,
      "draws": 5610,
      "black_wins": 2170,
      "total_games": 13200,
      "white_win_pct": 41.1,
      "draw_pct": 42.5,
      "black_win_pct": 16.4
    }
  ]
}
```

When the position is legal but does not exist in the indexed database:
The API returns HTTP 200 with zeroed game counters and an empty candidate move array:
```json
{
  "collection": "masters-otb",
  "position_key": "c0e6488a0e28f7310000000000000000",
  "fen": "rnbqkbnr/pppppppp/8/8/8/7N/PPPPPPPP/RNBQKB1R b KQkq - 1 1",
  "total_games": 0,
  "white_wins": 0,
  "draws": 0,
  "black_wins": 0,
  "white_win_pct": 0.0,
  "draw_pct": 0.0,
  "black_win_pct": 0.0,
  "moves": []
}
```

### 6.2 `GET /v1/collections/{collection}/games/{id}`
Retrieves the full reconstructed game in standard PGN format by collection and 64-bit stable game ID.

#### Path Parameters
- `collection` (string): `"masters-otb"` or `"elite-online"`.
- `id` (integer): 64-bit unsigned integer stable game ID (`u64`).

#### Response Headers & Body
- **Content-Type:** `application/x-chess-pgn; charset=utf-8`
- **Status:** HTTP 200 OK
- **Body:** Canonical PGN text:
```pgn
[Event "FIDE World Championship 2018"]
[Site "London ENG"]
[Date "2018.11.09"]
[Round "1"]
[White "Caruana, Fabiano"]
[Black "Carlsen, Magnus"]
[Result "1/2-1/2"]
[WhiteElo "2832"]
[BlackElo "2835"]
[ECO "B31"]

1. e4 c5 2. Nf3 Nc6 3. Bb5 g6 4. Bxc6 dxc6 1/2-1/2
```

If the collection is valid but the game ID does not exist in the index, returns HTTP 404 (`game_not_found`).

### 6.3 `GET /v1/collections`
Returns summary status and metadata for all datasets loaded in the active server snapshot.

#### Response Payload (`application/json`, HTTP 200)
```json
{
  "collections": [
    {
      "collection": "masters-otb",
      "version": "v1_build_20261106",
      "snapshot_path": "/var/data/masters-otb/versions/v1",
      "total_games": 100000,
      "cpi_indexed": true,
      "cpi_depth_plies": 40
    }
  ]
}
```

### 6.4 Operational Probes & Administrative Reload

#### `GET /health`
Liveness probe. Returns HTTP 200 `{"status": "ok"}` when the HTTP listener is operational.

#### `GET /ready`
Readiness probe. Returns HTTP 200 `{"status": "ready", "collections": [...]}` when at least one collection dataset is successfully loaded and ready for queries. If no datasets are loaded, returns HTTP 503 Service Unavailable.

#### `POST /v1/admin/reload`
Triggers an authenticated, fail-closed zero-downtime reload of active dataset snapshots from disk.

- **Access Control:** Requires administrative authorization via `Authorization: Bearer <token>` or `X-Admin-Token: <token>` matching `--admin-token` or `CODB_ADMIN_TOKEN`. If no admin token is configured, the endpoint is disabled and returns `HTTP 403 Forbidden` (`admin_disabled`). If the token is missing or invalid, returns `HTTP 401 Unauthorized` (`unauthorized`).
- **Cross-Origin Protection:** Cross-origin triggers (requests containing an `Origin` header) are strictly rejected with `HTTP 401 Unauthorized` to prevent cross-site request forgery.
- **Fail-Closed Validation:** Candidate snapshots are deeply verified before activation (`verify_dataset` integrity check + CPI index verification). If verification fails, the replacement is aborted, returning `HTTP 500 Internal Server Error` (`reload_failed`), and existing active snapshots remain untouched and operational.
- **Concurrent Request Coordination:** Reload operations are serialized using an asynchronous mutex to prevent overlapping reload thrashing.
- **Response Payload (`application/json`, HTTP 200):**
```json
{
  "status": "reloaded",
  "reloaded_collections": ["masters-otb", "elite-online"],
  "duration_ms": 3.29
}
```

#### Native Unix Signal Reload (`SIGHUP`)
For local deployments without exposing HTTP administrative credentials, sending `SIGHUP` (`kill -HUP <pid>`) triggers the same atomic verification and reload process asynchronously without requiring network credentials.

### 6.5 Error Response Envelope
All error responses adhere to a consistent JSON error schema:
```json
{
  "error": {
    "code": "invalid_moves",
    "message": "Illegal move 'e5' at ply 1 for current position"
  }
}
```

#### Standard Error Codes
| Code | HTTP Status | Description |
| :--- | :--- | :--- |
| `invalid_collection` | 400 Bad Request | Collection parameter was not `masters-otb` or `elite-online`. |
| `missing_position` | 400 Bad Request | Neither `fen` nor `moves` parameter was supplied. |
| `ambiguous_position` | 400 Bad Request | Mutually exclusive parameters (`fen` and `moves`) were both supplied. |
| `invalid_fen` | 400 Bad Request | FEN string could not be parsed or violates chess rules. |
| `invalid_moves` | 400 Bad Request | Move sequence contains illegal moves or invalid notation. |
| `move_sequence_too_long`| 400 Bad Request | Move sequence exceeds configured maximum plies. |
| `unsupported_parameter`| 400 Bad Request | Query requested unsupported filter parameter (`since_year` or `min_rating`). |
| `invalid_game_id` | 400 Bad Request | Game ID parameter was not a valid 64-bit unsigned integer. |
| `unauthorized` | 401 Unauthorized | Missing or invalid admin authorization token, or rejected cross-origin admin request. |
| `admin_disabled` | 403 Forbidden | Administrative reload endpoint is disabled (no `--admin-token` configured). |
| `game_not_found` | 404 Not Found | 64-bit game ID does not exist in the collection index. |
| `collection_unavailable`| 503 Service Unavailable | Target collection was not mounted at server startup. |
| `index_unavailable` | 503 Service Unavailable | Snapshot exists but CPI v2 position index is missing. |
| `pool_timeout` | 503 Service Unavailable | Bounded reader pool acquisition timed out under extreme concurrency. |
| `reload_failed` | 500 Internal Server Error | Candidate dataset snapshot failed pre-activation verification. |
| `storage_error` | 500 Internal Server Error | Storage engine I/O or decoding failure. |
| `internal_error` | 500 Internal Server Error | Unexpected internal server or worker thread failure. |

*Note on Error Sanitization:* All HTTP 500 responses (`reload_failed`, `storage_error`, `internal_error`) return sanitized generic messages to external clients, ensuring filesystem paths and engine internals are never leaked. Full diagnostic details are preserved in structured server-side logs.

---

## 6.6 HTTP Caching & Conditional Requests

The API implements RFC 7232 HTTP conditional validation:
- **Existence Verification Prior to Evaluation:** Conforming strictly to RFC 7232 Section 3.2, resource existence is verified before conditional headers are evaluated. If a game ID does not exist in `games.idx`, the server immediately returns `HTTP 404 Not Found` rather than evaluating `If-None-Match` or returning `HTTP 304 Not Modified`.
- **`ETag` Generation:** Derived deterministically from dataset version build ID, collection, and request representation:
  - Position queries: `"{collection}-{dataset_build_id}-{position_key_hex}-{fen_hash}"` (incorporates the full FEN string to differentiate positions with varying halfmove/fullmove clocks)
  - Game queries: `"{collection}-{dataset_build_id}-{game_id}"`
- **Conditional Requests (`If-None-Match`):** Evaluates strong ETags, weak ETags (`W/"..."`), wildcards (`*`), and comma-separated lists. If any tag matches the current resource ETag, returns `HTTP 304 Not Modified` with an empty body.
- **Cache-Control Headers:**
  - Position and Collections endpoints: `Cache-Control: public, max-age=60`
  - Game retrieval endpoint: `Cache-Control: public, max-age=3600`
- **CORS Headers:** Restrictive default: empty (`allowed_origins = []`), allowing same-origin requests only. Configurable via `--allowed-origins <origin>`. When configured, `ETag`, `Content-Type`, and `Cache-Control` are exposed via `Access-Control-Expose-Headers`.
