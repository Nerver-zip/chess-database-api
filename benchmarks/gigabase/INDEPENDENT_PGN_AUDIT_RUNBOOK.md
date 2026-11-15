# Independent PGN Audit Runbook

## Scope

[`INDEPENDENT_PGN_AUDIT.json`](INDEPENDENT_PGN_AUDIT.json) records a local independent-parser byte-window sample of the Gigabase candidate. It examined 40,000 systematic offsets, compared 144 unique candidate games, and checked year, rating, game-length, source, and identity dimensions. The sample did **not** meet all planned quotas. The scan did not parse the full PGN or provide a full-source coverage result.

The report contains aggregate checks only; it omits record IDs, player/event/site values, move text, FENs, and raw PGN. The source hash is copied from the local structural inventory and was not recomputed by the 40,000-window audit. The report points to the pre provenance-correction candidate build; the later corrected candidate's canonical content was separately reported as unchanged, but this independent parser sample was not rerun against that build. This evidence does not establish event-level OTB, official FIDE Elo, or rights.

## Full-source scan follow-up

A separate sequential full-source attempt used bounded `readline()` calls (4 MiB maximum line) and a 64 MiB maximum record. It was interrupted before EOF because progress was too slow; no completion report, final peak RSS, elapsed scan time, or quota result was produced. One earlier observation during that attempt showed 36–39 MiB RSS after about 90 seconds, but this is not a completed-run peak or evidence that the full scan finished within that footprint. The completed 144-game sample used an mmap and recorded 1,386,212 KiB peak RSS. Neither run supports a bounded-memory full-source claim. The separate completed source-to-canonical and source-deduplication reports use the production Shakmaty parser and are documented independently.

## External parser and licensing boundary

The parser library is the PyPI wheel `chess==1.11.2`, imported as `chess`, with license `GPL-3.0-or-later`; the installed `python-chess==1.999` distribution is its PyPI metapackage. The report records the exact parser wheel, wheel SHA-256, installed metadata and license hashes, and the local runner SHA-256. The checker and wheel remain under ignored `data/work/` or the local pip cache; neither is part of CODB, the repository, or PR #7. Do not add this package as a CODB runtime/build dependency or redistribute its source in this MIT OR Apache-2.0 project.

Because the GPL checker itself is not in Git, this report and runbook do **not** provide executable reproduction from the PR checkout alone. They record the exact local tool artifacts and command used so the local run can be audited while preserving the repository licensing boundary. A future in-repository executable reproduction needs a separately licensed compatible parser.

## Local reproduction

Reproducing the original byte-window audit requires the ignored source archive extraction, the earlier local candidate dataset, Docker container `codb-pr7-rust-work`, Python 3.13.5, and the pinned wheel installed under `data/work/gigabase-eval/audit-python-deps/`. No network access or source download is needed after these local artifacts exist. The runner is local-only and embeds its paths and sample configuration; the PR checkout alone cannot execute it.

```bash
PYTHONPATH=data/work/gigabase-eval/audit-python-deps:data/work/gigabase-eval \
python3 data/work/gigabase-eval/independent_pgn_sample_audit.py
```

This command reruns the original 40,000-window sample and writes only under ignored `data/work/`; it is not the interrupted full-source scanner. The source file and report paths are hard-coded in the local runner. The audit leaves the dataset and production release pointers untouched.

## Supplemental targeted byte-window sample

[`INDEPENDENT_PGN_AUDIT_TARGETED.json`](INDEPENDENT_PGN_AUDIT_TARGETED.json) records a separate deterministic 300,000-window seek/read sample against the provenance-corrected candidate. It examined 300,000 record windows, consumed 434,132,364 logical source bytes, and independently parsed 4,109 header-prefiltered records that joined to candidate IDs. Min-hash selection compared 227 unique candidate games. All 227 had legal canonical moves and exact matches for persisted metadata, move sequence, starting and final board, and source attribution; parser errors, illegal lines, missing joins, retrieval failures, and comparison mismatches were zero.

Ten source-tag strata had at least ten candidate games and were eligible for the quota. The audit met ten-game quotas in 6 of those 10 source strata, and in all supported year, rating, game-length, and identity strata. Four source strata missed quota: CanadianChessFederation (0/10), ChessScotland (5/10), NiedersächsischerSchachverband (0/10), and SchachverbandRuhrgebiet (0/10). The allowed NiederrheinischerSchachverband tag had one candidate game and was below the ten-game support threshold. These are byte-window sample limitations, not missing or rejected source records.

The targeted run took 100.238 seconds and measured 339,388 KiB peak RSS. It used no `mmap`, capped sampled lines at 4 MiB and records at 64 MiB, and retained candidate membership in an in-memory SQLite database. This RSS is the observed process peak for this run; there is no fixed memory ceiling claim. The source SHA-256 was copied from the local inventory rather than recomputed by this audit. The checker and wheel remain local-only; exact parser, license, wheel, installed metadata, and runner hashes are in the report. This is not a full-source Python parse or an O(1) full-source memory result.

```bash
PYTHONPATH=data/work/gigabase-eval/audit-python-deps:data/work/gigabase-eval \
python3 data/work/gigabase-eval/independent_pgn_targeted_audit.py
```

This local runner uses the corrected candidate at `data/work/gigabase-eval/candidate-provenance-r1/` and writes the committed aggregate report. It does not modify the dataset or either production release pointer. The report includes no row IDs, names, event/site values, moves, or FENs.

## Evidence interpretation

- The 144 selected rows had no parser errors, illegal moves, retrieval failures, move mismatches, start-position mismatches, or final-board mismatches. Metadata differences in the sample reconciled to duplicate or stable-ID collision dispositions.
- Full-source duplicate/collision examples and source-attribution checks are in `SOURCE_DEDUPE_AUDIT.json` and `SOURCE_CANONICAL_AUDIT.json`; those use the production parser and deduplicator, not python-chess.
- Every accepted candidate lacked a per-game `Mode` tag and therefore relies on the publisher's archive-level OTB assertion. This does not imply event-level verification.
- The structural inventory observed zero recoverable syntax errors, but rejected records were not all replayed independently. No malformed source example was observed; synthetic parser fixtures are separate.
- The offline report validator checks both committed sample reports for their measured scope, quota results, external-tool hashes, and privacy exclusions. It does not rerun the local-only GPL checker or require the archive, wheel, or Docker.
