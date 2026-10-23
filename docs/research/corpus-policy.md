# Research Track B: Corpus Inclusion Policies & Curation Enforcers

## 1. Core Principles & Collection Isolation

The project explicitly rejects creating a single ambiguous collection of "strong chess games." Over-the-board (OTB) classical tournaments and high-rated online games reflect fundamentally different player populations, time pressures, psychological settings, and rating mathematics:

- **FIDE Elo** is an Elo system based on classical, rapid, and blitz over-the-board tournaments with slow rating adjustments and high entry barriers.
- **Lichess Glicko-2** is an online platform rating with dynamic rating deviation ($RD$) and continuous adjustment across high volumes of rapid, blitz, and bullet games.

> [!IMPORTANT]
> **Zero Conflation Rule:** OTB FIDE ratings and online platform ratings are **never** combined into a single number or pooled together in statistics. Queries against `masters-otb` and `elite-online` are strictly segregated.

---

## 2. Collection 1 — Masters OTB Policy

### 2.1 Scope & Purpose
To curate verified, high-level over-the-board competitive games from international tournaments, national championships, Olympiads, and historical master events.

### 2.2 Concrete Policy Specification (`MastersOtbPolicy`)
Implemented in `crates/curate/src/policy.rs`:

```rust
pub struct MastersOtbPolicy {
    pub min_both_elo: u16,        // Default: 2200
    pub min_one_elo: u16,         // Default: 2400 (IM/GM)
    pub min_other_elo: u16,       // Default: 2000
    pub min_plies: usize,         // Default: 4 (2 full moves)
    pub allow_historical_pre_1970: bool, // Default: true
}
```

### 2.3 Tiered Hierarchy & Historical Exception
1. **Modern Era (Post-1970 FIDE Elo):**
   - Both players $\ge 2200$ FIDE Elo, OR
   - One player $\ge 2400$ and opponent $\ge 2000$.
2. **Historical Era (Pre-1970 / Pre-FIDE Elo):**
   - Pre-1970 games from verified master events are accepted even when Elo headers are absent, as official FIDE Elo lists did not exist prior to 1970.
   - Historical master tournaments (e.g. Zurich 1953, AVRO 1938, St. Petersburg 1914) are preserved with verified player names.
3. **Standard Starting Position:**
   - Standard chess initial setup required (`rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1`).
   - Chess960 (Fischer Random) and handicap setups are rejected with `NonStandardStartingPosition`.

---

## 3. Collection 2 — Elite Online Policy

### 3.1 Scope & Purpose
To capture modern opening trends, deep tactical sidelines, and high-volume empirical statistics from verified top-tier online players.

### 3.2 Concrete Policy Specification (`EliteOnlinePolicy`)
Implemented in `crates/curate/src/policy.rs`:

```rust
pub struct EliteOnlinePolicy {
    pub min_both_rating: u16,     // Default: 2200
    pub min_one_rating: u16,      // Default: 2400
    pub min_other_rating: u16,    // Default: 2000
    pub min_plies: usize,         // Default: 6
    pub exclude_bullet: bool,     // Default: true
}
```

### 3.3 Strict Inclusion Filters
1. **Rating Thresholds:**
   - Both players $\ge 2200$ platform rating, OR
   - One player $\ge 2400$ and opponent $\ge 2000$.
   - Games with missing or unrated headers are strictly rejected.
2. **Speed Categories & Bullet Exclusion:**
   - **Bullet & UltraBullet Strictly Excluded:** Flag-hunting and severe time trouble produce erratic premoves, mouse slips, and unprincipled play that corrupt opening statistics.
   - **Duration Metric:** Time control is evaluated as estimated game duration:
     $$T_{\text{est}} = \text{initial\_seconds} + 40 \times \text{increment\_seconds}$$
     Any game with $T_{\text{est}} < 180\text{ seconds}$ is classified as Bullet and rejected.
   - Rapid ($\ge 600\text{s}$), Classical ($\ge 1800\text{s}$), and Blitz ($\ge 180\text{s}$) are accepted.
3. **Standard Starting Position Only:** Non-standard starting FENs and variants (Crazyhouse, King of the Hill) are rejected.

---

## 4. Rejection Taxonomy & Invariant Accounting

Every game encountered during ingestion is categorized into a mutually exclusive classification:

$$\text{total\_encountered} = \text{accepted\_unique} + \text{confirmed\_duplicates} + \text{superseded\_prior} + \text{rejected\_by\_policy} + \text{rejected\_variant} + \text{rejected\_illegal} + \text{rejected\_malformed} + \text{quarantined}$$

1. **ACCEPTED_UNIQUE:** Passed legal replay, satisfied policy, verified unique or legitimate transposition.
2. **CONFIRMED_DUPLICATES:** Exact duplicate of a previously indexed record.
3. **SUPERSEDED_PRIOR:** Legitimate complete game replacing an earlier truncated relay.
4. **REJECTED_BY_POLICY:** Sub-threshold rating, bullet time control, or unrated game.
5. **REJECTED_VARIANT / FEN:** Non-standard variant or non-standard starting position.
6. **REJECTED_ILLEGAL:** Legal move replay failure (illegal move geometry or invalid board state).
7. **REJECTED_MALFORMED:** Syntax error or corrupt PGN bytes.
8. **QUARANTINED:** Incomplete header records requiring heuristic review.
