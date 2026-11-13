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
        http = load(EVIDENCE / "http_check.json")
        if not http["passed"] or http["checks_failed"] != 0 or http["checks_total"] < 50:
            fail("http_check.json does not record a passing run")
        if http["collections"][c]["mismatches"] != 0:
            fail(f"{c}: HTTP/CLI mismatches recorded")
        print(f"  [PASS] {c}: {rel['accepted_games']} games, {len(rel['inputs'])} verified inputs, reconciled")
    return 0


if __name__ == "__main__":
    sys.exit(main())
