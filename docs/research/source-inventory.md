---
summary: "Systematic survey and classification matrix of evaluated upstream chess data sources."
read_when: "Reviewing third-party source evaluations, volume estimates, or upstream classifications."
status: reference
---

# Research Track A: Source Inventory

> [!NOTE]
> This research document is preserved for reference. The canonical policy and inventory are consolidated in [docs/LICENSING.md](../LICENSING.md).

This document provides a systematic survey of prospective chess game data sources evaluated for the **Chess Openings DB** project.

## 1. Source Classification Criteria

Each candidate source is evaluated across thirteen dimensions:
1. **Official Distribution Location:** Canonical hosting URL or official API.
2. **Publisher / Maintainer:** Entity or individual responsible for publishing and maintaining the data.
3. **Explicit License and Redistribution Terms:** Published license or terms of service governing redistribution.
4. **Redistribution of Derivatives:** Legal permissibility of redistributing normalized, cleaned, compressed, or transformed subsets.
5. **Historical Coverage:** Temporal range of games in the archive.
6. **Approximate Volume:** Total count of raw records available.
7. **Update Frequency:** Cadence of new data additions.
8. **Player & Event Metadata:** Completeness of player names, FIDE/platform ratings, titles, event names, rounds, and sites.
9. **Rating Provenance:** Exact rating pool (FIDE Elo, Glicko-2, National Federation).
10. **Time-Control Metadata:** Presence and precision of `TimeControl` tags.
11. **Move Record Completeness:** Prevalence of incomplete moves, truncated games, or parsing errors.
12. **Duplicate Prevalence:** Frequency of redundant game entries or duplicate transmissions.
13. **Stable Source Identifiers:** Availability of immutable game IDs, URLs, or transmission keys.

### Classification Categories
- **Approved for Initial Public Corpus:** Fully transparent open license permitting redistribution of derivative works; verified provenance and quality.
- **Potentially Usable Subject to Further Verification:** Legally promising but requiring verification of upstream provenance or unbundled licensing.
- **Requires Explicit Permission:** Commercial or non-open terms requiring written authorization before any public redistribution.
- **Excluded:** Prohibited terms, legal liability, or tainted compilation provenance.

---

## 2. Source Inventory Matrix

| Source | Publisher | Explicit License | Derivative Redistribution | Volume | Rating Pool | Classification |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Lichess Standard Rated Games** | Lichess.org | CC0 1.0 Universal | Unrestricted | >6 Billion | Lichess Glicko-2 | **Approved** (Elite Online) |
| **Lichess Official Broadcasts** | Lichess.org | CC BY-SA 4.0 | Permitted with Attribution & ShareAlike | ~1.5 Million | FIDE Elo | **Approved** (Masters OTB) |
| **Lichess Opening Names** | Lichess.org | CC0 1.0 Universal | Unrestricted | ~4,000 ECOs | N/A (Metadata) | **Approved** (ECO Reference) |
| **The Week in Chess (TWIC)** | Mark Crowther | Proprietary / Restricted | Prohibited without written license | ~3.5 Million | FIDE Elo | **Excluded** |
| **PGN Mentor** | Attila Schneider | Unspecified (Personal use) | Unclear / No open grant | ~1.8 Million | FIDE Elo | **Excluded** |
| **Caissabase** | Caissabase.co.uk | Ambiguous (TWIC derivative) | High legal risk | ~5.0 Million | Mixed / FIDE | **Excluded** |
| **KingBase 2019** | Community mirror | Ambiguous / CC BY-SA claimed | Upstream provenance unverified | ~2.2 Million | FIDE Elo | **Potentially Usable (Deferred)** |
| **Pre-1928 Historical Archives** | Public Domain | Public Domain (US / Berne) | Unrestricted | ~150,000 | Historical Master | **Approved** (Historical OTB) |
| **FIDE Official PGN Relay** | FIDE | Proprietary / Terms vary | Requires event-by-event check | Varies | FIDE Elo | **Requires Explicit Permission** |

---

## 3. Detailed Source Evaluations

