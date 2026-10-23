#!/usr/bin/env python3
"""
scripts/test_reproduce_runner.py — Regression tests for reproduce_benchmarks.sh runner.

Verifies:
1. --ci succeeds cleanly without external empirical datasets.
2. --all fails closed (exit code 1) when empirical datasets are missing.
3. --all fails closed (exit code 1) when dataset checksums do not match expected hashes.
4. Invalid/unknown execution modes fail closed.
"""

import os
import subprocess
import tempfile
import unittest

ROOT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUNNER_PATH = os.path.join(ROOT_DIR, "scripts", "reproduce_benchmarks.sh")


class TestReproduceRunner(unittest.TestCase):
    def test_ci_mode_succeeds_without_external_data(self):
        """Condition 1: --ci succeeds cleanly without external datasets."""
        res = subprocess.run(
            [RUNNER_PATH, "--ci"],
            cwd=ROOT_DIR,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertEqual(
            res.returncode,
            0,
            f"--ci mode failed with code {res.returncode}:\n{res.stderr}",
        )
        self.assertIn("PR-01 CI Benchmark Validation Summary", res.stdout)
        self.assertIn("Overall Status:          SUCCESS", res.stdout)

    def test_all_mode_fails_closed_when_data_missing(self):
        """Condition 2: --all fails closed (exit 1) when empirical datasets are missing."""
        env = {**os.environ, "CODB_DATA_DIR": "/tmp/nonexistent_data_dir_pr01_test"}
        res = subprocess.run(
            [RUNNER_PATH, "--all"],
            cwd=ROOT_DIR,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        # MUST fail closed with nonzero exit code
        self.assertNotEqual(
            res.returncode,
            0,
            "--all mode unexpectedly succeeded with missing datasets!",
        )
        self.assertTrue(
            "Missing required file" in res.stderr
            or "not found" in res.stdout
            or "error" in res.stderr.lower(),
            f"Expected error message missing from output:\nstdout: {res.stdout}\nstderr: {res.stderr}",
        )

    def test_all_mode_fails_closed_on_checksum_mismatch(self):
        """Condition 3: --all fails closed on checksum mismatch."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create a corrupted data/masters_combined.pgn.zst with invalid bytes
            with open(os.path.join(tmpdir, "masters_combined.pgn.zst"), "wb") as f:
                f.write(b"CORRUPT_INVALID_BYTES_1234567890")

            env = {**os.environ, "CODB_DATA_DIR": tmpdir}
            res = subprocess.run(
                [RUNNER_PATH, "--all"],
                cwd=ROOT_DIR,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            # MUST fail closed with nonzero exit code and report integrity check failure
            self.assertNotEqual(
                res.returncode,
                0,
                "--all mode unexpectedly succeeded with corrupted dataset!",
            )
            self.assertTrue(
                "integrity" in res.stderr.lower()
                or "checksum mismatch" in res.stderr.lower()
                or "error" in res.stderr.lower(),
                f"Expected checksum error missing from output:\nstdout: {res.stdout}\nstderr: {res.stderr}",
            )

    def test_invalid_mode_fails_closed(self):
        """Condition 4: Unknown execution modes fail closed."""
        res = subprocess.run(
            [RUNNER_PATH, "--nonexistent-mode"],
            cwd=ROOT_DIR,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertNotEqual(res.returncode, 0)
        self.assertIn("Unknown reproduction mode", res.stderr)


if __name__ == "__main__":
    unittest.main()
