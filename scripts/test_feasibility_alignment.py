#!/usr/bin/env python3
"""
scripts/test_feasibility_alignment.py — Regression test verifying strict numerical alignment
and absence of contradictory statements across storage projections, feasibility report,
methodology notes, README, and milestone roadmap.
"""

import copy
import json
import os
import re
import unittest

ROOT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROJECTIONS_PATH = os.path.join(ROOT_DIR, "benchmarks", "results", "projections_heaps.json")
FEASIBILITY_PATH = os.path.join(ROOT_DIR, "docs", "research", "feasibility-report.md")
README_PATH = os.path.join(ROOT_DIR, "README.md")

STALE_PATTERNS = [
    "7.50 GB",
    "6.44 GB",
    "9.71 GB",
    "7.06 GB",
    "0.8878",
    "8.22 GB",
    "comfortably below the 10 GB envelope under all tested scenarios",
]


def check_document_consistency(feasibility_text, readme_text, projections):
    """
    Validates that documentation and projections agree completely and contains zero
    contradictory statements or stale figures. Raises ValueError on contradiction.
    """
    # 1. Projections structure
    opt_b = projections.get("budget_scenarios_gb", {}).get(
        "option_b_recommended_d40_masters_d30_elite", {}
    )
    if not opt_b:
        raise ValueError("projections_heaps.json missing Option B scenario")

    baseline = opt_b.get("deployed_total_baseline")
    optimistic = opt_b.get("deployed_total_optimistic")
    conservative = opt_b.get("deployed_total_conservative")

    if baseline != 7.47 or optimistic != 6.28 or conservative != 9.66:
        raise ValueError(
            f"Invalid Option B figures: baseline={baseline}, opt={optimistic}, cons={conservative}"
        )

    # 2. Methodology note consistency
    note = projections.get("methodology_note", "")
    if "7.47 GB baseline" not in note or "6.28 GB optimistic" not in note or "9.66 GB conservative" not in note:
        raise ValueError("methodology_note does not match Option B calculated scenario totals")
    if "340 MB (3.4%) headroom" not in note:
        raise ValueError("methodology_note missing explicit 340 MB (3.4%) headroom note")

    # 3. Parameter consistency
    d30_elite_alpha = projections.get("parameters", {}).get("elite_online", {}).get("depth_30", {}).get("alpha_ols")
    if d30_elite_alpha != 0.9293:
        raise ValueError(f"Elite Online Depth 30 exponent is {d30_elite_alpha}, expected 0.9293")

    # 4. Check for prohibited stale figures in feasibility report and README
    for pat in STALE_PATTERNS:
        if pat.lower() in feasibility_text.lower():
            raise ValueError(f"feasibility-report.md contains stale pattern: {pat}")
        if pat.lower() in readme_text.lower():
            raise ValueError(f"README.md contains stale pattern: {pat}")

    # 5. Check presence of authoritative numbers and conditional feasibility in feasibility report
    required_feasibility_strings = [
        "7.47 GB baseline",
        "6.28 GB optimistic",
        "9.66 GB conservative",
        "7.03 GB",
        "340 MB",
        "conditional",
    ]
    for req in required_feasibility_strings:
        if req not in feasibility_text:
            raise ValueError(f"feasibility-report.md missing authoritative requirement: {req!r}")

    # 6. Check presence in README
    required_readme_strings = [
        "7.47 GB",
        "6.28 GB",
        "9.66 GB",
        "340 MB",
        "conditional",
    ]
    for req in required_readme_strings:
        if req not in readme_text:
            raise ValueError(f"README.md missing authoritative requirement: {req!r}")

    return True


class TestFeasibilityAlignment(unittest.TestCase):
    def setUp(self):
        with open(PROJECTIONS_PATH) as f:
            self.projections = json.load(f)
        with open(FEASIBILITY_PATH) as f:
            self.feasibility_text = f.read()
        with open(README_PATH) as f:
            self.readme_text = f.read()

    def test_authoritative_projections_and_docs_are_consistent(self):
        """Validates that real repo artifacts pass all alignment checks."""
        self.assertTrue(
            check_document_consistency(
                self.feasibility_text, self.readme_text, self.projections
            )
        )

    def test_negative_stale_baseline_fails(self):
        """Negative test: injecting stale 7.50 GB baseline causes failure."""
        bad_feasibility = self.feasibility_text.replace("7.47 GB baseline", "7.50 GB baseline")
        with self.assertRaises(ValueError) as ctx:
            check_document_consistency(bad_feasibility, self.readme_text, self.projections)
        self.assertIn("7.50", str(ctx.exception))

    def test_negative_stale_conservative_fails(self):
        """Negative test: injecting stale 9.71 GB conservative causes failure."""
        bad_feasibility = self.feasibility_text.replace("9.66 GB conservative", "9.71 GB conservative")
        with self.assertRaises(ValueError) as ctx:
            check_document_consistency(bad_feasibility, self.readme_text, self.projections)
        self.assertIn("9.71", str(ctx.exception))

    def test_negative_stale_exponent_fails(self):
        """Negative test: injecting stale 0.8878 exponent causes failure."""
        bad_feasibility = self.feasibility_text + "\nStale exponent: 0.8878\n"
        with self.assertRaises(ValueError) as ctx:
            check_document_consistency(bad_feasibility, self.readme_text, self.projections)
        self.assertIn("0.8878", str(ctx.exception))

    def test_negative_stale_methodology_note_fails(self):
        """Negative test: injecting stale note into projections causes failure."""
        bad_projections = copy.deepcopy(self.projections)
        bad_projections["methodology_note"] = "Option B projects at ~6.44 GB baseline (~8.22 GB conservative)."
        with self.assertRaises(ValueError) as ctx:
            check_document_consistency(self.feasibility_text, self.readme_text, bad_projections)
        self.assertIn("methodology_note does not match", str(ctx.exception))

    def test_negative_unconditional_feasibility_claim_fails(self):
        """Negative test: claiming unreserved 'comfortably below budget' causes failure."""
        bad_feasibility = self.feasibility_text + "\ncomfortably below the 10 GB envelope under all tested scenarios\n"
        with self.assertRaises(ValueError) as ctx:
            check_document_consistency(bad_feasibility, self.readme_text, self.projections)
        self.assertIn("comfortably below", str(ctx.exception))

    def test_roadmap_milestones_aligned(self):
        # PR-03 is Position Index Construction
        self.assertIn("PR-03 — Position Index Construction", self.feasibility_text)
        # PR-04 is Query Engine & Integration
        self.assertIn("PR-04 — Query Engine", self.feasibility_text)


if __name__ == "__main__":
    unittest.main()
