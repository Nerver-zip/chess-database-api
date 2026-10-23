# Research Track B: Corpus Inclusion Policies

## 1. Core Principles & Collection Isolation

The project explicitly rejects creating a single ambiguous collection of "strong chess games." Over-the-board (OTB) classical tournaments and high-rated online blitz games reflect fundamentally different player populations, time pressures, psychological settings, and rating mathematics:

- **FIDE Elo** is an Elo system based on classical, rapid, and blitz over-the-board tournaments with slow rating adjustments and high entry barriers.
- **Lichess Glicko-2** is an online platform rating with dynamic rating deviation ($RD$) and continuous adjustment across high volumes of rapid, blitz, and bullet games.

> [!IMPORTANT]
> **Zero Conflation Rule:** OTB FIDE ratings and online platform ratings are **never** combined into a single number or pooled together in statistics. Queries against `masters-otb` and `elite-online` are strictly segregated.

---

## 2. Collection 1 — Masters OTB Policy

### 2.1 Scope & Purpose
To curate verified, high-level over-the-board competitive games from international tournaments, national championships, and historical master events.

### 2.2 Inclusion Rules & Tiered Hierarchy

```
┌────────────────────────────────────────────────────────┐
│               Masters OTB Tiers                        │
├────────────────────────────────────────────────────────┤
│ Tier 1: Modern Elite OTB (Post-1970)                   │
│   • Both players >= 2300 FIDE Elo, OR                  │
│   • At least one player >= 2500 (GM title), other >= 2100
├────────────────────────────────────────────────────────┤
│ Tier 2: Open Master Competitive                        │
│   • Both players >= 2100 FIDE Elo, OR                  │
│   • Officially titled players (GM, IM, WGM, FM)        │
├────────────────────────────────────────────────────────┤
│ Tier 3: Historical Master (Pre-1970 / Pre-Elo)         │
│   • Curated historical tournaments (e.g. Zurich 1953)  │
│   • Matches featuring recognized historical masters    │
│   • FIDE ratings recorded as `None` (not invented)     │
└────────────────────────────────────────────────────────┘
```

### 2.3 Time Control Semantics
- **Explicit Preservation:** If a PGN lacks a `TimeControl` header, the field is preserved as `None` / `Unknown`. **It is never inferred to be classical.**
- **Event Tags:** Tags such as "World Rapid", "World Blitz", "Superbet Rapid", or `Event` containing "blitz" are marked with category `Rapid` or `Blitz`.
- **Primary Focus:** Classical time controls are the golden standard for opening preparation and theory. Rapid and Blitz events from official broadcast relays are indexed with explicit time-category flags.

### 2.4 Acceptance & Rejection Criteria

| Condition | Action | Rationale |
| :--- | :--- | :--- |
| Full legal move replay succeeds | **Accept** | Confirms game integrity and rules fidelity. |
| Illegal move or board state error | **Reject** | Corrupt transmission or corrupted score sheet. |
| Non-standard starting position (Chess960) | **Reject** | Out of scope for standard opening explorer. |
| Total plies < 4 without clear draw/resignation | **Quarantine** | Incomplete broadcast transmission or aborted relay. |
| Identical move sequence + same event + same date | **Deduplicate** | Redundant broadcast relay stream. |
| Identical move sequence + different event/year | **Preserve** | Genuine transposition or distinct historical game. |

---

## 3. Collection 2 — Elite Online Policy

### 3.1 Scope & Purpose
To capture modern opening trends, deep tactical sidelines, and high-volume empirical statistics from verified top-tier online players.

### 3.2 Evaluation of Candidate Rating Thresholds (Lichess Pool)

We evaluate three candidate thresholds applied to standard rated games:

| Threshold Dimension | Candidate 2200+ | Candidate 2400+ | Candidate 2500+ | Recommended Policy |
| :--- | :--- | :--- | :--- | :--- |
| **Percentile of Active Pool** | Top ~3.5% | Top ~0.6% | Top ~0.15% | **Both $\ge 2200$ OR One $\ge 2400$ & Opp $\ge 2100$** |
| **Monthly Game Volume** | ~2.5M games | ~450k games | ~120k games | ~1.2M games |
| **Opening Discipline** | Good (some blunders) | High (theoretical) | Elite (GM/Master prep) | Balanced coverage & quality |
| **Rare Sideline Representation** | Excellent | Moderate | Sparse in rare lines | Excellent depth without noise |
| **Estimated 10GB Capacity Fit** | Requires aggressive ply pruning | Fits comfortably | Fits with full moves | Highly sustainable |

### 3.3 Strict Inclusion Filters
1. **Standard Variant Only:** Standard chess starting position (`FEN: rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1`). Exclude Crazyhouse, Chess960, King of the Hill, etc.
2. **Rated Only:** `Rated` flag must be true. Exclude casual games where players experiment or play unscientifically.
3. **Speed Categories:**
   - **Blitz:** Included ($\ge 180\text{s}$ or $\ge 120\text{s}+1\text{s}$).
   - **Rapid:** Included ($\ge 600\text{s}$).
   - **Classical:** Included ($\ge 1800\text{s}$).
   - **Bullet & Ultrabullet:** **Strictly Excluded.** Flag-hunting under severe time pressure produces mouse slips, spurious premoves, and unprincipled opening play that pollutes opening statistics.
4. **Rating Reliability:** Established ratings only. Exclude provisional accounts (flagged by high rating deviation or `?` in rating).
5. **Game Termination:** Exclude games terminated by `Abandoned` before move 3 or marked as `Rules infraction` (cheat bans).
6. **Minimum Plies:** Minimum 6 plies (3 full moves).

---

## 4. Rejection & Quarantine Taxonomy

Every game encountered by the pipeline is cataloged into one of five deterministic buckets:

1. **ACCEPTED:** All headers parsed, all moves verified legally on the board, matches collection criteria.
2. **REJECTED_VARIANT:** Chess960, Antichess, or other non-standard chess variants.
3. **REJECTED_FILTER:** Sub-threshold rating, excluded speed category (bullet), casual game, or aborted.
4. **REJECTED_ILLEGAL:** Move parsing failure, illegal move geometry, illegal check evasions, or bad promotion.
5. **QUARANTINED:** Malformed headers, missing player names, unparseable dates, or potential corruption requiring manual or specialized heuristics.
6. **DUPLICATE_SUPERSEDED:** Valid game verified as an exact duplicate of a higher-priority or earlier imported record.

---

## 5. Tradeoffs & PR-02 Directives

- **Filter Generosity vs Storage:** Setting the Elite Online threshold to 2200 increases game volume by ~5× compared to 2400. To preserve storage within the 10 GB limit, indexing depth can be tuned (e.g. indexing up to 30 plies for 2200+ games, or storing full move records only for 2400+ games).
- **Historical Gaps:** FIDE Elo was officially adopted in 1970. Historical games prior to 1970 must use event/player reputation filters rather than numerical Elo thresholds.
