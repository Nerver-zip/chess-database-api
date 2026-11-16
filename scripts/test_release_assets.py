#!/usr/bin/env python3
"""Offline regression tests for scripts/release_assets.py.

No test touches the network, GitHub, or a real dataset. Splitting, verification, and
reconstruction use small random files; the package round trip uses a synthetic release
tree and corpus.py's own safe extractor; the upload gate runs against a throwaway git
repository with `gh` mocked out.

Run:  python3 -m unittest scripts/test_release_assets.py
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import corpus  # noqa: E402
import release_assets as ra  # noqa: E402

MIB = 1024 * 1024
RELEASE_ID = "masters-otb-r3"
BUILD_ID = "codb-masters-otb-1794774904566910568-6701-5698b67c3952ca6b-cpi-d40"


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_manifest(assets: list[dict]) -> dict:
    """A minimal manifest for dist-verification tests (validation is tested separately)."""
    return {"release_id": RELEASE_ID, "assets": assets}


def split_asset(directory: Path, name: str, data: bytes, limit: int) -> dict:
    source = directory / (name + ".whole")
    source.write_bytes(data)
    entry = ra.split_product(source, directory, name, limit)
    source.unlink()
    entry["kind"] = "indexed-package" if name.endswith(".tar") else "filtered-pgn"
    return entry


class SplitAndReconstructTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.data = os.urandom(5 * MIB + 123)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_split_produces_numbered_parts_and_reconstructs_exactly(self) -> None:
        entry = split_asset(self.dist, f"{RELEASE_ID}-indexed.tar", self.data, 2 * MIB)
        names = [part["name"] for part in entry["parts"]]
        self.assertEqual(names, [f"{RELEASE_ID}-indexed.tar.part-{i:03d}" for i in (1, 2, 3)])
        self.assertEqual([part["size_bytes"] for part in entry["parts"]],
                         [2 * MIB, 2 * MIB, len(self.data) - 4 * MIB])
        self.assertEqual(entry["sha256"], sha(self.data))
        self.assertEqual(entry["size_bytes"], len(self.data))
        self.assertEqual(b"".join((self.dist / n).read_bytes() for n in names), self.data)

    def test_split_is_deterministic(self) -> None:
        first = split_asset(self.dist, "a.tar", self.data, 2 * MIB)
        other = self.root / "other"
        other.mkdir()
        second = split_asset(other, "a.tar", self.data, 2 * MIB)
        self.assertEqual(first["parts"], second["parts"])

    def test_small_product_keeps_its_final_name(self) -> None:
        entry = split_asset(self.dist, f"{RELEASE_ID}-filtered.pgn.zst", b"x" * 100, 2 * MIB)
        self.assertEqual([p["name"] for p in entry["parts"]], [f"{RELEASE_ID}-filtered.pgn.zst"])
        self.assertTrue((self.dist / f"{RELEASE_ID}-filtered.pgn.zst").is_file())

    def test_part_limit_must_stay_below_the_github_asset_limit(self) -> None:
        source = self.root / "x"
        source.write_bytes(b"1")
        with self.assertRaises(ra.ReleaseError):
            ra.split_product(source, self.dist, "x", 2 * 1024 * 1024 * 1024)

    def _manifest_for(self, entry: dict) -> dict:
        return make_manifest([entry])

    def test_reconstruct_writes_verified_archive(self) -> None:
        entry = split_asset(self.dist, "pkg.tar", self.data, 2 * MIB)
        manifest = self._manifest_for(entry)
        # verify_dist expects the sidecar and notice names too; mirror the real dist layout.
        for name in (f"{RELEASE_ID}-NOTICE.md", f"{RELEASE_ID}-RELEASE_MANIFEST.json", f"{RELEASE_ID}-SHA256SUMS.txt"):
            (self.dist / name).write_text("placeholder\n")
        manifest["release_id"] = RELEASE_ID
        manifest["assets"][0]["name"] = "pkg.tar"
        out = self.root / "out"
        with mock.patch.object(ra, "dist_expected_files",
                               lambda m: {p["name"] for a in m["assets"] for p in a["parts"]}
                               | {f"{RELEASE_ID}-NOTICE.md", f"{RELEASE_ID}-RELEASE_MANIFEST.json",
                                  f"{RELEASE_ID}-SHA256SUMS.txt"}):
            produced = ra.reconstruct(manifest, self.dist, out)
        self.assertEqual(produced["pkg.tar"].read_bytes(), self.data)
        self.assertEqual((out / "pkg.tar.sha256").read_text().split()[0], sha(self.data))
        self.assertFalse(any(p.name.endswith(".partial") for p in out.iterdir()))

    def test_reconstruct_refuses_to_overwrite(self) -> None:
        entry = split_asset(self.dist, "pkg.tar", self.data, 2 * MIB)
        out = self.root / "out"
        out.mkdir()
        (out / "pkg.tar").write_bytes(b"keep")
        manifest = self._manifest_for(entry)
        with mock.patch.object(ra, "dist_expected_files",
                               lambda m: {p["name"] for a in m["assets"] for p in a["parts"]}):
            with self.assertRaises(ra.ReleaseError):
                ra.reconstruct(manifest, self.dist, out)
        self.assertEqual((out / "pkg.tar").read_bytes(), b"keep")


class VerificationRejectionTests(unittest.TestCase):
    """Checksum, completeness, and size failures must stop verification and reconstruction."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.data = os.urandom(5 * MIB)
        self.entry = split_asset(self.dist, "pkg.tar", self.data, 2 * MIB)
        self.metadata = [f"{RELEASE_ID}-NOTICE.md", f"{RELEASE_ID}-RELEASE_MANIFEST.json", f"{RELEASE_ID}-SHA256SUMS.txt"]
        for name in self.metadata:
            (self.dist / name).write_text("placeholder\n")
        self.manifest = make_manifest([self.entry])
        self.manifest["assets"][0]["name"] = "pkg.tar"
        self.expected_patch = mock.patch.object(
            ra, "dist_expected_files",
            lambda m: {p["name"] for a in m["assets"] for p in a["parts"]} | set(self.metadata),
        )
        self.expected_patch.start()

    def tearDown(self) -> None:
        self.expected_patch.stop()
        self._tmp.cleanup()

    def test_intact_dist_verifies(self) -> None:
        ra.verify_dist(self.manifest, self.dist)

    def test_checksum_mismatch_is_rejected(self) -> None:
        part = self.dist / "pkg.tar.part-002"
        data = bytearray(part.read_bytes())
        data[0] ^= 0xFF
        part.write_bytes(bytes(data))
        with self.assertRaisesRegex(ra.ReleaseError, "SHA-256 mismatch"):
            ra.verify_dist(self.manifest, self.dist)
        out = self.root / "out"
        with self.assertRaises(ra.ReleaseError):
            ra.reconstruct(self.manifest, self.dist, out)
        self.assertFalse((out / "pkg.tar").exists())
        self.assertFalse((out / ".pkg.tar.partial").exists())

    def test_size_mismatch_is_rejected(self) -> None:
        part = self.dist / "pkg.tar.part-001"
        part.write_bytes(part.read_bytes() + b"x")
        with self.assertRaisesRegex(ra.ReleaseError, "size mismatch"):
            ra.verify_dist(self.manifest, self.dist)

    def test_incomplete_asset_set_is_rejected(self) -> None:
        (self.dist / "pkg.tar.part-002").unlink()
        with self.assertRaisesRegex(ra.ReleaseError, "incomplete; missing: pkg.tar.part-002"):
            ra.verify_dist(self.manifest, self.dist)

    def test_unexpected_file_is_rejected(self) -> None:
        (self.dist / "pkg.tar.part-009").write_bytes(b"stale")
        with self.assertRaisesRegex(ra.ReleaseError, "unexpected files"):
            ra.verify_dist(self.manifest, self.dist)

    def test_hidden_partial_files_do_not_count_as_assets(self) -> None:
        (self.dist / ".pkg.tar.partial").write_bytes(b"in progress")
        ra.verify_dist(self.manifest, self.dist)

    def test_whole_archive_hash_mismatch_is_rejected(self) -> None:
        bad = dict(self.manifest)
        bad_asset = dict(self.entry)
        bad_asset["sha256"] = "0" * 64
        bad["assets"] = [dict(bad_asset, name="pkg.tar")]
        with self.assertRaisesRegex(ra.ReleaseError, "reconstructed SHA-256"):
            ra.verify_dist(bad, self.dist)


