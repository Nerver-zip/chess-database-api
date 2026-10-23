# ADR-0001: Initial Architecture, Storage, and Indexing Foundations

- **Status:** Approved / Recommended for Implementation (PR-02)
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

We adopt the following architectural pillars:

### 2.1 Workspace and Technology Stack
- **Language:** Rust (2021 edition), leveraging high memory safety, zero-cost abstractions, and fearless concurrency.
- **Modularity:** A clean Cargo workspace separating domain models (`core`), streaming parser (`ingest`), deduplication heuristics (`curate`), binary game blocks (`storage`), opening tree indexes (`index`), and CLI/benchmarking (`cli`).
- **Legal License:** Code is dual-licensed under **MIT OR Apache-2.0**. No code is imported or derived from AGPL-3.0 sources (`lila-openingexplorer`).

### 2.2 Dual Collection Isolation
The engine strictly segregates two collections:
1. **Masters OTB:** Tournament games between titled/rated masters sourced from official broadcasts (CC BY-SA 4.0) and public domain historical tournaments.
2. **Elite Online:** High-rated online games from Lichess standard rated archives (CC0 1.0 Universal).

> [!IMPORTANT]
> FIDE Elo ratings and online platform Glicko-2 ratings are never merged or conflated.

### 2.3 Storage Representation: CODB Block-Zstd Container
- **16-bit Move Encoding:** All legal chess moves are packed into a 16-bit word (`from`: 6 bits, `to`: 6 bits, `promo`: 3 bits, reserved: 1 bit). Replaying against legal board state resolves castling, en passant, and captures without ambiguity.
- **Dictionary Header Compression:** Players, events, and sites are dictionary-encoded in blocks of $N = 512$ games.
- **Block-level Zstandard:** Entire 512-game blocks are compressed with Zstandard Level 3–7, achieving **~144–185 bytes per game** (a 4.3× to 4.4× compression ratio over raw PGN), while enabling sub-millisecond $O(1)$ random-access game retrieval via a lightweight offset index (`.cgi`).

### 2.4 Position Identity: 128-bit Collision-Resistant Keys
- 64-bit Zobrist hashing is vulnerable to the Birthday Paradox at scale ($p \approx 10^{-5}$ at 20M positions).
- We standardize on a **128-bit key (`Zobrist128`)** using `shakmaty::zobrist::Zobrist128` with `EnPassantMode::Legal`. This guarantees zero hash collisions even across hundreds of millions of positions.
- Halfmove clock and fullmove number are excluded from the position key to ensure proper opening transposition mapping.

### 2.5 Index Depth Strategy: Standard Depth 40
- Unique position growth follows a power law ($U(N) \approx k \cdot N^\alpha$).
- Slicing up to **Depth 40 (20 full moves)** captures $>99.8\%$ of opening theory while consuming only **~1.02 GB per 10 million games**.
- Deep middlegame positions ($\ge 60$ plies) diverge into unique games and are omitted from the default opening explorer snapshot to preserve capacity headroom.

### 2.6 Index Engine: Custom Immutable Sorted Block Index (`.cpi`)
- For distribution and read-only querying, we select the **Custom Immutable Sorted Index** over general-purpose relational/embedded KV databases.
- Empirical benchmarks demonstrated **sub-microsecond lookup latency (p50: <1.0 µs, p99: 2.0 µs)** and an index size **4× smaller** than equivalent SQLite B-Tree tables.

### 2.7 Evalis Local-First Independence
- The query service is completely decoupled from Evalis. Evalis functions 100% offline without requiring a running database server.

---

## 3. Explicitly Rejected Alternatives

1. **Rejected: Storing Plaintext PGN Files:**
   - Raw PGN takes 600–900 bytes per game. 10 million games would consume ~8 GB for game text alone, leaving zero headroom for indexes or search structures.
2. **Rejected: Monolithic Zstandard PGN Stream:**
   - While achieving ~140 bytes/game, monolithic decompression requires scanning the entire file to retrieve game #5,000,000, eliminating interactive random access.
3. **Rejected: 64-bit Zobrist Hash Alone:**
   - At millions of unique positions, 64-bit keys risk hash collisions, corrupting opening statistics.
4. **Rejected: Unrestricted Indexing to Ply 80+:**
   - Positions beyond ply 40 exhibit $\alpha \approx 0.88$ growth with near-zero transpositional sharing, inflating the index beyond 6 GB without benefiting opening preparation.
5. **Rejected: Naive Multi-Dimensional Materialization:**
   - Materializing all combinations of (Rating Band $\times$ Year Band $\times$ Time Control $\times$ Collection) would multiply index size by 18×, exploding storage past 18 GB.
6. **Rejected: TWIC & Murky Internet Archives:**
   - Excluded due to UK/EU database copyright restrictions and lack of clean licensing chain of title.
7. **Rejected: Direct Scala/Akka Code Reuse from `lila-openingexplorer`:**
   - Excluded to avoid AGPL-3.0 copyleft obligations on the Rust codebase.

---

## 4. Consequences and PR-02 Roadmap

### Positive Consequences
- **Feasibility Confirmed:** 10M to 20M games fit within 2.8 GB to 5.3 GB (comfortably inside the 10 GB target).
- **Extreme Speed:** Ingestion exceeds 34,000 games/s; query latency is sub-microsecond.
- **Legal Safety:** Transparent CC0 and CC BY-SA 4.0 data segregation with full MIT/Apache-2.0 software independence.

### PR-02 Directives
1. Implement the binary production writer for `.cgb` block files and `.cpi` immutable position tables.
2. Build the HTTP query daemon (`codb serve`) using `axum`.
3. Publish initial public dataset releases with cryptographically verifiable `provenance.json` manifests.
