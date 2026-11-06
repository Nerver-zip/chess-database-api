---
summary: "Original exploration of future HTTP Opening Explorer API contracts and client compatibility."
read_when: "Reviewing the initial research draft for the planned Opening Explorer HTTP API."
status: planned
---

# Research Track: Future API Contract & Evalis Compatibility

> [!NOTE]
> This research document is preserved for reference. Authoritative API data contracts are consolidated in [docs/DATA_CONTRACTS.md](../DATA_CONTRACTS.md#6-future-http-opening-explorer-api-contract).

## 1. Architectural Boundaries & Decoupling

The relationship between **`chess-openings-db`** and **Evalis** follows strict architectural decoupling:

```
┌────────────────────────────────────────────────────────┐
│ Evalis Client Application (Desktop / Web / Tauri)      │
│ • Local chess board GUI & navigation                   │
│ • Local engine analysis (Stockfish WASM / Native)      │
│ • User's local private games & review notes            │
│ • 100% functional in OFFLINE mode                      │
└───────────────────────────┬────────────────────────────┘
                            │ (Optional async HTTP queries)
┌───────────────────────────▼────────────────────────────┐
│ CODB Opening Explorer API (Self-Hosted or Static CDN)  │
│ • Read-only queries by canonical position              │
│ • Isolated `masters-otb` and `elite-online` collections│
│ • Random-access game retrieval by stable 64-bit ID     │
└────────────────────────────────────────────────────────┘
```

> [!IMPORTANT]
> **Offline Independence:** Evalis must **never** hang, disable local board review, or require an internet connection if the Opening Explorer service is unavailable. The Explorer API is strictly an optional analytical enhancement.

---

## 2. API Contract Specification (Draft v1)

### 2.1 Endpoint 1: Position Statistics Query
`GET /v1/explorer/position`

#### Request Parameters
| Parameter | Type | Required | Description | Example |
| :--- | :--- | :--- | :--- | :--- |
| `fen` | string | Either `fen` or `moves` | Standard FEN string of the position | `r1bqkb1r/pppp1ppp/2n2n2/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 4 4` |
| `moves` | string | Either `fen` or `moves` | UCI move sequence from starting position | `e2e4,e7e5,g1f3,b8c6` |
| `collection` | enum | No (default: `masters-otb`) | Collection filter: `masters-otb` or `elite-online` | `masters-otb` |
| `since_year` | integer | No | Filter games played on or after year | `2000` |
| `min_rating` | integer | No | Filter games where both players meet rating | `2400` |

#### Response Schema (`application/json`)
```json
{
  "dataset_version": "2026.10-v1",
  "dataset_freshness": "2026-10-23T00:00:00Z",
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
      "total_games": 13200,
      "performance_rating": 2685,
      "representative_game_id": 1048220
    },
    {
      "uci": "f1c4",
      "san": "Bc4",
      "white_wins": 2840,
      "draws": 2980,
      "black_wins": 1380,
      "total_games": 7200,
      "performance_rating": 2650,
      "representative_game_id": 984121
    },
    {
      "uci": "d2d4",
      "san": "d4",
      "white_wins": 1544,
      "draws": 1704,
      "black_wins": 862,
      "total_games": 4110,
      "performance_rating": 2642,
      "representative_game_id": 110294
    }
  ],
  "top_games": [
    {
      "id": 1048220,
      "white": "Carlsen, Magnus",
      "black": "Caruana, Fabiano",
      "white_elo": 2835,
      "black_elo": 2832,
      "date": "2018.11.09",
      "event": "World Championship Match",
      "result": "1/2-1/2"
    }
  ]
}
```

---

### 2.2 Endpoint 2: Full Game Retrieval
`GET /v1/games/{id}`

#### Request Parameters
- `id` (path, u64): Stable 64-bit Game Identifier.
- `format` (query, optional): `json` (default) or `pgn`.

#### Response Schema (PGN Format)
```http
HTTP/1.1 200 OK
Content-Type: application/x-chess-pgn
Cache-Control: public, max-age=31536000, immutable
ETag: "game-1048220-v1"

[Event "World Championship Match"]
[Site "London ENG"]
[Date "2018.11.09"]
[Round "1"]
[White "Caruana, Fabiano"]
[Black "Carlsen, Magnus"]
[Result "1/2-1/2"]
[WhiteElo "2832"]
[BlackElo "2835"]
[ECO "B31"]

1. e4 c5 2. Nf3 Nc6 3. Bb5 g6 4. Bxc6 dxc6 5. d3 Bg7 6. h3 Nf6 7. Nc3 Nd7 1/2-1/2
```

---

### 2.3 CLI & Native Rust Query Contract (`codb query`)

For local-first integrations, scripting, and offline verification, the CLI directly implements the Opening Explorer query engine:

#### CLI Syntax
```bash
# Query by move sequence (SAN or UCI, comma or whitespace separated)
codb query --dataset <PATH> --moves "e4,e5,Nf3" [--json]

# Query by orthodox legal FEN
codb query --dataset <PATH> --fen "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6 0 2" [--json]

# Legacy standalone index query (backward compatible)
codb query --index <PATH_TO_CPI> --moves "e4" [--json]
```

#### JSON Output Structure
```json
{
  "position_key": "463b96181691fc9c0000000000000000",
  "fen": "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6 0 2",
  "total_games": 24510,
  "white_wins": 9804,
  "draws": 10294,
  "black_wins": 4412,
  "moves": [
    {
      "uci": "g1f3",
      "san": "Nf3",
      "white_wins": 5420,
      "draws": 5610,
      "black_wins": 2170,
      "total": 13200,
      "white_win_pct": 41.1,
      "draw_pct": 42.5,
      "black_win_pct": 16.4
    }
  ]
}
```

---

## 3. Operational Deployment Alternatives

| Operational Model | Infrastructure Requirements | Hosting Cost | Cold Start / Latency | Offline Feasibility in Evalis |
| :--- | :--- | :--- | :--- | :--- |
| **Model A: Lightweight Self-Hosted HTTP Daemon** | Single VPS (1 vCPU, 512MB RAM, 10GB SSD) running `codb serve` in Rust | ~$4 - $6 / month | < 1 ms latency; persistent memory-mapped index | Can be embedded locally via SQLite/CPI reader |
| **Model B: Static Sharded CDN (Serverless / S3)** | Static object store partitioned by 16-bit prefix (`/explorer/{col}/{prefix}/{pos_key}.json`) | Pennies (Storage only) | Dependent on CDN cache hit; 20-50ms HTTP | Highly portable, zero server maintenance |
| **Model C: Hybrid Architecture (Recommended)** | Cloudflare Workers / Fastly Edge Cache in front of lightweight Origin Server | Free tier to minimal cost | Worldwide edge cache (< 15 ms); dynamic game streaming | Combines static CDN cacheability with compact binary queries |

### Web & Browser Considerations
- **CORS:** All query endpoints must respond with `Access-Control-Allow-Origin: *` to enable direct in-browser inspection from Web-based Evalis or third-party analysis sites.
- **Cache-Control:**
  - Game endpoints: `public, max-age=31536000, immutable` (games are immutable once published).
  - Position queries: `public, max-age=604800, stale-while-revalidate=86400` (keyed by dataset version tag).
- **Request Size:** Query requests take $< 100\text{ bytes}$ (FEN or compact move list).
- **Response Size:** Explorer JSON responses take $\sim 1.2\text{ KB}$ uncompressed, or $< 350\text{ bytes}$ with Brotli/gzip compression.