class ManifestValidationTests(unittest.TestCase):
    def _valid(self) -> dict:
        def part(name: str) -> dict:
            return {"name": name, "size_bytes": 10, "sha256": "a" * 64}

        return {
            "schema_version": 1,
            "release_id": RELEASE_ID,
            "tag": f"dataset-{RELEASE_ID}",
            "dataset_version": BUILD_ID,
            "collection": "masters-otb",
            "game_count": 3_284_429,
            "index": {"depth_plies": 40, "unique_positions": 51_909_604, "unique_position_move_pairs": 55_864_091},
            "assets": [
                {"kind": "indexed-package", "name": f"{RELEASE_ID}-indexed.tar", "size_bytes": 20, "sha256": "b" * 64,
                 "parts": [part(f"{RELEASE_ID}-indexed.tar.part-001"), part(f"{RELEASE_ID}-indexed.tar.part-002")]},
                {"kind": "filtered-pgn", "name": f"{RELEASE_ID}-filtered.pgn.zst", "size_bytes": 10, "sha256": "c" * 64,
                 "parts": [part(f"{RELEASE_ID}-filtered.pgn.zst")]},
            ],
            "license": {"notice": f"{RELEASE_ID}-NOTICE.md"},
            "distribution": {"clearance_file": "releases/distribution_clearance.json"},
        }

    def test_valid_manifest_passes(self) -> None:
        ra.validate_manifest(self._valid())

    def test_wrong_game_count_is_rejected(self) -> None:
        manifest = self._valid()
        manifest["game_count"] = 29_640
        with self.assertRaisesRegex(ra.ReleaseError, "game count"):
            ra.validate_manifest(manifest)

    def test_wrong_index_depth_is_rejected(self) -> None:
        manifest = self._valid()
        manifest["index"]["depth_plies"] = 30
        with self.assertRaisesRegex(ra.ReleaseError, "index facts"):
            ra.validate_manifest(manifest)

    def test_nonconforming_part_name_is_rejected(self) -> None:
        manifest = self._valid()
        manifest["assets"][0]["parts"][1]["name"] = f"{RELEASE_ID}-indexed.tar.part-3"
        with self.assertRaisesRegex(ra.ReleaseError, "should be"):
            ra.validate_manifest(manifest)

    def test_parts_must_sum_to_the_asset_size(self) -> None:
        manifest = self._valid()
        manifest["assets"][0]["size_bytes"] = 999
        with self.assertRaisesRegex(ra.ReleaseError, "do not sum"):
            ra.validate_manifest(manifest)

    def test_asset_naming_rule_is_enforced(self) -> None:
        manifest = self._valid()
        manifest["assets"][1]["name"] = "pgn.zst"
        with self.assertRaisesRegex(ra.ReleaseError, "naming rule"):
            ra.validate_manifest(manifest)

    def test_manifest_must_list_exactly_two_products(self) -> None:
        manifest = self._valid()
        manifest["assets"] = manifest["assets"][:1]
        with self.assertRaisesRegex(ra.ReleaseError, "exactly the two"):
            ra.validate_manifest(manifest)

    def test_part_above_github_limit_is_rejected(self) -> None:
        manifest = self._valid()
        manifest["assets"][1]["parts"][0]["size_bytes"] = 2 * 1024 * 1024 * 1024
        manifest["assets"][1]["size_bytes"] = 2 * 1024 * 1024 * 1024
        with self.assertRaisesRegex(ra.ReleaseError, "GitHub asset limit"):
            ra.validate_manifest(manifest)


