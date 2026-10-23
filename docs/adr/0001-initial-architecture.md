# ADR-0001: Initial Architecture, Storage, and Indexing Foundations

- **Status:** Under Validation / Proposed for PR-02
- **Date:** 2026-10-23
- **Deciders:** Core Engineering Team
- **Technical Context:** Feasibility Milestone PR-01

---

## 1. Context and Problem Statement

The goal of the **Chess Openings DB** project is to curate, deduplicate, compress, and index high-quality chess games to power a position-centric Opening Explorer through a lightweight, reusable API.

The central feasibility constraint investigated is:
> **Can we build a reliable, reproducible, publicly redistributable chess database containing millions of high-quality games, with full game retrieval and position-based opening statistics, within approximately 10 GB of deployed storage?**

---

## 2. Decision Summary

We propose the following architectural foundations based on our empirical feasibility measurements:

### 2.1 Workspace and Technology Stack
- **Language:** Rust (2021 edition), leveraging memory safety, zero-cost abstractions, and fearless concurrency.
- **Modularity:** A Cargo workspace separating domain models (`core`), streaming parser (`ingest`), deduplication heuristics (`curate`), binary game blocks (`storage`), opening tree indexes (`index`), and CLI/benchmarking (`cli`).
- **Legal License:** Code is dual-licensed under **MIT OR Apache-2.0**. No code is imported or derived from AGPL-3.0 sources (`lila-openingexplorer`).

### 2.2 Dual Collection Isolation
The engine strictly segregates two collections:
1. **Masters OTB:** Tournament games between titled/rated masters sourced from official broadcasts (CC BY-SA 4.0) and public domain historical tournaments.
2. **Elite Online:** High-rated online games from Lichess standard rated archives (CC0 1.0 Universal).

> [!IMPORTANT]
> FIDE Elo ratings and online platform Glicko-2 ratings are never merged or conflated.

### 2.3 Storage Representation: CODB-v2 Block-Zstd Container (`.cgb`)
- **16-bit Move Encoding:** All legal chess moves are packed into a 16-bit word (`from`: 6 bits, `to`: 6 bits, `promo`: 3 bits, reserved: 1 bit). Replaying against legal board state resolves castling, en passant, and captures without ambiguity.
- **Dictionary Header Compression:** Players, events, sites, rounds, time controls, and source IDs are dictionary-encoded in blocks of $N = 512$ games.
- **Full Round-Trip Fidelity:** Achieves **~202–206 bytes per game** (a 4.2× compression ratio over raw ASCII PGN) while retaining 100% metadata fidelity and enabling sub-millisecond $O(1)$ random-access game retrieval via a lightweight offset index (`.cgi`).

### 2.4 Position Identity: 128-bit Collision-Resistant Keys
- 64-bit Zobrist hashing is vulnerable to the Birthday Paradox at scale ($p \approx 10^{-5}$ at 20M positions).
- We standardize on a **128-bit key (`Zobrist128`)** using `shakmaty::zobrist::Zobrist128` with `EnPassantMode::Legal`. This guarantees zero hash collisions even across hundreds of millions of positions.
- Halfmove clock and fullmove number are excluded from the position key to ensure proper opening transposition mapping.

### 2.5 Index Engine: Custom Immutable Sorted Block Index (`.cpi`)
- For distribution and read-only querying, we select the **Custom Immutable Sorted Index** (`.cpi`) with block-level zstd compression (256 positions/block) and an in-memory sparse index directory.
- **Empirical Storage Advantage:** Consumes **20.3–21.6 bytes per unique position**, which is **7.2× to 9.4× smaller on disk than SQLite tables**.
- **Empirical Query Latency:** Random-access on-disk lookups execute in **11–16 µs (p50)** on NVMe storage, while in-memory queries execute in **< 1.0 µs**.

### 2.6 Index Depth Strategy: Balanced Multi-Collection Tiering
- Unique position growth follows Heaps' Law ($U(N) = k \cdot N^\alpha$).
  - Depth 20: $k = 44.29, \alpha = 0.8023 \pm 0.0194, R^2 = 0.9995$
  - Depth 40: $k = 49.88, \alpha = 0.9293 \pm 0.0040, R^2 = 0.9999$
  - Depth 60: $k = 64.88, \alpha = 0.9565 \pm 0.0029, R^2 = 0.9999$
- **Recommended Option B:** Slicing **Masters OTB to Depth 40** (20 full moves, capturing >99.8% of theoretical opening preparation) and **Elite Online to Depth 30** (15 full moves, capturing tactical opening branches) yields a total combined deployment of **6.22 GB for 15 million games (38% headroom under 10 GB)**.

### 2.7 Verified Prefix Deduplication
- A candidate game supersedes an existing game **only** if the candidate is a legally verified prefix extension:
  `existing.moves == candidate.moves[..existing.moves.len()]`
- Divergent moves are preserved as distinct independent games (e.g. blitz playoff matches).
- Hash collisions are resolved via full move-by-move element equality comparison.

### 2.8 Evalis Local-First Independence
- The database project is developed independently of Evalis. Evalis functions 100% offline without requiring a running database server.

---

## 3. Explicitly Rejected Alternatives

1. **Rejected: Storing Plaintext PGN Files:**
   - Raw PGN consumes ~850 bytes/game. 15 million games would require ~12.8 GB for game text alone, exceeding the entire 10 GB budget before indexing.
2. **Rejected: Monolithic Zstandard PGN Stream:**
   - Monolithic compression prevents random access (requires sequential scan to fetch game #5,000,000).
3. **Rejected: SQLite for Deployed Position Tables:**
   - Physical on-disk SQLite tables require **~13.7 GB for 83.7M positions**, exceeding the budget. CPI reduces this to **~1.73 GB**.
4. **Rejected: Indexing Beyond Depth 40 (Depth 60):**
   - At Depth 60, $\alpha \approx 0.9565$ growth expands 15M games to **12.94 GB**, exceeding the 10 GB limit.
5. **Rejected: Naive Multi-Dimensional Materialization:**
   - Materializing every slice combination (Rating $\times$ Era $\times$ Speed) multiplies index size by 18×, exploding storage past 20 GB.

---

## 4. PR-02 Roadmap

1. Implement the production binary pipeline for `.cgb` block files and `.cpi` position tables.
2. Build the high-performance HTTP query daemon (`codb serve`) using `axum`.
3. Publish initial public dataset releases with cryptographically verifiable `provenance.json` manifests.
