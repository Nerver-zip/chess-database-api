#!/usr/bin/env python3
"""Offline tests for the local-only Gigabase evaluation workflow."""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import local_gigabase_eval as evaluation  # noqa: E402


REPO = Path(__file__).resolve().parent.parent


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


class LocalGigabaseEvaluationTests(unittest.TestCase):
    def setUp(self):
        self.source_root = REPO / "data" / "sources"
        self.source_root.mkdir(parents=True, exist_ok=True)
        self.eval_root = evaluation.DEFAULT_WORK_ROOT
        self.eval_root.mkdir(parents=True, exist_ok=True)
        self.source_temp = tempfile.TemporaryDirectory(prefix="gigabase-test-", dir=self.source_root)
        self.eval_temp = tempfile.TemporaryDirectory(prefix="gigabase-test-", dir=self.eval_root)
        self.source_dir = Path(self.source_temp.name)
        self.eval_dir = Path(self.eval_temp.name)

    def tearDown(self):
        self.source_temp.cleanup()
        self.eval_temp.cleanup()

    def test_register_pins_only_local_hashes_and_preserves_rights_status(self):
        archive = self.source_dir / "local.7z"
        pgn = self.source_dir / "local.pgn"
        archive.write_bytes(b"local archive fixture")
        pgn.write_bytes(b"[Event \"synthetic\"]\n\n1. e4 e5 1/2-1/2\n")
        evidence = self.source_dir / "local-source.json"
        evidence.write_text(json.dumps({
            "source_kind": "user_supplied_local_archive",
            "archive_sha256_local": digest(archive),
            "archive_size_bytes": archive.stat().st_size,
            "publisher_checksum": None,
            "publisher_checksum_verified": False,
            "member_filename": pgn.name,
            "member_sha256_local": digest(pgn),
            "member_size_bytes": pgn.stat().st_size,
            "extraction_tool": "synthetic test fixture",
            "release_eligible": False,
        }))
        registration_path = self.eval_dir / "registration.json"

        registration = evaluation.register_source(
            archive, pgn, evidence, registration_path
        )
        loaded, resolved_pgn = evaluation.load_registration(registration_path)

        self.assertIsNone(registration["publisher_checksum"])
        self.assertFalse(registration["publisher_checksum_verified"])
        self.assertFalse(registration["release_eligible"])
        self.assertEqual(loaded["rights_clearance"], "unresolved_upstream_rights")
        self.assertEqual(loaded["archive_sha256_local"], digest(archive))
        self.assertEqual(resolved_pgn, pgn.resolve())

        for field, value, expected in (
            ("publisher_checksum", "b" * 64, "publisher checksum must remain absent"),
            ("publisher_checksum_verified", True, "publisher checksum must remain absent"),
            ("publisher_license_claim", "CC0 1.0", "publisher license claim differs"),
        ):
            with self.subTest(field=field):
                registration_path.write_text(json.dumps({**registration, field: value}))
                with self.assertRaisesRegex(evaluation.EvaluationError, expected):
                    evaluation.load_registration(registration_path)

    def test_registration_rejects_changed_source_bytes(self):
        archive = self.source_dir / "local.7z"
        pgn = self.source_dir / "local.pgn"
        archive.write_bytes(b"local archive fixture")
        pgn.write_bytes(b"synthetic PGN")
        evidence = self.source_dir / "local-source.json"
        evidence.write_text(json.dumps({
            "source_kind": "user_supplied_local_archive",
            "archive_sha256_local": digest(archive),
            "archive_size_bytes": archive.stat().st_size,
            "publisher_checksum": None,
            "publisher_checksum_verified": False,
            "member_filename": pgn.name,
            "member_sha256_local": digest(pgn),
            "member_size_bytes": pgn.stat().st_size,
            "release_eligible": False,
        }))
        registration_path = self.eval_dir / "registration.json"
        evaluation.register_source(archive, pgn, evidence, registration_path)
        pgn.write_bytes(b"changed PGN")

        with self.assertRaisesRegex(evaluation.EvaluationError, "PGN member size differs"):
            evaluation.load_registration(registration_path)

    def test_build_manifest_pins_local_hash_and_labels_publisher_license_claim(self):
        registration = {
            "member_sha256_local": "a" * 64,
            "publisher_license_claim": "CC BY-NC 4.0",
            "publisher_checksum": None,
            "publisher_checksum_verified": False,
            "release_eligible": False,
        }
        pgn = self.source_dir / "local.pgn"

        manifest = evaluation._local_source_manifest(registration, pgn)

        self.assertEqual(manifest["collection"], "masters-otb")
        self.assertEqual(manifest["sources"], [{
            "path": str(pgn),
            "url": None,
            "sha256": "a" * 64,
            "license": "Publisher claim: CC BY-NC 4.0; upstream rights unresolved",
        }])

        for field, value in (
            ("publisher_checksum", "b" * 64),
            ("publisher_checksum_verified", True),
            ("release_eligible", True),
        ):
            with self.subTest(field=field):
                invalid_registration = {**registration, field: value}
                with self.assertRaisesRegex(
                    evaluation.EvaluationError,
                    "local source manifest requires absent publisher checksum",
                ):
                    evaluation._local_source_manifest(invalid_registration, pgn)

    def test_build_source_metadata_must_match_registered_local_bytes(self):
        registration = {
            "member_filename": "local.pgn",
            "member_sha256_local": "a" * 64,
            "member_size_bytes": 123,
            "publisher_license_claim": "CC BY-NC 4.0",
        }
        source = {
            "filename": "local.pgn",
            "url": None,
            "sha256": "a" * 64,
            "size_bytes": 123,
            "license": "Publisher claim: CC BY-NC 4.0; upstream rights unresolved",
            "checksum_verified": True,
        }

        evaluation._validate_source_archive_metadata([source], registration, "test manifest")

        for invalid_sources in ([], ["not an object"], [source, source]):
            with self.subTest(invalid_sources=invalid_sources):
                with self.assertRaises(evaluation.EvaluationError):
                    evaluation._validate_source_archive_metadata(
                        invalid_sources, registration, "test manifest"
                    )

        for field, changed in (
            ("url", "https://example.invalid/source.pgn"),
            ("sha256", "b" * 64),
            ("size_bytes", 124),
            ("license", "CC BY-NC 4.0"),
            ("checksum_verified", False),
        ):
            invalid = {**source, field: changed}
            with self.subTest(field=field):
                with self.assertRaisesRegex(
                    evaluation.EvaluationError,
                    "source archive metadata differs from the registered local source",
                ):
                    evaluation._validate_source_archive_metadata(
                        [invalid], registration, "test manifest"
                    )

    def test_write_targets_are_confined_to_ignored_evaluation_workspace(self):
        with self.assertRaisesRegex(evaluation.EvaluationError, "must stay under"):
            evaluation._work_path(REPO / "data" / "releases" / "masters-otb", "target")

    def test_source_attribution_summary_counts_each_game_and_source(self):
        snapshot = self.eval_dir / "snapshot"
        snapshot.mkdir()
        sidecar = snapshot / "source-attribution.jsonl"
        sidecar.write_text(
            '{"game_id":1,"source_attributions":['
            '{"source_tag":"Alpha","mode":null},'
            '{"source_tag":"Alpha","mode":null},'
            '{"source_tag":"Beta","mode":null}]}\n'
            '{"game_id":2,"source_attributions":['
            '{"source_tag":"Alpha","mode":"OTB"}]}\n',
            encoding="utf-8",
        )
        summary = evaluation._source_attribution_counts(snapshot, {
            "source_attribution": {
                "filename": sidecar.name,
                "sha256": digest(sidecar),
            }
        })

        self.assertEqual(summary["source_tag_counts"], {"Alpha": 2, "Beta": 1})
        self.assertEqual(summary["mode_provenance_counts"], {
            "explicit_otb": 1,
            "missing_mode": 1,
        })

    def test_notice_keeps_publisher_claim_distinct_from_rights_clearance(self):
        notice = evaluation._license_notice({
            "archive_filename": "fixture.7z",
            "archive_size_bytes": 10,
            "archive_sha256_local": "a" * 64,
            "member_filename": "fixture.pgn",
            "member_size_bytes": 20,
            "member_sha256_local": "b" * 64,
            "full_decompression_check": "passed",
            "local_extraction_tool": None,
            "packed_stream_crc_metadata_available": False,
        })
        self.assertIn("CC BY-NC 4.0", notice)
        self.assertIn("Local archive SHA-256", notice)
        self.assertIn("not independent rights clearance", notice)
        self.assertIn("Do not redistribute it", notice)


if __name__ == "__main__":
    unittest.main()
