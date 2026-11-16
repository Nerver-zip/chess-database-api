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
   - Official FIDE Elo lists did not exist prior to 1970. A pre-1970 game is accepted without Elo headers **only if the record names both the event and the venue** (`Event` and `Site` tags present and not `?`). A pre-1970 date alone is never sufficient (`HistoricalWithoutProvenance`). This is an executable check, not a statement of trust in the source: no historical archive is part of the pinned corpus catalog (see [CORPUS_BUILD.md](CORPUS_BUILD.md)).
3. **Event-level OTB qualification for corpus releases:**
   - Lichess broadcast PGNs do not provide a universal OTB flag. The event auditor groups accepted games by the exact first tournament identifier in BroadcastURL, then records official Broadcast API tournament IDs, names, format, location, website, and standings fields. It aggregates repeated games from the same event.
   - **Verified online** requires explicit event-level metadata or an identifier naming online play or engine competition. A website hosted on a chess platform, a missing venue, and a high FIDE rating are not sufficient on their own.
   - **Verified OTB** requires a physical location in the official event metadata plus a linked event or standings page that independently matches at least two tournament identity terms and a location term. The audit records the source URL, response hash, matched terms, and classification evidence.
   - **Unresolved** covers unavailable metadata, missing venue information, or event and venue evidence that cannot be corroborated. These games are excluded from the Masters corpus release.
   - The corpus release workflow applies exact tournament slugs from corpus/masters_event_qualification.json. Only verified OTB tournaments can pass; verified online events and unresolved or missing tournament IDs fail closed. A direct online platform Site value still rejects a game. The Lichess [Broadcast help](https://lichess.org/broadcast/help) describes event venue and related details as optional, so absent Site information does not itself imply online play.
   - The source audit is preserved in every Masters release as RELEASE/MASTERS_EVENT_QUALIFICATION.json. It records aggregate game and event counts, the evidence policy, and per-event outcomes. The corpus.sh build workflow requires the audit; an ad hoc CLI build without the audit retains legacy marker checks and must not be represented as a provenance-qualified release.
4. **Move Legality & Orthodox Position:**
   - Standard orthodox starting position (`rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1`) or verified orthodox legal custom FEN setup.
   - Chess960, Fischer Random, and custom handicap variants are rejected.
   - At least 4 plies (2 full moves) to exclude accidental aborts and trivial forfeits.

#### Opt-in Gigabase source qualification

The CLI has a separate `--gigabase-source-policy` path for restricted private evaluation of Lumbra's Gigabase; this R3 policy now underlies the active private Masters deployment. It remains outside the pinned Lichess catalog and is not cleared for public redistribution. Policy `gigabase-otb/2026-11-r3` trusts the publisher's OTB Complete archive classification when a record has no concrete contradictory metadata. It accepts any nonempty `Source` tag and preserves the original value and publisher identity-evidence labels in the attribution sidecar. These choices broaden source and identity eligibility; they do not independently verify source rights, player identity, or play mode.

The r3 strength rules are symmetric by color: both supplied Elo values are at least 2200; one is at least 2400 and the other at least 1800; one exact case-insensitive `GM` title and an opponent Elo of at least 1800; or exact `GM` titles for both players. Any supplied Elo must parse and fall in the inclusive 100–3500 plausibility range. `GM` is treated as a publisher label, with no claim that the player held that title on the game date. Other titles, including `WGM`, do not satisfy the GM rule. Missing Elo can qualify only through the applicable GM-title rule.

An absent `Mode` relies on the archive-level assertion. Explicit correspondence, online, engine-only, or other non-OTB modes are rejected, as is concrete online or correspondence evidence in event/site metadata. A broadcast-hosting URL by itself does not contradict OTB play. Dates may be complete, valid partial dates, or unknown; supplied components are preserved, and there is no pre-1970 cutoff or inferred rating/title history. The existing standard-chess, legal-move, minimum-four-ply, termination, deduplication, and fail-closed accounting rules still apply. This opt-in policy is not enabled by the pinned Lichess catalog build and does not change the default Masters or Elite policy.

Ratings in this path are publisher-supplied values. They are not independently verified as official FIDE ratings for each game or as contemporaneous event ratings. The full-source `opening-explorer inventory` policy-impact report counts header candidates only; legal replay, deduplication, and final retention are measured by the production build. The inventory also reports candidates with pre-1970 dates without treating those dates as ineligible.

If a source-qualified candidate collides on stable game ID with a distinct normalized game record, the Gigabase path quarantines that candidate under `stable_game_id_collision`; the count is reported separately and included in accounting. Other deduplication errors remain fatal. This avoids publishing two records under one ID while preserving the existing duplicate and supersession rules.

Accepted source and identity fields are kept in an optional `source-attribution.jsonl` sidecar. The sidecar has a manifest checksum and one sorted record per retained game; deep verification aligns each record's stable game ID with `games.idx`. It is outside CGB3, so the persisted binary layout and existing datasets remain compatible. The sidecar is attribution evidence, not a record-level license grant.

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
   - Casual games are rejected (`CasualUnratedGame`) when the `Event` label contains `casual`. The Lichess *standard rated* archives contain only rated games, so this check is a defensive backstop.
   - **Provisional ratings:** The pinned Lichess PGN archives carry no provisional-rating marker, so this status cannot be distinguished from the exported Glicko-2 number and remains an explicit source limitation. Missing or unparsable ratings are rejected.
   - **Termination:** The reader checks PGN Termination values during curated ingestion. Aborted, Abandoned, Rules infraction, No contest, Unplayed, and Unterminated are rejected as AbortedOrInfraction; Normal and Time forfeit remain eligible. Unknown and missing values are not reinterpreted.
2. **Speed Category & Bullet Exclusion:**
   - **Bullet & UltraBullet are strictly excluded.** Extremely rapid time controls suffer from mouse slips, flag-hunting, and premoves that corrupt positional opening theory.
   - **Estimated Duration Formula:** Evaluated using estimated game length:
     $$T_{\text{est}} = \text{base\_seconds} + 40 \times \text{increment\_seconds}$$
   - Any game with $T_{\text{est}} < 180\text{ seconds}$ is classified as Bullet and rejected.
   - Valid accepted time controls: Blitz ($180\text{s} \le T_{\text{est}} < 600\text{s}$), Rapid ($600\text{s} \le T_{\text{est}} < 1800\text{s}$), and Classical ($T_{\text{est}} \ge 1800\text{s}$).
3. **Plies & Termination:**
   - Minimum 6 plies (3 full moves) to eliminate immediate resignations, disconnected connections, or pre-move blunders.
   - PGN Termination values that explicitly identify aborted, abandoned, unplayed, or rules-infraction games are rejected. Normal and Time forfeit are not rejected by this check.

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
- AbortedOrInfraction(String): the curated reader rejects explicit aborted, abandoned, no-contest, unplayed, unterminated, and rules-infraction Termination values.
- OnlineEventInMastersSource: Masters OTB only; the event audit or a direct online platform Site identifies online play.
- UnverifiedMastersProvenance: Masters OTB release only; no exact verified OTB broadcast tournament is present.
- `HistoricalWithoutProvenance`: Masters OTB only; pre-1970 date without both event and venue.

### 3.1 Machine-Readable Rejection Categories

The release build report records stable snake_case policy categories: below rating threshold, missing or provisional rating, casual or unrated game, bullet or ultrabullet, too few plies, non-standard variant or start, aborted or infraction termination, online Masters event, unresolved Masters provenance, and historical game without provenance. These values sum exactly to rejected_by_policy. Variant and illegal-move rejections retain their separate accounting counters.

### 3.2 Curation Policy Version

Releases record curation_policy_version from the source catalog (masters-otb/2026.11-r3, elite-online/2026.11-r2). Masters revision r3 adds a reproducible event-level qualification audit and accepts only its verified OTB tournament allowlist; verified online and unresolved events are rejected before deduplication. It retains the explicit online Site guard and historical venue check. Elite revision r2 adds explicit Termination filtering for aborted, abandoned, rules-infraction, no-contest, unplayed, and unterminated games. Unknown termination values remain eligible. Standard Lichess PGNs do not provide a provisional-rating marker, so ProvisionalRating cannot be inferred. Dataset changes under r3 have a new release ID and regenerated CPI, checksums, and verification records. Datasets built under earlier revisions are not directly comparable.
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
