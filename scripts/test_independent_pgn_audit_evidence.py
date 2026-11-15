"""Offline validation for the anonymized independent PGN sample evidence."""

from __future__ import annotations

import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPORT_PATH = ROOT / "benchmarks/gigabase/INDEPENDENT_PGN_AUDIT.json"
TARGET_REPORT_PATH = ROOT / "benchmarks/gigabase/INDEPENDENT_PGN_AUDIT_TARGETED.json"
RUNBOOK_PATH = ROOT / "benchmarks/gigabase/INDEPENDENT_PGN_AUDIT_RUNBOOK.md"
HEX_256 = re.compile(r"^[0-9a-f]{64}$")
FORBIDDEN_KEYS = {
    "game_id", "stable_id", "sample_id", "white", "black", "event", "site",
    "moves", "fen", "raw_pgn", "player_name", "player_names", "event_name",
    "event_names", "source_id", "game_url", "broadcast_url",
}


def walk_keys(value):
    if isinstance(value, dict):
        for key, item in value.items():
            yield key
            yield from walk_keys(item)
    elif isinstance(value, list):
        for item in value:
            yield from walk_keys(item)


class IndependentPgnAuditEvidenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.report = json.loads(REPORT_PATH.read_text(encoding="utf-8"))

    def test_report_records_only_the_completed_byte_window_sample(self):
        report = self.report
        self.assertEqual(report["audit_kind"], "independent_pgn_parser_stratified_sample")
        sampling = report["sampling"]
        self.assertEqual(sampling["byte_offsets_requested"], 40_000)
        self.assertEqual(sampling["unique_candidate_games_selected"], 144)
        self.assertEqual(sampling["candidate_games_compared"], 144)
        self.assertFalse(sampling["target_quotas_met"])
        self.assertFalse(report["source_sha256_computed_by_this_audit"])
        validation = report["validation"]
        self.assertTrue(validation["all_sampled_source_rows_reconciled"])
        for key in (
            "source_parser_errors_on_candidate_id_hits", "source_illegal_mainline_games",
            "canonical_illegal_mainline_games", "candidate_retrieval_failures",
            "move_sequence_mismatches", "starting_position_mismatches", "final_position_mismatches",
        ):
            self.assertEqual(validation[key], 0, key)

    def test_external_gpl_tool_provenance_is_explicit(self):
        parser = self.report["external_parser"]
        self.assertEqual(parser["parser_package"], "chess")
        self.assertEqual(parser["parser_package_version"], "1.11.2")
        self.assertEqual(parser["distribution"], "python-chess")
        self.assertEqual(parser["distribution_version"], "1.999")
        self.assertIn("PyPI wheel", parser["parser_package_source"])
        self.assertEqual(parser["license"], "GPL-3.0-or-later")
        for key in (
            "wheel_sha256_local", "installed_metadata_sha256_local",
            "installed_license_sha256_local",
        ):
            self.assertRegex(parser[key], HEX_256)
        self.assertRegex(self.report["audit_runner_sha256_local"], HEX_256)
        self.assertFalse(parser["runner_in_git_or_pr"])
        self.assertFalse(parser["wheel_in_git_or_pr"])

    def test_memory_interpretation_does_not_claim_a_bounded_full_scan(self):
        memory = self.report["memory_interpretation"]
        self.assertEqual(memory["completed_sample_peak_rss_kib"], self.report["process_peak_rss_kib"])
        self.assertGreater(memory["completed_sample_peak_rss_kib"], 1_000_000)
        self.assertFalse(memory["completed_full_source_scan"])
        self.assertFalse(memory["completed_bounded_memory_full_source_scan"])
        self.assertFalse(memory["bounded_memory_claim_supported"])
        self.assertEqual(memory["separate_buffered_full_scan_maximum_line_bytes"], 4 * 1024 * 1024)
        self.assertEqual(memory["separate_buffered_full_scan_maximum_record_bytes"], 64 * 1024 * 1024)
        self.assertIn("interrupted_before_eof", memory["separate_buffered_full_scan_status"])

    def test_report_and_runbook_are_anonymized_and_scoped(self):
        keys = {key.casefold() for key in walk_keys(self.report)}
        self.assertFalse(FORBIDDEN_KEYS.intersection(keys))
        self.assertFalse(self.report["sampling"][
            "record_identifiers_player_names_event_names_and_raw_pgn_in_report"
        ])
        runbook = RUNBOOK_PATH.read_text(encoding="utf-8")
        self.assertIn("did **not** meet all planned quotas", runbook)
        self.assertIn("did not parse the full PGN", runbook)
        self.assertIn("GPL-3.0-or-later", runbook)
        self.assertIn("do **not** provide executable reproduction", runbook)
        self.assertIn("malformed source example was observed", runbook)

    def test_targeted_sample_reports_its_exact_scope_and_stratum_misses(self):
        report = json.loads(TARGET_REPORT_PATH.read_text(encoding="utf-8"))
        self.assertEqual(report["audit_kind"], "independent_pgn_parser_targeted_byte_sample")
        self.assertEqual(report["candidate_dataset_build_id"],
                         "codb-masters-otb-1794734897423894473-41647-72708103e81b880b-cpi-d40")
        self.assertEqual(report["source"]["sha256_from_local_inventory"],
                         "87fde5bc0a893fcfc877634a42bf0b41bd0a24b84b3e4d7553d6352f96dad759")
        self.assertFalse(report["source"]["sha256_recomputed_by_targeted_audit"])

        scan = report["scan"]
        self.assertEqual(scan["windows_requested"], 300_000)
        self.assertEqual(scan["windows_attempted"], 300_000)
        self.assertEqual(scan["source_records_examined"], 300_000)
        self.assertEqual(scan["source_bytes_read"], 434_132_364)
        self.assertEqual(scan["candidate_sidecar_games"], 156_898)
        self.assertEqual(scan["unique_candidate_ids_sampled"], 4_109)
        self.assertEqual(scan["policy_pass_records_without_candidate_id"], 0)
        self.assertEqual(scan["independent_parser_errors"], 0)
        self.assertEqual(scan["illegal_mainline_records"], 0)
        self.assertEqual(scan["sampled_header_parse_failures"], 0)

        sampling = report["sampling"]
        self.assertEqual(sampling["quota_per_stratum"], 10)
        self.assertFalse(sampling["all_quotas_met"])
        self.assertEqual(sampling["selected_unique_candidate_games"], 227)
        self.assertFalse(sampling["sample_row_ids_names_event_site_moves_and_fen_in_report"])
        strata = sampling["strata"]
        missing = {
            (dimension, category): evidence["selected_games"]
            for dimension, categories in strata.items()
            for category, evidence in categories.items()
            if not evidence["quota_met"]
        }
        self.assertEqual(missing, {
            ("source_tag", "CanadianChessFederation"): 0,
            ("source_tag", "ChessScotland"): 5,
            ("source_tag", "NiedersächsischerSchachverband"): 0,
            ("source_tag", "SchachverbandRuhrgebiet"): 0,
        })
        self.assertEqual(scan["source_tag_candidate_population_by_category"]["NiederrheinischerSchachverband"], 1)
        for categories in strata.values():
            for evidence in categories.values():
                selected = evidence["selected_games"]
                self.assertLessEqual(selected, 10)
                self.assertEqual(evidence["quota_met"], selected == 10)
                for count in evidence["comparison"].values():
                    self.assertEqual(count, selected)

        comparisons = report["comparisons"]
        self.assertEqual(comparisons["selected_games_retrieved"], 227)
        self.assertEqual(comparisons["exact_canonical_samples"], 227)
        for key in (
            "metadata_exact_matches", "move_sequence_matches", "starting_position_matches",
            "final_position_matches", "sidecar_attribution_matches", "canonical_legal_games",
        ):
            self.assertEqual(comparisons[key], 227, key)
        self.assertEqual(comparisons["candidate_retrieval_failures"], 0)

        controls = report["method"]["memory_controls"]
        self.assertFalse(controls["mmap_used"])
        self.assertEqual(controls["maximum_line_bytes"], 4 * 1024 * 1024)
        self.assertEqual(controls["maximum_record_bytes"], 64 * 1024 * 1024)
        self.assertIn("no fixed memory ceiling", controls["candidate_id_membership"])
        self.assertEqual(report["process_peak_rss_kib"], 339_388)
        self.assertGreater(report["duration_seconds"], 0)

        parser = report["external_parser"]
        self.assertEqual(parser["license"], "GPL-3.0-or-later")
        self.assertFalse(parser["runner_in_git_or_pr"])
        self.assertFalse(parser["wheel_in_git_or_pr"])
        for key in ("wheel_sha256_local", "installed_metadata_sha256_local",
                    "installed_license_sha256_local", "runner_sha256_local", "helper_sha256_local"):
            self.assertRegex(parser[key], HEX_256)

        runbook = RUNBOOK_PATH.read_text(encoding="utf-8")
        self.assertIn("INDEPENDENT_PGN_AUDIT_TARGETED.json", runbook)
        self.assertIn("300,000", runbook)
        self.assertIn("339,388 KiB", runbook)
        self.assertIn("not a full-source Python parse", runbook)
        keys = {key.casefold() for key in walk_keys(report)}
        self.assertFalse(FORBIDDEN_KEYS.intersection(keys))


if __name__ == "__main__":
    unittest.main()
