---
summary: "Normative corpus inclusion policies, rating isolation, deduplication engine, and accounting invariants."
read_when: "Configuring or modifying ingestion filters, curation rules, deduplication policies, or accounting."
status: current
---

# Corpus Inclusion & Deduplication Policies

## 1. Zero Conflation Rule and Collection Isolation

The project strictly segregates two distinct curated game collections:
1. **`masters-otb`:** Over-the-board classical, rapid, and blitz games contested by titled grandmasters and verified masters in national/international championships, Olympiads, and historical master events.
2. **`elite-online`:** High-rated standard online games from platform rated archives contested by top-tier accounts under clean time controls.

> [!IMPORTANT]
> **Zero Conflation Principle:** Over-the-board FIDE Elo ratings and online platform Glicko-2 ratings are fundamentally incompatible metrics with different inflation rates, deviation dynamics, and player pools. They are **never** averaged, pooled, or combined into composite figures. Statistics and opening lines remain strictly isolated by collection.

---

## 2. Collection Inclusion Policies (`policy.rs`)

Implemented in [`crates/curate/src/policy.rs`](../crates/curate/src/policy.rs).

### 2.1 Masters OTB Policy (`MastersOtbPolicy`)
```rust
pub struct MastersOtbPolicy {
    pub min_both_elo: u16,               // Default: 2200
    pub min_one_elo: u16,                // Default: 2400 (IM/GM threshold)
    pub min_other_elo: u16,              // Default: 2000
    pub min_plies: usize,                // Default: 4 (2 full moves)
    pub allow_historical_pre_1970: bool, // Default: true
}
```

#### Acceptance Criteria
1. **Modern Era (Post-1970 FIDE Elo):**
   - Both players have FIDE Elo $\ge 2200$, OR
   - One player has FIDE Elo $\ge 2400$ and opponent has FIDE Elo $\ge 2000$.
2. **Historical Era Exception (Pre-1970):**
   - Official FIDE Elo lists did not exist prior to 1970. Pre-1970 games from verified master tournaments (e.g. Zurich 1953, AVRO 1938, St. Petersburg 1914) are accepted even if Elo headers are absent, provided player names and event headers are authentic.
3. **Move Legality & Orthodox Position:**
   - Standard orthodox starting position (`rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1`) or verified orthodox legal custom FEN setup.
   - Chess960, Fischer Random, and custom handicap variants are rejected.
   - At least 4 plies (2 full moves) to exclude accidental aborts and trivial forfeits.

---

### 2.2 Elite Online Policy (`EliteOnlinePolicy`)
```rust
pub struct EliteOnlinePolicy {
    pub min_both_rating: u16,     // Default: 2200
    pub min_one_rating: u16,      // Default: 2400
    pub min_other_rating: u16,    // Default: 2000
    pub min_plies: usize,         // Default: 6 (3 full moves)
    pub exclude_bullet: bool,     // Default: true
}
```

#### Acceptance Criteria
1. **Rating Thresholds:**
   - Both players have verified online rating $\ge 2200$, OR
   - One player $\ge 2400$ and opponent $\ge 2000$.
   - Unrated and casual games are strictly rejected (`CasualUnratedGame`).
   - Provisional ratings (e.g. marked with `?` or high $RD$) are rejected (`ProvisionalRating`).
2. **Speed Category & Bullet Exclusion:**
   - **Bullet & UltraBullet are strictly excluded.** Extremely rapid time controls suffer from mouse slips, flag-hunting, and premoves that corrupt positional opening theory.
   - **Estimated Duration Formula:** Evaluated using estimated game length:
     $$T_{\text{est}} = \text{base\_seconds} + 40 \times \text{increment\_seconds}$$
   - Any game with $T_{\text{est}} < 180\text{ seconds}$ is classified as Bullet and rejected.
   - Valid accepted time controls: Blitz ($180\text{s} \le T_{\text{est}} < 600\text{s}$), Rapid ($600\text{s} \le T_{\text{est}} < 1800\text{s}$), and Classical ($T_{\text{est}} \ge 1800\text{s}$).
3. **Plies & Termination:**
   - Minimum 6 plies (3 full moves) to eliminate immediate resignations, disconnected connections, or pre-move blunders.
   - Games terminated by rules infractions or pre-move disconnects are rejected.

---

## 3. Rejection Reason Taxonomy

When a candidate game is rejected during curation, it is classified into one of the following mutually exclusive categories (`PolicyRejectionReason`):

- `BelowRatingThreshold { white_elo, black_elo, reason }`: Fails rating criteria.
- `ProvisionalRating`: Player rating is provisional.
- `CasualUnratedGame`: Casual or unrated match.
- `BulletOrUltraBullet { event, time_control }`: $T_{\text{est}} < 180\text{ s}$.
- `TooFewPlies { plies, minimum }`: Game too short to reflect intentional opening play.
- `NonStandardVariant(String)`: Chess960, Crazyhouse, Antichess, King of the Hill, etc.
- `NonStandardStartingPosition(String)`: Illegal initial setup.
- `AbortedOrInfraction(String)`: Abandoned before play or terminated by administrative infraction.

