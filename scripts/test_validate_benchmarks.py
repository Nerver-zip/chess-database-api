#!/usr/bin/env python3
"""
scripts/test_validate_benchmarks.py — Regression test suite for benchmark validation.

Verifies that:
1. A mathematically valid projection exceeding 10 GB passes integrity validation (Fix 1).
2. A valid regression with lower R^2 is not rejected solely because of its fit (Fix 1).
3. A valid exponent outside the previous preferred range is reported without automatic failure (Fix 1).
4. Incorrect derived calculations still fail (Data integrity).
5. Broken accounting invariants still fail (Data integrity).
"""

import copy
import json
import os
import sys
import tempfile
import unittest

# Import validation functions
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from validate_benchmarks import validate_benchmark_file, validate_projections


class TestValidateBenchmarks(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open("benchmarks/results/masters_20k.json") as f:
            cls.m20k_data = json.load(f)
        with open("benchmarks/results/projections_heaps.json") as f:
            cls.valid_projections = json.load(f)

    def test_valid_current_projections_pass(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(self.valid_projections, f)
            temp_path = f.name
        try:
            res = validate_projections(temp_path, self.m20k_data)
            self.assertEqual(res["assessment"], "within_budget")
            self.assertLessEqual(res["baseline_gb"], 10.0)
        finally:
            os.remove(temp_path)

    def test_over_budget_projection_passes_integrity_validation(self):
        """Condition 1: Mathematically valid projection exceeding 10 GB passes validation."""
        proj = copy.deepcopy(self.valid_projections)
        opt_b = proj["budget_scenarios_gb"]["option_b_recommended_d40_masters_d30_elite"]
        opt_b["deployed_total_optimistic"] = 11.5
        opt_b["deployed_total_baseline"] = 13.8  # Exceeds 10 GB
        opt_b["deployed_total_conservative"] = 17.2

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(proj, f)
            temp_path = f.name
        try:
            res = validate_projections(temp_path, self.m20k_data)
            # Must PASS validation without exception and report over_budget assessment
            self.assertEqual(res["assessment"], "over_budget")
            self.assertEqual(res["baseline_gb"], 13.8)
        finally:
            os.remove(temp_path)

    def test_lower_r2_passes_validation_with_advisory(self):
        """Condition 2: Valid regression with lower R^2 is not rejected solely because of fit."""
        proj = copy.deepcopy(self.valid_projections)
        proj["parameters"]["depth_40"]["r2"] = 0.94  # Below 0.999 and 0.99

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(proj, f)
            temp_path = f.name
        try:
            res = validate_projections(temp_path, self.m20k_data)
            # Must PASS validation without exception
            self.assertIn("within_budget", res["assessment"])
            self.assertTrue(any("depth_40" in adv and "R^2" in adv for adv in res["advisories"]))
        finally:
            os.remove(temp_path)

    def test_unexpected_exponent_passes_validation_with_advisory(self):
        """Condition 3: Valid exponent outside preferred range is reported without failure."""
        proj = copy.deepcopy(self.valid_projections)
        proj["parameters"]["depth_40"]["alpha_ols"] = 1.05  # Super-linear, > 0.98

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(proj, f)
            temp_path = f.name
        try:
            res = validate_projections(temp_path, self.m20k_data)
            # Must PASS validation without exception
            self.assertTrue(any("depth_40" in adv and "alpha_ols" in adv for adv in res["advisories"]))
        finally:
            os.remove(temp_path)

    def test_inconsistent_derived_calculation_fails(self):
        """Condition 4: Incorrect derived calculations still fail."""
        proj = copy.deepcopy(self.valid_projections)
        # Fabricate an incorrect SQLite multiplier that does not match m20k data
        proj["parameters"]["sqlite_multiplier_vs_cpi_at_d40"] = 99.99

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(proj, f)
            temp_path = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_projections(temp_path, self.m20k_data)
            self.assertIn("sqlite_multiplier_vs_cpi_at_d40", str(ctx.exception))
        finally:
            os.remove(temp_path)

        # Inconsistent scenario monotonicity: optimistic > baseline
        proj2 = copy.deepcopy(self.valid_projections)
        opt_b = proj2["budget_scenarios_gb"]["option_b_recommended_d40_masters_d30_elite"]
        opt_b["deployed_total_optimistic"] = 12.0
        opt_b["deployed_total_baseline"] = 6.96  # optimistic > baseline
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(proj2, f)
            temp_path2 = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_projections(temp_path2, self.m20k_data)
            self.assertIn("Inconsistent scenario totals", str(ctx.exception))
        finally:
            os.remove(temp_path2)

    def test_broken_accounting_invariant_fails(self):
        """Condition 5: Broken accounting invariants still fail."""
        corrupt_data = copy.deepcopy(self.m20k_data)
        # Corrupt the mutually exclusive accounting sum
        corrupt_data["total_encountered"] += 500

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(corrupt_data, f)
            temp_path = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_benchmark_file(temp_path)
            self.assertIn("Ingestion accounting does not sum", str(ctx.exception))
        finally:
            os.remove(temp_path)

        # Corrupt retained vs accepted
        corrupt_data2 = copy.deepcopy(self.m20k_data)
        corrupt_data2["retained_canonical_games"] -= 10
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(corrupt_data2, f)
            temp_path2 = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_benchmark_file(temp_path2)
            self.assertIn("retained_canonical_games", str(ctx.exception))
        finally:
            os.remove(temp_path2)

    def test_dataset_archive_with_import_duration_fails(self):
        """Condition 6: Ambiguous import_duration on dataset_archive is rejected."""
        with open("benchmarks/results/pr02_scale_elite_100k.json") as f:
            archive_data = json.load(f)

        corrupt = copy.deepcopy(archive_data)
        corrupt["import_duration_secs"] = 0.1544  # Ambiguous duplicate duration!

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(corrupt, f)
            temp_path = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_benchmark_file(temp_path)
            self.assertIn("Ambiguous benchmark semantics", str(ctx.exception))
        finally:
            os.remove(temp_path)

    def test_placeholder_sha256_is_rejected(self):
        """Condition 7: Placeholder strings as cryptographic hashes are rejected."""
        with open("benchmarks/results/pr02_scale_elite_100k.json") as f:
            archive_data = json.load(f)

        corrupt = copy.deepcopy(archive_data)
        corrupt["source_archives"][0]["sha256"] = "streamed-external-source"

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(corrupt, f)
            temp_path = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_benchmark_file(temp_path)
            self.assertIn("Invalid SHA-256 in source_archives", str(ctx.exception))
        finally:
            os.remove(temp_path)

    def test_verified_status_without_hash_is_rejected(self):
        """Condition 8: Provenance claim 'verified' without valid hash is rejected."""
        with open("benchmarks/results/pr02_scale_elite_100k.json") as f:
            archive_data = json.load(f)

        corrupt = copy.deepcopy(archive_data)
        corrupt["provenance_status"] = "verified"  # Claiming verified without sha256!
        corrupt["source_archives"][0]["sha256"] = None

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(corrupt, f)
            temp_path = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_benchmark_file(temp_path)
            self.assertIn("lacks sha256", str(ctx.exception))
        finally:
            os.remove(temp_path)

    def test_verified_status_dataset_archive_without_checksum_verified_is_rejected(self):
        """Historical dataset with valid SHA-256 but no checksum_verified proof cannot claim 'verified'."""
        with open("benchmarks/results/pr02_scale_elite_100k.json") as f:
            archive_data = json.load(f)

        corrupt = copy.deepcopy(archive_data)
        corrupt["provenance_status"] = "verified"
        corrupt["source_archives"][0]["sha256"] = "a" * 64
        corrupt["source_archives"][0].pop("checksum_verified", None)

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(corrupt, f)
            temp_path = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_benchmark_file(temp_path)
            self.assertIn("lacks physical build-time verification proof", str(ctx.exception))
        finally:
            os.remove(temp_path)

    def test_verified_status_dataset_archive_with_checksum_verified_passes(self):
        """Dataset archive with valid SHA-256 and checksum_verified=True passes when claiming 'verified'."""
        with open("benchmarks/results/pr02_scale_elite_100k.json") as f:
            archive_data = json.load(f)

        valid = copy.deepcopy(archive_data)
        valid["provenance_status"] = "verified"
        valid["source_archives"][0]["sha256"] = "a" * 64
        valid["source_archives"][0]["checksum_verified"] = True

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(valid, f)
            temp_path = f.name
        try:
            res = validate_benchmark_file(temp_path)
            self.assertEqual(res["provenance_status"], "verified")
        finally:
            os.remove(temp_path)

    def test_physical_archive_bytes_as_input_size_is_rejected(self):
        """Condition 9: Conflating total persistent bytes with input size is rejected."""
        with open("benchmarks/results/pr02_scale_elite_100k.json") as f:
            archive_data = json.load(f)

        corrupt = copy.deepcopy(archive_data)
        # Put total_persistent_bytes (19671343) into input_size_bytes instead of source size
        corrupt["input_size_bytes"] = corrupt["persistent_storage"]["total_persistent_bytes"]

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(corrupt, f)
            temp_path = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_benchmark_file(temp_path)
            self.assertIn("incorrectly populates source input_size_bytes", str(ctx.exception))
        finally:
            os.remove(temp_path)

    def test_unknown_source_size_cannot_disguise_as_archive_bytes(self):
        """Condition 10: Unknown source size cannot disguise archive bytes as input size."""
        with open("benchmarks/results/pr02_scale_elite_100k.json") as f:
            archive_data = json.load(f)

        corrupt = copy.deepcopy(archive_data)
        # Zero source size (unknown source) but set input_size_bytes = total_persistent_bytes
        corrupt["source_archives"][0]["size_bytes"] = 0
        corrupt["input_size_bytes"] = corrupt["persistent_storage"]["total_persistent_bytes"]

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(corrupt, f)
            temp_path = f.name
        try:
            with self.assertRaises(ValueError) as ctx:
                validate_benchmark_file(temp_path)
            self.assertIn("incorrectly populates source input_size_bytes", str(ctx.exception))
        finally:
            os.remove(temp_path)

    def test_unknown_source_size_with_none_passes(self):
        """Condition 11: Unknown source size represented honestly as null passes."""
        with open("benchmarks/results/pr02_scale_elite_100k.json") as f:
            archive_data = json.load(f)

        valid = copy.deepcopy(archive_data)
        valid["source_archives"][0]["size_bytes"] = 0
        valid["input_size_bytes"] = None

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(valid, f)
            temp_path = f.name
        try:
            res = validate_benchmark_file(temp_path)
            self.assertIsNone(res["input_size_bytes"])
        finally:
            os.remove(temp_path)


if __name__ == "__main__":
    unittest.main()