### 3.1 Lichess Standard Rated Games
- **Distribution Location:** `https://database.lichess.org/`
- **Maintainer:** Lichess.org (non-profit registered association, France).
- **License:** Creative Commons CC0 1.0 Universal (Public Domain Dedication).
- **Derivative Rights:** Explicitly permitted. Redistribution of normalized, compressed, and filtered subsets is 100% legal without royalties or copyleft obligations.
- **Historical Coverage:** January 2013 to present (updated monthly).
- **Volume:** >6.2 billion games as of late 2026.
- **Metadata Quality:** Very high consistency in standard tags (`White`, `Black`, `Result`, `UTCDate`, `UTCTime`, `WhiteElo`, `BlackElo`, `TimeControl`, `Site`). Includes clock times and move evaluation annotations in secondary archives.
- **Rating Provenance:** Lichess Glicko-2 rating pool. Distinct by time control (`Blitz`, `Rapid`, `Classical`, `Bullet`). **Must not be conflated with FIDE Elo.**
- **Time Controls:** Explicit `TimeControl` tag formatted as `<initial_seconds>+<increment_seconds>`.
- **Known Quality Limitations:** Highly heterogeneous skill distribution (from 400 to 3000+). Contains bullet games, berserk games, aborted games, bot accounts, server restarts, and resignation upon first blunder.
- **Curation Strategy:** Strict thresholding (e.g., both players >= 2200, non-aborted, classical/rapid/blitz only).

### 3.2 Lichess Official Broadcasts
- **Distribution Location:** `https://database.lichess.org/#broadcasts`
- **Maintainer:** Lichess.org and tournament broadcast organizers.
- **License:** Creative Commons Attribution-ShareAlike 4.0 International (CC BY-SA 4.0).
- **Derivative Rights:** Permitted under CC BY-SA 4.0. Derivative datasets must be distributed with proper attribution ("Data sourced from Lichess.org broadcasts") and under the same or compatible CC BY-SA 4.0 terms.
- **Historical Coverage:** 2020 to present, covering international elite tournaments, FIDE World Championships, Candidates, Grand Swiss, Chess Olympiads, and national championships.
- **Volume:** ~1,500,000 games across thousands of tournament stages.
- **Metadata Quality:** Features official FIDE player names, FIDE ratings, FIDE titles (`GM`, `IM`, `WGM`), round numbers, tournament event names, and boards.
- **Rating Provenance:** Official FIDE Elo.
- **Known Quality Limitations:** Relay transmission artifacts (temporary disconnects, duplicate PGN rounds, live operator input corrections, clock sync glitches).
- **Curation Strategy:** Automated legality replaying, strict move-sequence and event/round deduplication.

### 3.3 The Week in Chess (TWIC)
- **Distribution Location:** `https://theweekinchess.com/twic`
- **Publisher:** Mark Crowther.
- **Terms:** Commercial compilation. Free weekly download for personal study, but the publisher strictly reserves rights against mass republishing, automated scraping, or derivative database distribution.
- **Legal Assessment:** In the UK and EU, the *sui generis* database right (Directive 96/9/EC) protects substantial investment in obtaining, verifying, or presenting contents. Under US law, although factual move records lack copyright, compiling and redistributing the proprietary bundle violates site terms and risks contributory liability.
- **Conclusion:** **Excluded.** We will neither distribute nor build release artifacts from TWIC.

### 3.4 Caissabase and KingBase
- **Legal Assessment:** Caissabase and KingBase are free community compilations that aggregate historical games and weekly TWIC releases. Neither publisher provides a documented legal chain of title. Distributing derivatives creates transitive copyright risks.
- **Conclusion:** **Excluded from initial release.**

### 3.5 Pre-1928 Historical Archives
- **Legal Assessment:** Chess game moves played in historical matches (e.g. Steinitz vs Zukertort, Capablanca vs Alekhine) published prior to 1928 are in the worldwide public domain.
- **Quality Limitations:** Inconsistent dates, non-standard player transliterations (e.g., "Tschigorin" vs "Chigorin"), incomplete round information, absence of official Elo ratings (which began in 1970).
- **Conclusion:** **Approved** for Masters OTB historical baseline under verified public domain sources.

---

## 4. Source Selection for PR-01 and Beyond

```
┌────────────────────────────────────────────────────────┐
│               Approved Initial Sources                 │
├───────────────────────────┬────────────────────────────┤
│       Masters OTB         │        Elite Online        │
├───────────────────────────┼────────────────────────────┤
│ • Lichess Broadcasts      │ • Lichess Rated Games      │
│   (CC BY-SA 4.0)          │   (CC0 1.0 Universal)      │
│ • Historical PD Matches   │                            │
│   (Public Domain)         │                            │
└───────────────────────────┴────────────────────────────┘
```

Both collections fulfill the requirements for legally sound, publicly redistributable release artifacts.
