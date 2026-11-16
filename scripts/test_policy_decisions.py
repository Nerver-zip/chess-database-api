"""Offline checks that the current licensing and deployment policy is recorded and consistent.

These tests do not judge the legal question. They keep the maintainer's recorded decisions
machine-readable, keep the existing distribution clearance intact, and stop current-facing
documents from reasserting restrictions that the decisions superseded.
"""

import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RELEASES = ROOT / "releases"

# Current-facing files. Historical evidence (benchmarks/, docs/research/, handoff/, CHANGELOG.md,
# HANDOFF.md) and the immutable release records are deliberately excluded.
CURRENT_DOCS = [
    "README.md",
    "dataset/ATTRIBUTION.md",
    ".env.example",
    ".env.gigabase.example",
    "compose.yaml",
    "compose.named-volume.yaml",
    "compose.debug.yaml",
]
CURRENT_DOC_DIRS = ["docs", "examples"]
EXCLUDED_PREFIXES = ("docs/research/", "docs/adr/")

# Statements that described the earlier private-evaluation posture as current policy.
OBSOLETE = [
    r"public service and redistribution remain unauthori[sz]ed",
    r"public redistribution and public service (are|remain) (not authori[sz]ed|unauthori[sz]ed)",
    r"restricted to authorized private evaluation",
    r"requires additional source-rights authori[sz]ation",
    r"not cleared for public (distribution|service|use)",
    r"keep raw-pgn retrieval disabled",
    r"operators must disable retrieval when",
    r"this deployment is not a public service",
    r"remains? local/private",
]


def current_files():
    paths = [ROOT / p for p in CURRENT_DOCS]
    for d in CURRENT_DOC_DIRS:
        for p in sorted((ROOT / d).rglob("*")):
            rel = p.relative_to(ROOT).as_posix()
            if p.is_file() and p.suffix in {".md", ".sh", ".mjs", ".py"} and not rel.startswith(EXCLUDED_PREFIXES):
                paths.append(p)
    return paths


class DecisionRecordTests(unittest.TestCase):
    def setUp(self):
        self.decision = json.loads((RELEASES / "public_deployment_decision.json").read_text())
        self.clearance = json.loads((RELEASES / "distribution_clearance.json").read_text())

    def test_public_deployment_decision_covers_both_capabilities(self):
        d = self.decision
        self.assertEqual(d["status"], "authorized")
        text = " ".join(d["authorizes"]).lower()
        self.assertIn("position statistics", text)
        self.assertIn("/v1/collections/{collection}/games/{id}", text)
        self.assertIn("enabled by default", text)

    def test_commercial_use_and_bulk_export_stay_out_of_scope(self):
        text = " ".join(self.decision["not_authorized"]).lower()
        self.assertIn("commercial", text)
        self.assertIn("bulk export", text)

    def test_decision_is_not_presented_as_publisher_permission(self):
        ref = self.decision["decision_reference"].lower()
        self.assertIn("maintainer's interpretation", ref)
        self.assertIn("not separate written permission from the publisher", ref)
        self.assertTrue(self.decision["recorded_observations"])
        self.assertTrue(any("b2b" in o.lower() for o in self.decision["recorded_observations"]))

    def test_decision_names_what_it_supersedes_and_what_it_preserves(self):
        self.assertTrue(self.decision["supersedes_as_current_policy"])
        for path in self.decision["preserved_historical_records"]:
            if "/" in path and not path.startswith("the "):
                self.assertTrue((ROOT / path.rstrip("/")).exists(), path)

    def test_distribution_clearance_is_preserved(self):
        c = self.clearance
        self.assertEqual(c["status"], "cleared")
        self.assertEqual(c["release_id"], "masters-otb-r3")
        self.assertEqual(c["cleared_on"], "2026-11-16")
        self.assertTrue(c["clearance_reference"])
        self.assertEqual(len(c["recorded_conflicts"]), 4)


class CurrentDocumentationTests(unittest.TestCase):
    def test_no_current_document_reasserts_superseded_restrictions(self):
        problems = []
        for path in current_files():
            text = path.read_text(encoding="utf-8", errors="replace")
            for pattern in OBSOLETE:
                for m in re.finditer(pattern, text, re.IGNORECASE):
                    problems.append(f"{path.relative_to(ROOT)}: {m.group(0)!r}")
        self.assertEqual(problems, [])

    def test_gigabase_env_template_keeps_game_retrieval_enabled(self):
        text = (ROOT / ".env.gigabase.example").read_text()
        self.assertRegex(text, r"(?m)^OPENING_EXPLORER_ENABLE_GAME_RETRIEVAL=true$")

    def test_canonical_policy_section_exists_and_names_both_records(self):
        text = (ROOT / "docs" / "LICENSING.md").read_text()
        self.assertIn("## 6. Current policy for the Masters OTB dataset and public deployment", text)
        self.assertIn("releases/public_deployment_decision.json", text)
        self.assertIn("releases/distribution_clearance.json", text)
        self.assertIn("Not covered", text.replace("**Not covered.**", "Not covered"))
        self.assertIn("Required acknowledgment", text)

    def test_policy_documents_do_not_imply_commercial_authorization(self):
        text = (ROOT / "docs" / "LICENSING.md").read_text().lower()
        self.assertIn("commercial use", text)
        self.assertIn("not covered", text)


if __name__ == "__main__":
    unittest.main()
