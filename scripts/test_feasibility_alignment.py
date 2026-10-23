#!/usr/bin/env python3
"""
scripts/test_feasibility_alignment.py — Regression test verifying alignment across
storage projections, documentation, README, and milestone roadmap.
"""

import json
import os
import unittest

ROOT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROJECTIONS_PATH = os.path.join(ROOT_DIR, "benchmarks", "results", "projections_heaps.json")
FEASIBILITY_PATH = os.path.join(ROOT_DIR, "docs", "research", "feasibility-report.md")
README_PATH = os.path.join(ROOT_DIR, "README.md")


class TestFeasibilityAlignment(unittest.TestCase):
    def setUp(self):
        with open(PROJECTIONS_PATH) as f:
            self.projections = json.load(f)
        with open(FEASIBILITY_PATH) as f:
            self.feasibility_text = f.read()
        with open(README_PATH) as f:
            self.readme_text = f.read()

    def test_projections_json_option_b_numbers(self):
        opt_b = self.projections["budget_scenarios_gb"]["option_b_recommended_d40_masters_d30_elite"]
        self.assertEqual(opt_b["deployed_total_baseline"], 7.47)
        self.assertEqual(opt_b["deployed_total_optimistic"], 6.28)
        self.assertEqual(opt_b["deployed_total_conservative"], 9.66)

    def test_feasibility_report_aligned(self):
        self.assertIn("7.47 GB", self.feasibility_text)
        self.assertIn("6.28 GB", self.feasibility_text)
        self.assertIn("9.66 GB", self.feasibility_text)
        self.assertIn("340 MB", self.feasibility_text)
        self.assertIn("conditional", self.feasibility_text.lower())

    def test_roadmap_milestones_aligned(self):
        # PR-03 is Position Index Construction
        self.assertIn("PR-03 — Position Index Construction", self.feasibility_text)
        # PR-04 is Query Engine & Integration
        self.assertIn("PR-04 — Query Engine", self.feasibility_text)

    def test_readme_aligned(self):
        self.assertIn("7.47 GB", self.readme_text)
        self.assertIn("6.28 GB", self.readme_text)
        self.assertIn("9.66 GB", self.readme_text)
        self.assertIn("340 MB", self.readme_text)
        self.assertIn("conditional", self.readme_text.lower())


if __name__ == "__main__":
    unittest.main()