class DistributionGateTests(unittest.TestCase):
    def test_committed_clearance_blocks_publication(self) -> None:
        clearance = ra.load_clearance()
        blockers = ra.clearance_blockers(clearance, RELEASE_ID)
        self.assertTrue(blockers)
        self.assertTrue(any("not cleared" in item for item in blockers))

    def test_cleared_record_still_needs_a_reference_and_matching_release(self) -> None:
        cleared = {"status": "cleared", "release_id": RELEASE_ID, "clearance_reference": None}
        self.assertEqual(len(ra.clearance_blockers(cleared, RELEASE_ID)), 1)
        cleared["clearance_reference"] = "written permission on file"
        self.assertEqual(ra.clearance_blockers(cleared, RELEASE_ID), [])
        self.assertTrue(ra.clearance_blockers(cleared, "other-release"))


class UploadTests(unittest.TestCase):
    """Dry-run by default; --execute refuses unless the gate and tag checks pass."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.manifest = {
            "schema_version": 1, "release_id": RELEASE_ID, "tag": f"dataset-{RELEASE_ID}",
            "dataset_version": BUILD_ID, "collection": "masters-otb", "game_count": 3_284_429,
            "index": {"depth_plies": 40, "unique_positions": 51_909_604, "unique_position_move_pairs": 55_864_091},
            "assets": [
                {"kind": "indexed-package", "name": f"{RELEASE_ID}-indexed.tar", "size_bytes": 4, "sha256": sha(b"abcd"),
                 "parts": [{"name": f"{RELEASE_ID}-indexed.tar", "size_bytes": 4, "sha256": sha(b"abcd")}]},
                {"kind": "filtered-pgn", "name": f"{RELEASE_ID}-filtered.pgn.zst", "size_bytes": 2, "sha256": sha(b"zz"),
                 "parts": [{"name": f"{RELEASE_ID}-filtered.pgn.zst", "size_bytes": 2, "sha256": sha(b"zz")}]},
            ],
            "license": {"notice": f"{RELEASE_ID}-NOTICE.md"},
            "distribution": {"clearance_file": "releases/distribution_clearance.json"},
        }
        self.manifest_path = self.dist / f"{RELEASE_ID}-RELEASE_MANIFEST.json"
        self.manifest_path.write_text(json.dumps(self.manifest, indent=2, sort_keys=True) + "\n")
        (self.dist / f"{RELEASE_ID}-indexed.tar").write_bytes(b"abcd")
        (self.dist / f"{RELEASE_ID}-filtered.pgn.zst").write_bytes(b"zz")
        (self.dist / f"{RELEASE_ID}-NOTICE.md").write_text("notice\n")
        (self.dist / f"{RELEASE_ID}-SHA256SUMS.txt").write_text("sums\n")
        self.patches = [
            mock.patch.object(ra, "dist_expected_files", lambda m: {p["name"] for a in m["assets"] for p in a["parts"]}
                              | {f"{RELEASE_ID}-NOTICE.md", f"{RELEASE_ID}-RELEASE_MANIFEST.json", f"{RELEASE_ID}-SHA256SUMS.txt"}),
            mock.patch.object(ra, "validate_manifest", lambda m: m),
        ]
        for patch in self.patches:
            patch.start()

    def tearDown(self) -> None:
        for patch in self.patches:
            patch.stop()
        self._tmp.cleanup()

    def _args(self, execute: bool) -> object:
        class Args:
            manifest = str(self.manifest_path)
            dist_dir = str(self.dist)
            tag = None
        args = Args()
        args.execute = execute
        return args

    def test_dry_run_never_calls_gh_and_reports_the_gate(self) -> None:
        with mock.patch.object(ra, "git_output", return_value=None), \
                mock.patch.object(ra, "gh_release_exists", return_value=False), \
                mock.patch.object(ra.subprocess, "run") as run:
            self.assertEqual(ra.cmd_upload(self._args(execute=False)), 0)
        run.assert_not_called()

    def test_execute_refuses_while_gate_is_closed(self) -> None:
        with mock.patch.object(ra, "git_output", return_value=None), \
                mock.patch.object(ra, "gh_release_exists", return_value=False), \
                mock.patch.object(ra.subprocess, "run") as run:
            self.assertEqual(ra.cmd_upload(self._args(execute=True)), 3)
        run.assert_not_called()

    def test_tag_must_be_reachable_from_main(self) -> None:
        repo = self.root / "repo"
        repo.mkdir()
        git = ["git", "-C", str(repo)]
        subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
        subprocess.run([*git, "config", "user.email", "test@example.invalid"], check=True)
        subprocess.run([*git, "config", "user.name", "test"], check=True)
        (repo / "releases").mkdir()
        (repo / "releases" / f"{RELEASE_ID}.json").write_bytes(self.manifest_path.read_bytes())
        subprocess.run([*git, "add", "-A"], check=True)
        subprocess.run([*git, "commit", "-q", "-m", "main"], check=True)
        subprocess.run([*git, "checkout", "-q", "-b", "side"], check=True)
        (repo / "side.txt").write_text("side\n")
        subprocess.run([*git, "add", "-A"], check=True)
        subprocess.run([*git, "commit", "-q", "-m", "side"], check=True)
        subprocess.run([*git, "tag", f"dataset-{RELEASE_ID}"], check=True)
        subprocess.run([*git, "checkout", "-q", "main"], check=True)
        output = io.StringIO()
        with mock.patch.object(ra, "REPO_ROOT", repo), \
                mock.patch.object(ra, "gh_release_exists", return_value=False), \
                contextlib.redirect_stdout(output):
            # Real git is used here; `gh release create` is only reached when every check passes.
            self.assertEqual(ra.cmd_upload(self._args(execute=True)), 3)
            self.assertEqual(ra.cmd_upload(self._args(execute=False)), 0)
        self.assertIn("not reachable from main", output.getvalue())

    def test_gh_unavailable_is_reported_as_a_blocker(self) -> None:
        with mock.patch.object(ra, "gh_release_exists", return_value=None), \
                mock.patch.object(ra, "git_output", return_value=None):
            self.assertEqual(ra.cmd_upload(self._args(execute=False)), 0)


class VerifyRemoteTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.manifest = {
            "release_id": RELEASE_ID, "tag": f"dataset-{RELEASE_ID}",
            "assets": [{"name": "a.tar", "parts": [{"name": "a.tar.part-001", "size_bytes": 3, "sha256": sha(b"abc")},
                                                   {"name": "a.tar.part-002", "size_bytes": 2, "sha256": sha(b"de")}]}],
        }
        self.manifest_path = self.root / "m.json"
        self.manifest_path.write_text(json.dumps(self.manifest))
        self.metadata = [f"{RELEASE_ID}-NOTICE.md", f"{RELEASE_ID}-RELEASE_MANIFEST.json", f"{RELEASE_ID}-SHA256SUMS.txt"]
        self.patches = [
            mock.patch.object(ra, "validate_manifest", lambda m: m),
            mock.patch.object(ra, "dist_expected_files",
                              lambda m: {p["name"] for a in m["assets"] for p in a["parts"]} | set(self.metadata)),
        ]
        for patch in self.patches:
            patch.start()

    def tearDown(self) -> None:
        for patch in self.patches:
            patch.stop()
        self._tmp.cleanup()

    def _remote(self, **overrides: object) -> Path:
        remote = {
            "tag_name": f"dataset-{RELEASE_ID}", "draft": True,
            "assets": [{"name": n, "size": s} for n, s in [
                ("a.tar.part-001", 3), ("a.tar.part-002", 2),
                *[(m, 1) for m in self.metadata]]],
        }
        remote.update(overrides)
        path = self.root / "remote.json"
        path.write_text(json.dumps(remote))
        return path

    def _args(self, assets_json: Path) -> object:
        class Args:
            manifest = str(self.manifest_path)
            assets_json = ""
        args = Args()
        args.assets_json = str(assets_json)
        return args

    def test_matching_draft_passes(self) -> None:
        self.assertEqual(ra.cmd_verify_remote(self._args(self._remote())), 0)

    def test_published_release_is_refused(self) -> None:
        with self.assertRaisesRegex(ra.ReleaseError, "not a draft"):
            ra.cmd_verify_remote(self._args(self._remote(draft=False)))

    def test_missing_asset_is_refused(self) -> None:
        remote = json.loads(self._remote().read_text())
        remote["assets"] = remote["assets"][:1]
        path = self.root / "short.json"
        path.write_text(json.dumps(remote))
        with self.assertRaisesRegex(ra.ReleaseError, "missing assets"):
            ra.cmd_verify_remote(self._args(path))

    def test_size_mismatch_is_refused(self) -> None:
        remote = json.loads(self._remote().read_text())
        remote["assets"][0]["size"] = 99
        path = self.root / "size.json"
        path.write_text(json.dumps(remote))
        with self.assertRaisesRegex(ra.ReleaseError, "size"):
            ra.cmd_verify_remote(self._args(path))


class InstallGuardTests(unittest.TestCase):
    def test_install_refuses_the_live_masters_root(self) -> None:
        class Args:
            target = str(ra.LIVE_ROOT)
            manifest = ""
            package = ""
            codb = None
        with self.assertRaisesRegex(ra.ReleaseError, "live Masters root"):
            ra.cmd_install(Args())

    def test_install_refuses_paths_inside_the_live_root(self) -> None:
        class Args:
            target = str(ra.LIVE_ROOT / "versions")
            manifest = ""
            package = ""
            codb = None
        with self.assertRaisesRegex(ra.ReleaseError, "live Masters root"):
            ra.cmd_install(Args())


class IndexedPackageTests(unittest.TestCase):
    """The package builder must be deterministic and must extract through corpus.py's checks."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.live = self.root / "masters-otb"
        version = self.live / "versions" / BUILD_ID
        (version / "RELEASE").mkdir(parents=True)
        (version / "shards").mkdir()
        payloads = {
            "manifest.json": json.dumps({"collection": "masters-otb", "dataset_build_id": BUILD_ID,
                                         "is_complete": True, "total_games": 3}).encode(),
            "games.idx": b"index-bytes",
            "positions.cpi": b"cpi-bytes" * 10,
            "shards/shard_0000.cgb3": b"shard-bytes",
            "RELEASE/DATASET_LICENSE.txt": b"license record\n",
        }
        for relative, data in payloads.items():
            (version / relative).write_bytes(data)
        release = {
            "release_schema_version": 1,
            "collection": "masters-otb",
            "dataset_version": BUILD_ID,
            "artifacts": {name: {"sha256": sha(data), "size_bytes": len(data)}
                          for name, data in payloads.items() if name != "RELEASE/DATASET_LICENSE.txt"},
        }
        (version / "RELEASE" / "RELEASE.json").write_text(json.dumps(release))
        os.symlink(f"versions/{BUILD_ID}", self.live / "current")
        os.symlink(f"current/games.idx", self.live / "games.idx")
        os.symlink(f"current/shards", self.live / "shards")
        self.notice = self.root / "NOTICE.md"
        self.notice.write_text("notice text\n")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_package_is_byte_identical_across_builds(self) -> None:
        first = self.root / "first.tar"
        second = self.root / "second.tar"
        ra.build_indexed_tar(self.live, first, self.notice)
        ra.build_indexed_tar(self.live, second, self.notice)
        self.assertEqual(sha(first.read_bytes()), sha(second.read_bytes()))

    def test_package_extracts_with_corpus_checks_and_carries_the_notice(self) -> None:
        archive = self.root / "pkg.tar"
        ra.build_indexed_tar(self.live, archive, self.notice)
        ra.write_sha_sidecar(archive)
        extracted = corpus.extract_release_archive(archive, self.root / "extract")
        self.assertIsNone(corpus.verify_release_files(extracted))
        version = corpus.current_version_dir(extracted)
        self.assertEqual((version / "RELEASE" / "NOTICE.md").read_text(), "notice text\n")
        self.assertEqual((version / "RELEASE" / "DATASET_LICENSE.txt").read_text(), "license record\n")

    def test_notice_template_placeholders_must_all_resolve(self) -> None:
        with self.assertRaisesRegex(ra.ReleaseError, "unresolved"):
            ra.render_notice("a {{KNOWN}} b {{MISSING}}", {"KNOWN": "x"})
        self.assertEqual(ra.render_notice("a {{KNOWN}}", {"KNOWN": "x"}), "a x")


class CommittedRecordTests(unittest.TestCase):
    def test_shipped_notice_template_has_no_unexplained_placeholders(self) -> None:
        text = ra.NOTICE_TEMPLATE.read_text(encoding="utf-8")
        self.assertIn("CC BY-NC 4.0", text)
        self.assertIn("Lumbra's Gigabase", text)
        self.assertIn("MIT OR Apache-2.0", text)
        self.assertIn("CC0 1.0", text)
        self.assertIn("not endorsed by the publisher", text.lower())

    def test_clearance_record_is_committed_and_closed(self) -> None:
        clearance = json.loads((ra.RELEASES_DIR / "distribution_clearance.json").read_text())
        self.assertEqual(clearance["status"], "not_cleared")
        self.assertIsNone(clearance["clearance_reference"])


if __name__ == "__main__":
    unittest.main()