---

## 4. Deduplication and Identity Architecture (`dedupe.rs`)

Implemented in [`crates/curate/src/dedupe.rs`](../crates/curate/src/dedupe.rs).

Chess games cannot be identified solely by moves (opening transpositions frequently occur between different players in different tournaments) or solely by metadata (grandmasters contest multiple games in the same tournament). The deduplication engine resolves identity through a multi-key composite verification system:

```mermaid
flowchart TD
    RAW["Candidate Game"] --> NORM["Canonical Normalization<br/>(sanitize metadata, 16-bit moves, FNV-1a hash)"]
    NORM --> LOOKUP{"Existing Record with<br/>Same Metadata Key?"}

    LOOKUP -- No --> UNIQUE["Verdict: Unique Game<br/>(Register in catalog)"]

    LOOKUP -- Yes --> CHECK{"Compare Move Slices"}

    CHECK -- "Exact Match" --> DUP["Verdict: Confirmed Duplicate<br/>(Keep canonical; merge metadata)"]
    CHECK -- "Candidate longer &<br/>existing is strict prefix" --> SUPERSEDE["Verdict: Supersedes Previous<br/>(Replace truncated record with complete game)"]
    CHECK -- "Candidate shorter &<br/>candidate is strict prefix" --> TRUNC_DUP["Verdict: Confirmed Duplicate<br/>(Discard shorter candidate)"]
    CHECK -- "Moves diverge at any ply" --> INDEP["Verdict: Independent Game<br/>(Both preserved; distinct tournament games)"]
```

### 4.1 Normalized Metadata Key (`NormalizedMetadataKey`)
Metadata fields are normalized to prevent spurious variations (e.g. spelling or whitespace) from hiding duplicates:
- **Player Names:** Sanitized to lowercase `lastname,firstinitial` format (e.g., `"Carlsen, Magnus"` $\to$ `"carlsen,m"`).
- **Year:** Extracted 4-digit calendar year (or blank).
- **Event:** Lowercase alphanumeric string with punctuation trimmed.
- **Round:** Standardized string.
- **Delimiter Sanitization:** Any pipe character (`|`) is replaced with `/` to prevent delimiter injection attacks in serializations.

### 4.2 Verification Decision Matrix

| Condition | Verdict | Action Taken | Guarantee |
| :--- | :--- | :--- | :--- |
| **Identical Metadata + Exact Move Slice Equality** | `ConfirmedDuplicate` | Discard candidate; keep canonical record. Merge metadata if candidate has richer fields. | Element-by-element equality check prevents 64-bit FNV-1a hash collision false positives. |
| **Identical Metadata + Candidate longer + Existing is strict prefix** | `SupersedesPrevious` | Replace existing truncated record with the full game. Atomically update lookup catalogs. | Strict prefix validation: `candidate.moves[..existing.moves.len()] == existing.moves`. |
| **Identical Metadata + Candidate shorter + Candidate is strict prefix** | `ConfirmedDuplicate` | Discard candidate; retain existing complete game. | Prevents broken live relays from degrading existing full game records. |
| **Identical Metadata + Moves diverge at any ply** | `TranspositionDifferentGame` | Accept both as distinct unique games. | Preserves blitz/rapid tiebreak games played between the same opponents in the same event. |
| **Identical Moves + Different Metadata** | `Unique` | Accept both as distinct unique games. | Preserves theoretical transpositions and opening novelties across different events. |

---

## 5. Deduplication Backends

1. **`DeduplicatorBackend::Memory`:**
   - In-memory hash maps (`HashMap<NormalizedMetadataKey, SeenGameRecord>`).
   - Optimized for fast test suites, deterministic fixtures, and CI validation.
2. **`DeduplicatorBackend::Disk`:**
   - Embedded SQLite catalog housed in the temporary build staging directory (`catalog.sqlite`).
   - Indexes metadata keys and move hashes on disk, fetching move sequences on demand for slice comparisons.
   - Keeps memory footprint strictly bounded ($< 64\text{ MB}$ SQLite cache) while processing multi-million game archives.
   - Automatically cleaned up upon dataset finalization.

---

## 6. Accounting Invariant (Fail-Closed Verification)

The ingestion and curation pipeline enforces a strict mathematical invariant. Across every run, all encountered games are accounted for in mutually exclusive, collectively exhaustive categories:

$$\text{total\_encountered} = \text{accepted\_unique} + \text{confirmed\_duplicates} + \text{superseded\_prior} + \text{rejected\_by\_policy} + \text{rejected\_variant} + \text{rejected\_illegal} + \text{rejected\_malformed} + \text{quarantined}$$

Furthermore, retained canonical games must match accepted unique games:
$$\text{retained\_canonical\_games} = \text{accepted\_unique}$$

If any counter diverges by even a single game, the pipeline fails closed, aborts publication, and sets `is_reconciled = false`.
