#!/usr/bin/env python3
"""Validate the committed real-corpus evidence under benchmarks/corpus/.

The evidence files are small JSON records copied from a real release build
(`RELEASE.json`, `build_report.json`, `quality_report.json`, `metrics.json`) and
from `corpus.sh http-check` (`http_check.json`). This validator checks schema,
exact accounting reconciliation, hash formats, and that the source checksums
match the pinned catalog. It performs no network access and needs no dataset.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EVIDENCE = ROOT / "benchmarks" / "corpus"
CATALOG = ROOT / "corpus" / "catalog.json"
SHA = re.compile(r"^[0-9a-f]{64}$")
COLLECTIONS = ("masters-otb", "elite-online")
COUNTERS = ("accepted_unique", "confirmed_duplicates", "superseded_prior", "rejected_by_policy",
            "rejected_variant", "rejected_illegal", "rejected_malformed", "quarantined")


def fail(msg: str) -> None:
    print(f"  [FAIL] {msg}")
    raise SystemExit(1)


def load(p: Path):
    try:
        return json.loads(p.read_text())
    except (OSError, json.JSONDecodeError) as e:
        fail(f"{p.relative_to(ROOT)}: {e}")


def main() -> int:
    catalog = load(CATALOG)
    sources = {s["id"]: s for s in catalog["sources"]}
    for c in COLLECTIONS:
        d = EVIDENCE / c
        if not d.is_dir():
            fail(f"missing evidence directory {d.relative_to(ROOT)}")
        rel = load(d / "RELEASE.json")
        build = load(d / "build_report.json")
        quality = load(d / "quality_report.json")
        metrics = load(d / "metrics.json")
        if rel["collection"] != c or build["collection"] != c or quality["collection"] != c:
            fail(f"{c}: collection identity mismatch across evidence files")
        acc = rel["accounting"]
        if acc["total_encountered"] != sum(acc[k] for k in COUNTERS):
            fail(f"{c}: accounting identity does not reconcile")
        if acc != {k: build["accounting"][k] for k in acc}:
            fail(f"{c}: RELEASE and build report accounting differ")
        if rel["accepted_games"] != acc["accepted_unique"] or quality["total_games"] != acc["accepted_unique"]:
            fail(f"{c}: accepted game counts differ")
        if not (build["is_reconciled"] and build["retained_matches_accepted"]):
            fail(f"{c}: build report not reconciled")
        if sum(build["policy_rejection_reasons"].values()) != acc["rejected_by_policy"]:
            fail(f"{c}: rejection reasons do not sum to rejected_by_policy")
        if not SHA.match(rel["index"]["cpi_sha256"]):
            fail(f"{c}: malformed CPI hash")
        for name, meta in rel["artifacts"].items():
            if not SHA.match(meta["sha256"]) or meta["size_bytes"] <= 0:
                fail(f"{c}: malformed artifact record {name}")
        if not rel["inputs"]:
            fail(f"{c}: no inputs recorded")
        for i in rel["inputs"]:
            s = sources.get(i["id"])
            if s is None or s["publisher_sha256"] != i["sha256"] or s["collection"] != c:
                fail(f"{c}: input {i['id']} does not match the pinned catalog")
            if s.get("verified_sha256") != i["sha256"]:
                fail(f"{c}: input {i['id']} has no verified_sha256 recorded in the catalog")
        lic = rel["license"]
        want = "CC-BY-SA-4.0" if c == "masters-otb" else "CC0-1.0"
        if lic["id"] != want or lic["share_alike"] != (c == "masters-otb"):
            fail(f"{c}: license record inconsistent")
        if set(metrics["steps"]) != {"build", "build-index", "verify", "report"}:
            fail(f"{c}: metrics steps incomplete")
        if any(m["returncode"] != 0 for m in metrics["steps"].values()):
            fail(f"{c}: a recorded step failed")
        lk = load(d / "lookup_benchmark.json")
        if lk["collection"] != c or lk["dataset_build_id"] != rel["dataset_version"]:
            fail(f"{c}: lookup benchmark does not belong to this release")
        if lk["hit_warm_pass"]["hits"] != lk["hit_warm_pass"]["count"] or lk["miss_warm_pass"]["hits"] != 0:
            fail(f"{c}: lookup benchmark hit/miss semantics violated")
        http = load(EVIDENCE / "http_check.json")
        if not http["passed"] or http["checks_failed"] != 0 or http["checks_total"] < 50:
            fail("http_check.json does not record a passing run")
        if http["collections"][c]["mismatches"] != 0:
            fail(f"{c}: HTTP/CLI mismatches recorded")
        print(f"  [PASS] {c}: {rel['accepted_games']} games, {len(rel['inputs'])} verified inputs, reconciled")

    examples = load(EVIDENCE / "http_examples.json")
    if not examples.get("health_ready"):
        fail("http_examples.json does not record a ready HTTP server")
    loaded = {x["collection"]: x for x in examples.get("collections_loaded", [])}
    if set(loaded) != set(COLLECTIONS):
        fail("http_examples.json does not record both loaded collections")
    for c in COLLECTIONS:
        rel = load(EVIDENCE / c / "RELEASE.json")
        ex = examples.get("examples", {}).get(c)
        if not ex or ex.get("dataset_version") != rel["dataset_version"]:
            fail(f"{c}: direct HTTP example does not match the recorded release")
        if loaded[c].get("dataset_version") != rel["dataset_version"]:
            fail(f"{c}: HTTP collections response does not match the release")
        pos = ex["position"]
        if pos.get("http_status") != 200 or not pos.get("cli_match"):
            fail(f"{c}: direct HTTP query failed or differs from the CLI")
        if pos["white_wins"] + pos["draws"] + pos["black_wins"] != pos["total_games"]:
            fail(f"{c}: direct HTTP W/D/L counts do not reconcile")
        game = ex["stored_game"]
        if (game.get("http_status") != 200 or
                not game.get("content_type", "").startswith("application/x-chess-pgn") or
                game.get("pgn_bytes", 0) <= 0 or not SHA.match(game.get("pgn_sha256", ""))):
            fail(f"{c}: direct stored-game retrieval evidence is invalid")

    marker_audit = load(EVIDENCE / "masters-otb" / "online_marker_audit.json")
    masters_release = load(EVIDENCE / "masters-otb" / "RELEASE.json")
    if (marker_audit.get("result") != "PASS" or marker_audit.get("flagged_games") != 0 or
            marker_audit.get("games_scanned") != masters_release["accepted_games"] or
            marker_audit.get("dataset_version") != masters_release["dataset_version"]):
        fail("Masters post-build online-marker audit is missing, stale, or failed")
    reproducibility = load(EVIDENCE / "reproducibility_check.json")
    for c in COLLECTIONS:
        rel = load(EVIDENCE / c / "RELEASE.json")
        check = reproducibility.get("collections", {}).get(c)
        if not check or check.get("collection") != c:
            fail(f"{c}: missing independent rebuild evidence")
        if not (check.get("same_binary_artifacts") and check.get("accounting_reconciled")):
            fail(f"{c}: independent rebuild did not match or reconcile")
        if check.get("accepted_games") != rel["accepted_games"]:
            fail(f"{c}: independent rebuild accepted-game count differs")
        reference = check.get("reference_sha256", {})
        repeated = check.get("repeat_sha256", {})
        if not reference or reference != repeated:
            fail(f"{c}: independent rebuild artifact hashes differ")
        for name, digest in reference.items():
            if name not in rel["artifacts"] or rel["artifacts"][name]["sha256"] != digest:
                fail(f"{c}: repeat-build hash does not match current release artifact {name}")
        if any(value <= 0 for value in check.get("wall_seconds", {}).values()):
            fail(f"{c}: independent rebuild timing is invalid")
        print(f"  [PASS] {c}: direct HTTP/CLI and deterministic payload evidence")
    return 0


if __name__ == "__main__":
    sys.exit(main())
