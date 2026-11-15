#!/usr/bin/env python3
"""Offline regression tests for scripts/corpus.py.

No test touches the real network: archives are served from a local HTTP server
with fault injection, and corpus-assembly tests build tiny synthetic fixtures
(fixtures/corpus/*.pgn) through the real `codb` binary.

Run:  python3 -m unittest scripts/test_corpus.py
"""

from __future__ import annotations

import hashlib
import http.server
import io
import json
import os
import shutil
import socketserver
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
from unittest import mock
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))
os.environ["CODB_CORPUS_ALLOW_HTTP"] = "1"  # local test server only

import corpus  # noqa: E402

FIXTURES = REPO / "fixtures" / "corpus"


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# --------------------------------------------------------------------------- #
# Fault-injecting local HTTP server
# --------------------------------------------------------------------------- #
class ArchiveServer:
    """Serves `files` (path -> bytes) with per-path fault modes."""

    def __init__(self, files: dict[str, bytes]) -> None:
        self.files = files
        self.modes: dict[str, str] = {}
        self.fail_first: dict[str, int] = {}
        self.requests: list[tuple[str, str | None]] = []
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):  # silence
                pass

            def do_GET(self):
                path = self.path
                rng = self.headers.get("Range")
                outer.requests.append((path, rng))
                mode = outer.modes.get(path, "ok")
                if path not in outer.files or mode == "404":
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                if outer.fail_first.get(path, 0) > 0:
                    outer.fail_first[path] -= 1
                    self.send_response(503)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                data = outer.files[path]
                start = 0
                status = 200
                if rng and mode != "ignore-range":
                    start = int(rng.split("=")[1].split("-")[0])
                    status = 206
                body = data[start:]
                if mode == "wrong-length":
                    body = body[:-5]
                    self.send_response(status)
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                if mode == "truncate-once":
                    outer.modes[path] = "ok"
                    self.send_response(status)
                    if status == 206:
                        self.send_header("Content-Range", f"bytes {start}-{len(data) - 1}/{len(data)}")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body[: len(body) // 2])
                    self.wfile.flush()
                    self.close_connection = True
                    return
                if mode == "always-truncate":
                    self.send_response(status)
                    if status == 206:
                        self.send_header("Content-Range", f"bytes {start}-{len(data) - 1}/{len(data)}")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body[: max(1, len(body) // 3)])
                    self.wfile.flush()
                    self.close_connection = True
                    return
                self.send_response(status)
                if status == 206:
                    self.send_header("Content-Range", f"bytes {start}-{len(data) - 1}/{len(data)}")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
            daemon_threads = True

        self.httpd = Server(("127.0.0.1", 0), Handler)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def url(self, name: str) -> str:
        return f"http://127.0.0.1:{self.port}/{name}"

    def close(self) -> None:
        self.httpd.shutdown()
        self.httpd.server_close()


def make_catalog(entries: list[dict], profiles: dict[str, dict[str, list[str]]] | None = None) -> dict:
    """Minimal valid catalog. entries: dicts with id, collection, filename, url, data."""
    sources = []
    for e in entries:
        sources.append({
            "id": e["id"], "collection": e["collection"], "publisher": "test",
            "url": e["url"], "filename": e["filename"], "period": e.get("period", "2026-01"),
            "license": e.get("license", "CC0-1.0" if e["collection"] == "elite-online" else "CC-BY-SA-4.0"),
            "expected_size_bytes": e.get("size", len(e["data"])),
            "publisher_sha256": e.get("sha", sha(e["data"])),
            "publisher_checksum_url": "https://example.invalid/sha256sums.txt",
            "verified_sha256": None, "acquired_at": None,
            "provenance_confidence": "high", "eligibility": "test", "notes": "test",
        })
    base = {
        "schema_version": 1, "catalog_id": "test-catalog",
        "licenses": {
            "CC-BY-SA-4.0": {"name": "x", "url": "https://x", "approved": True,
                              "attribution_required": True, "share_alike": True},
            "CC0-1.0": {"name": "y", "url": "https://y", "approved": True,
                        "attribution_required": False, "share_alike": False},
            "PROPRIETARY": {"name": "z", "url": "https://z", "approved": False,
                            "attribution_required": True, "share_alike": False},
        },
        "collections": {
            "masters-otb": {"rating_system": "FIDE Elo", "default_depth_plies": 40,
                            "allowed_licenses": ["CC-BY-SA-4.0"], "curation_policy_version": "t",
                            "profiles": {"baseline": [s["id"] for s in sources if s["collection"] == "masters-otb"]}},
            "elite-online": {"rating_system": "Glicko-2", "default_depth_plies": 30,
                             "allowed_licenses": ["CC0-1.0"], "curation_policy_version": "t",
                             "profiles": {"baseline": [s["id"] for s in sources if s["collection"] == "elite-online"]}},
        },
        "sources": sources,
    }
    if profiles:
        for c, p in profiles.items():
            base["collections"][c]["profiles"] = p
    return base


# --------------------------------------------------------------------------- #
# Catalog validation
# --------------------------------------------------------------------------- #
class ResourceMeasurementTests(unittest.TestCase):
    @unittest.skipUnless(Path("/proc/self/status").is_file() and shutil.which("sleep"),
                         "Linux procfs and sleep are required")
    def test_run_measured_reports_child_rss_without_python_parent(self):
        with tempfile.TemporaryDirectory() as td:
            sleep = shutil.which("sleep")
            assert sleep is not None
            result = corpus.run_measured(
                [sleep, "0.15"], Path(td) / "sleep.log", {}, []
            )
        self.assertEqual(result["returncode"], 0)
        self.assertGreater(result["peak_rss_bytes"], 0)
        self.assertLess(result["peak_rss_bytes"], 12 * 1024 * 1024)


    @unittest.skipUnless(shutil.which("true"), "the true command is required")
    def test_wait4_rss_fallback_for_short_lived_child(self):
        with tempfile.TemporaryDirectory() as td:
            true = shutil.which("true")
            assert true is not None
            with mock.patch.object(corpus, "process_peak_rss_bytes", return_value=0):
                result = corpus.run_measured(
                    [true], Path(td) / "true.log", {}, []
                )
        self.assertEqual(result["returncode"], 0)
        self.assertGreater(result["peak_rss_bytes"], 0)


class CatalogTests(unittest.TestCase):
    def entry(self, **kw):
        data = b"payload"
        e = {"id": "a", "collection": "elite-online", "filename": "a.pgn.zst",
             "url": "http://127.0.0.1:1/a.pgn.zst", "data": data}
        e.update(kw)
        return e

    def test_committed_catalog_is_valid(self):
        cat = corpus.load_catalog(corpus.DEFAULT_CATALOG)
        for c in corpus.COLLECTIONS:
            srcs = corpus.select_sources(cat, c, "baseline")
            self.assertTrue(srcs, f"baseline profile of {c} is empty")
            for s in srcs:
                self.assertEqual(s["collection"], c)
                self.assertTrue(s["url"].startswith("https://database.lichess.org/"))
                self.assertRegex(s["publisher_sha256"], r"^[0-9a-f]{64}$")

    def test_valid_catalog(self):
        corpus.validate_catalog(make_catalog([self.entry()]))

    def test_missing_checksum_rejected(self):
        cat = make_catalog([self.entry()])
        del cat["sources"][0]["publisher_sha256"]
        with self.assertRaises(corpus.CorpusError):
            corpus.validate_catalog(cat)
        cat = make_catalog([self.entry(sha="")])
        with self.assertRaisesRegex(corpus.CorpusError, "publisher_sha256"):
            corpus.validate_catalog(cat)

    def test_malformed_checksum_rejected(self):
        with self.assertRaises(corpus.CorpusError):
            corpus.validate_catalog(make_catalog([self.entry(sha="XYZ")]))

    def test_unapproved_license_rejected(self):
        with self.assertRaisesRegex(corpus.CorpusError, "not approved"):
            corpus.validate_catalog(make_catalog([self.entry(license="PROPRIETARY")]))

    def test_license_not_allowed_for_collection(self):
        # CC0 source placed in masters-otb (which only allows CC BY-SA)
        with self.assertRaisesRegex(corpus.CorpusError, "not allowed for collection"):
            corpus.validate_catalog(make_catalog([self.entry(collection="masters-otb", license="CC0-1.0")]))

    def test_unknown_license_rejected(self):
        with self.assertRaisesRegex(corpus.CorpusError, "unknown license"):
            corpus.validate_catalog(make_catalog([self.entry(license="MIT")]))

    def test_duplicate_ids_rejected(self):
        cat = make_catalog([self.entry(), self.entry(filename="b.pgn.zst", url="http://127.0.0.1:1/b.pgn.zst")])
        with self.assertRaisesRegex(corpus.CorpusError, "duplicate source id"):
            corpus.validate_catalog(cat)

    def test_mixed_collection_profile_rejected(self):
        cat = make_catalog(
            [self.entry(id="m", collection="masters-otb", filename="m.pgn", url="http://127.0.0.1:1/m.pgn"),
             self.entry(id="e")],
            profiles={"masters-otb": {"baseline": ["m", "e"]}},
        )
        with self.assertRaisesRegex(corpus.CorpusError, "mixes collections"):
            corpus.validate_catalog(cat)

    def test_https_required_without_test_override(self):
        old = os.environ.pop("CODB_CORPUS_ALLOW_HTTP")
        try:
            with self.assertRaisesRegex(corpus.CorpusError, "HTTPS"):
                corpus.validate_catalog(make_catalog([self.entry()]))
        finally:
            os.environ["CODB_CORPUS_ALLOW_HTTP"] = old

    def test_url_basename_must_match_filename(self):
        with self.assertRaisesRegex(corpus.CorpusError, "basename"):
            corpus.validate_catalog(make_catalog([self.entry(url="http://127.0.0.1:1/other.pgn.zst")]))

    def test_verified_hash_must_equal_publisher_hash(self):
        cat = make_catalog([self.entry()])
        cat["sources"][0]["verified_sha256"] = "0" * 64
        with self.assertRaisesRegex(corpus.CorpusError, "verified_sha256"):
            corpus.validate_catalog(cat)

    def test_invalid_json_and_schema(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "c.json"
            p.write_text("{not json")
            with self.assertRaises(corpus.CorpusError):
                corpus.load_catalog(p)
            p.write_text(json.dumps({"schema_version": 99}))
            with self.assertRaises(corpus.CorpusError):
                corpus.load_catalog(p)

    def test_deterministic_source_ordering(self):
        e1 = self.entry(id="z-late", period="2026-09", filename="z.pgn", url="http://127.0.0.1:1/z.pgn")
        e2 = self.entry(id="a-early", period="2026-01", filename="a.pgn", url="http://127.0.0.1:1/a.pgn")
        e3 = self.entry(id="b-mid", period="2026-05", filename="b.pgn", url="http://127.0.0.1:1/b.pgn")
        order1 = [s["id"] for s in corpus.select_sources(make_catalog([e1, e2, e3]), "elite-online", "baseline")]
        order2 = [s["id"] for s in corpus.select_sources(make_catalog([e3, e1, e2]), "elite-online", "baseline")]
        self.assertEqual(order1, ["a-early", "b-mid", "z-late"])
        self.assertEqual(order1, order2)

    def test_unknown_profile_rejected(self):
        with self.assertRaisesRegex(corpus.CorpusError, "no profile"):
            corpus.select_sources(make_catalog([self.entry()]), "elite-online", "nope")


class ExportParserTests(unittest.TestCase):
    def test_parse_export_blocks_is_strict(self):
        pgn = (
            '[Event "A"]\n[White "x"]\n\n1. e4 e5 2. Nf3 {a comment} Nc6 3. O-O-O+ 1-0\n\n'
            '[Event "Multi\nline value"]\n[White "y"]\n\n1. d4 d5 2. c4 e6 1/2-1/2\n\n'
            '[Event "Garbage"]\n\n1. \u00a8\u00e5\u00bd e5 *\n\n'
            '[Event "Empty"]\n[White "z"]\n'
        )
        games = corpus.parse_export_blocks(pgn)
        self.assertEqual(games[0], ["e4", "e5", "Nf3", "Nc6", "O-O-O+"])
        self.assertIn(["d4", "d5", "c4", "e6"], games)
        self.assertEqual(len(games), 2, "records with non-SAN tokens or no movetext are skipped")


class PackageExtractionTests(unittest.TestCase):
    def make_package(self, directory: Path, entries: list[tuple[str, str, bytes | str]]) -> Path:
        archive = directory / "release.tar"
        with tarfile.open(archive, "w", format=tarfile.PAX_FORMAT) as tf:
            for name, kind, payload in entries:
                member = tarfile.TarInfo(name)
                if kind == "file":
                    assert isinstance(payload, bytes)
                    member.size = len(payload)
                    member.mode = 0o7777
                    member.uid = 0
                    member.gid = 0
                    tf.addfile(member, io.BytesIO(payload))
                elif kind == "directory":
                    member.type = tarfile.DIRTYPE
                    member.mode = 0o7777
                    tf.addfile(member)
                elif kind == "symlink":
                    assert isinstance(payload, str)
                    member.type = tarfile.SYMTYPE
                    member.linkname = payload
                    tf.addfile(member)
                else:
                    raise AssertionError(f"unsupported test entry kind: {kind}")
        digest = corpus.sha256_file(archive)
        archive.with_name(archive.name + ".sha256").write_text(
            f"{digest}  {archive.name}\n"
        )
        return archive

    def test_extracts_release_and_relative_links_without_tar_filter_support(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            release = {
                "release_schema_version": 1,
                "collection": "masters-otb",
                "dataset_version": "test-v1",
            }
            archive = self.make_package(
                directory,
                [
                    ("masters-otb/current", "symlink", "versions/test-v1"),
                    ("masters-otb/versions", "directory", b""),
                    ("masters-otb/versions/test-v1/RELEASE", "directory", b""),
                    (
                        "masters-otb/versions/test-v1/RELEASE/RELEASE.json",
                        "file",
                        (json.dumps(release) + "\n").encode(),
                    ),
                    (
                        "masters-otb/versions/test-v1/RELEASE/DATASET_LICENSE.txt",
                        "file",
                        b"Test license\n",
                    ),
                ],
            )
            root = corpus.extract_release_archive(archive, directory / "extracted", "masters-otb")
            self.assertTrue((root / "current").is_symlink())
            self.assertEqual(os.readlink(root / "current"), "versions/test-v1")
            self.assertTrue((root / "current" / "RELEASE" / "RELEASE.json").is_file())
            mode = (root / "current" / "RELEASE" / "RELEASE.json").stat().st_mode & 0o777
            self.assertEqual(mode, 0o644)

    def test_rejects_symlink_as_an_archive_path_ancestor(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            archive = self.make_package(
                directory,
                [
                    ("masters-otb/current", "symlink", "versions/test-v1"),
                    ("masters-otb/current/escape.txt", "file", b"outside"),
                ],
            )
            with self.assertRaisesRegex(corpus.CorpusError, "non-directory entry"):
                corpus.extract_release_archive(archive, directory / "extracted", "masters-otb")

    def test_rejects_symlinks_that_escape_the_release_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            archive = self.make_package(
                directory,
                [("masters-otb/current", "symlink", "../../outside")],
            )
            with self.assertRaisesRegex(corpus.CorpusError, "escapes release root"):
                corpus.extract_release_archive(archive, directory / "extracted", "masters-otb")

    def test_rejects_parent_traversal_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            archive = self.make_package(
                directory,
                [("masters-otb/../outside", "file", b"outside")],
            )
            with self.assertRaisesRegex(corpus.CorpusError, "unsafe package path"):
                corpus.extract_release_archive(archive, directory / "extracted", "masters-otb")


# --------------------------------------------------------------------------- #
# Acquisition
# --------------------------------------------------------------------------- #
class FetchTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.data_dir = Path(self.tmp.name)
        self.payload = os.urandom(300_000)
        self.server = ArchiveServer({"/a.pgn.zst": self.payload})
        self.src = {
            "id": "a", "collection": "elite-online", "filename": "a.pgn.zst",
            "url": self.server.url("a.pgn.zst"), "expected_size_bytes": len(self.payload),
            "publisher_sha256": sha(self.payload),
        }
        self.opts = corpus.FetchOptions(retries=3, backoff_base=0.0, min_free_bytes=0, timeout=10)

    def tearDown(self):
        self.server.close()
        self.tmp.cleanup()

    def final(self):
        return corpus.source_path(self.data_dir, self.src)

    def test_correct_checksum(self):
        r = corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertEqual(r["action"], "downloaded")
        self.assertEqual(r["bytes_downloaded"], len(self.payload))
        self.assertEqual(self.final().read_bytes(), self.payload)
        self.assertFalse(self.final().with_name(self.final().name + ".part").exists())
        sc = corpus.read_sidecar(self.final())
        self.assertEqual(sc["sha256"], sha(self.payload))
        self.assertFalse(sc["partial"])

    def test_incorrect_checksum_fails_closed(self):
        self.src["publisher_sha256"] = "f" * 64
        with self.assertRaises(corpus.ChecksumMismatch):
            corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertFalse(self.final().exists())
        self.assertFalse(self.final().with_name(self.final().name + ".part").exists())

    def test_corrupt_bytes_with_right_length(self):
        bad = bytearray(self.payload)
        bad[100] ^= 0xFF
        self.server.files["/a.pgn.zst"] = bytes(bad)
        with self.assertRaises(corpus.ChecksumMismatch):
            corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertFalse(self.final().exists())

    def test_truncated_download_is_resumed_and_verified(self):
        self.server.modes["/a.pgn.zst"] = "truncate-once"
        r = corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertEqual(r["action"], "downloaded")
        self.assertEqual(self.final().read_bytes(), self.payload)
        ranged = [rng for _, rng in self.server.requests if rng]
        self.assertTrue(ranged, "second request must resume with a Range header")
        self.assertEqual(r["bytes_downloaded"], len(self.payload))

    def test_interrupted_transfer_exhausts_retries_and_keeps_partial(self):
        self.server.modes["/a.pgn.zst"] = "always-truncate"
        with self.assertRaisesRegex(corpus.CorpusError, "giving up"):
            corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertFalse(self.final().exists(), "an incomplete download must never be finalized")
        self.assertTrue(self.final().with_name(self.final().name + ".part").exists())
        self.assertFalse(corpus.verify_local_source(self.data_dir, self.src)[0])

    def test_retry_on_server_error(self):
        self.server.fail_first["/a.pgn.zst"] = 2
        corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertEqual(self.final().read_bytes(), self.payload)
        self.assertEqual(len(self.server.requests), 3)

    def test_retries_exhausted(self):
        self.server.fail_first["/a.pgn.zst"] = 99
        with self.assertRaisesRegex(corpus.CorpusError, "giving up"):
            corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertFalse(self.final().exists())

    def test_already_verified_is_idempotent(self):
        corpus.fetch_source(self.data_dir, self.src, self.opts)
        n = len(self.server.requests)
        r = corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertEqual(r["action"], "already-verified")
        self.assertEqual(len(self.server.requests), n, "no network request for a verified archive")

    def test_corrupt_existing_file_is_quarantined_and_refetched(self):
        corpus.fetch_source(self.data_dir, self.src, self.opts)
        data = bytearray(self.payload)
        data[5] ^= 1
        self.final().write_bytes(bytes(data))
        corpus.fetch_source(self.data_dir, self.src, corpus.FetchOptions(
            backoff_base=0, min_free_bytes=0, rehash=True))
        self.assertEqual(self.final().read_bytes(), self.payload)
        self.assertTrue(list(self.final().parent.glob("*.corrupt-*")))

    def test_unexpected_remote_response(self):
        self.server.modes["/a.pgn.zst"] = "404"
        with self.assertRaisesRegex(corpus.CorpusError, "unexpected HTTP 404"):
            corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertEqual(len(self.server.requests), 1, "client errors are not retried")

    def test_remote_size_differs_from_pinned_size(self):
        self.src["expected_size_bytes"] = len(self.payload) + 10
        with self.assertRaisesRegex(corpus.CorpusError, "Content-Length"):
            corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertFalse(self.final().exists())

    def test_server_ignoring_range_restarts_cleanly(self):
        self.server.modes["/a.pgn.zst"] = "truncate-once"
        part = self.final().with_name(self.final().name + ".part")
        part.parent.mkdir(parents=True)
        part.write_bytes(self.payload[:1000])
        self.server.modes["/a.pgn.zst"] = "ignore-range"
        corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertEqual(self.final().read_bytes(), self.payload)

    def test_offline_mode(self):
        with self.assertRaisesRegex(corpus.CorpusError, "offline"):
            corpus.fetch_source(self.data_dir, self.src, corpus.FetchOptions(offline=True, min_free_bytes=0))
        self.assertEqual(self.server.requests, [])
        corpus.fetch_source(self.data_dir, self.src, self.opts)
        n = len(self.server.requests)
        r = corpus.fetch_source(self.data_dir, self.src, corpus.FetchOptions(offline=True, min_free_bytes=0))
        self.assertEqual(r["action"], "already-verified")
        self.assertEqual(len(self.server.requests), n)

    def test_disk_space_safety_limit(self):
        opts = corpus.FetchOptions(min_free_bytes=10**15, backoff_base=0)
        with self.assertRaisesRegex(corpus.CorpusError, "insufficient disk space"):
            corpus.fetch_source(self.data_dir, self.src, opts)
        self.assertEqual(self.server.requests, [])

    def test_verify_sources_detects_truncation_and_tamper(self):
        corpus.fetch_source(self.data_dir, self.src, self.opts)
        self.assertTrue(corpus.verify_local_source(self.data_dir, self.src)[0])
        self.final().write_bytes(self.payload[:-1])
        ok, why = corpus.verify_local_source(self.data_dir, self.src)
        self.assertFalse(ok)
        self.assertIn("size mismatch", why)
        tampered = bytearray(self.payload)
        tampered[0] ^= 1
        self.final().write_bytes(bytes(tampered))
        ok, why = corpus.verify_local_source(self.data_dir, self.src)
        self.assertFalse(ok)
        self.assertIn("sha256 mismatch", why)

    def test_cli_fetch_max_download_and_offline(self):
        cat = make_catalog([{"id": "a", "collection": "elite-online", "filename": "a.pgn.zst",
                             "url": self.server.url("a.pgn.zst"), "data": self.payload}])
        cp = self.data_dir / "catalog.json"
        cp.write_text(json.dumps(cat))
        base = ["--catalog", str(cp), "--data-dir", str(self.data_dir / "d")]
        rc = corpus.main([*base, "fetch", "--collection", "elite-online", "--max-download-bytes", "10",
                          "--min-free-bytes", "0"])
        self.assertEqual(rc, 2)
        rc = corpus.main([*base, "fetch", "--collection", "elite-online", "--min-free-bytes", "0",
                          "--backoff", "0", "--update-catalog"])
        self.assertEqual(rc, 0)
        rc = corpus.main([*base, "verify-sources", "--collection", "elite-online"])
        self.assertEqual(rc, 0)
        recorded = json.loads(cp.read_text())["sources"][0]
        self.assertEqual(recorded["verified_sha256"], sha(self.payload))
        self.assertTrue(recorded["acquired_at"])
        rc = corpus.main([*base, "fetch", "--collection", "elite-online", "--offline"])
        self.assertEqual(rc, 0)


# --------------------------------------------------------------------------- #
# Corpus assembly, publication, packaging, serving
# --------------------------------------------------------------------------- #
def ensure_codb() -> Path:
    try:
        return corpus.find_codb(None)
    except corpus.CorpusError:
        env = dict(os.environ)
        cargo_bin = Path.home() / ".cargo" / "bin"
        if cargo_bin.is_dir():
            env["PATH"] = f"{cargo_bin}:{env['PATH']}"
        subprocess.run(["cargo", "build", "-q", "-p", "codb-cli"], cwd=REPO, env=env, check=True)
        return corpus.find_codb(None)


class AssemblyBase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.codb = ensure_codb()
        cls.masters_pgn = (FIXTURES / "broadcast_sample.pgn").read_bytes()
        cls.elite_pgn = (FIXTURES / "standard_sample.pgn").read_bytes()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.data_dir = self.root / "data"
        self.server = ArchiveServer({})
        self.addCleanup(self.server.close)
        self.addCleanup(self.tmp.cleanup)

    def add_archive(self, name: str, data: bytes) -> dict:
        self.server.files["/" + name] = data
        return {"filename": name, "url": self.server.url(name), "data": data}

    def write_catalog(self, entries: list[dict], profiles=None) -> Path:
        cat = make_catalog(entries, profiles)
        p = self.root / "catalog.json"
        p.write_text(json.dumps(cat))
        return p

    def run_cli(self, catalog: Path, *args: str, codb: Path | None = None) -> int:
        return corpus.main(["--catalog", str(catalog), "--data-dir", str(self.data_dir),
                            "--codb", str(codb or self.codb), *args])

    def fetch_and_build(self, catalog: Path, collection: str, codb: Path | None = None) -> int:
        rc = self.run_cli(catalog, "fetch", "--collection", collection, "--min-free-bytes", "0", "--backoff", "0")
        self.assertEqual(rc, 0)
        return self.run_cli(catalog, "build", "--collection", collection, codb=codb)

    def release(self, collection: str) -> dict:
        return json.loads((corpus.current_version_dir(self.data_dir / "releases" / collection) / "RELEASE" / "RELEASE.json").read_text())

    def export(self, collection: str) -> str:
        r = subprocess.run([str(self.codb), "export", "--dataset", str(self.data_dir / "releases" / collection)],
                           capture_output=True, text=True, check=True)
        return r.stdout


class AssemblyTests(AssemblyBase):
    def build_masters(self):
        e = {"id": "m1", "collection": "masters-otb", "period": "2026-09", **self.add_archive("m1.pgn", self.masters_pgn)}
        cat = self.write_catalog([e])
        self.assertEqual(self.fetch_and_build(cat, "masters-otb"), 0)
        return cat

    def test_masters_release_accounting_and_online_exclusion(self):
        self.build_masters()
        rel = self.release("masters-otb")
        acc = rel["accounting"]
        # Exact accounting identity.
        self.assertEqual(acc["total_encountered"], acc["accepted_unique"] + acc["confirmed_duplicates"]
                         + acc["superseded_prior"] + acc["rejected_by_policy"] + acc["rejected_variant"]
                         + acc["rejected_illegal"] + acc["rejected_malformed"] + acc["quarantined"])
        self.assertEqual(acc["total_encountered"], 33)
        self.assertEqual(acc["accepted_unique"], 16)
        self.assertEqual(rel["accepted_games"], 16)
        self.assertEqual(acc["confirmed_duplicates"], 1)
        self.assertEqual(acc["superseded_prior"], 1)
        self.assertEqual(acc["rejected_variant"], 1)
        self.assertEqual(acc["rejected_illegal"], 1)
        self.assertEqual(acc["rejected_by_policy"], 13)
        reasons = json.loads((corpus.current_version_dir(self.data_dir / "releases/masters-otb") / "RELEASE/build_report.json").read_text())[
            "policy_rejection_reasons"]
        self.assertEqual(sum(reasons.values()), acc["rejected_by_policy"])
        self.assertEqual(reasons["online_event_in_masters_source"], 4)
        self.assertEqual(reasons["historical_without_provenance"], 1)
        # OTB vs online provenance: no online-event game made it into the dataset.
        pgn = self.export("masters-otb")
        self.assertNotIn("Titled Arena Online", pgn)
        self.assertNotIn("Chess.com", pgn)
        self.assertIn("AVRO 1938", pgn)           # documented historical game kept
        self.assertNotIn("Unknown A", pgn)        # bare pre-1970 date rejected
        self.assertNotIn("Freestyle Festival", pgn)  # Chess960 rejected
        # superseded live record replaced by the finished game
        self.assertEqual(pgn.count("Live White"), 1)
        # report schema: unknown results are reported
        q = json.loads((corpus.current_version_dir(self.data_dir / "releases/masters-otb") / "RELEASE/quality_report.json").read_text())
        self.assertEqual(q["total_games"], 16)
        self.assertIn("unknown_result_games", q)

    def test_release_manifest_contents(self):
        self.build_masters()
        rel = self.release("masters-otb")
        for key in ("collection", "dataset_version", "build_configuration", "source_catalog", "inputs",
                    "curation_policy", "accepted_games", "index", "artifacts", "license", "commands", "tool"):
            self.assertIn(key, rel)
        self.assertEqual(rel["collection"], "masters-otb")
        self.assertEqual(rel["build_configuration"]["index_depth_plies"], 40)
        self.assertEqual(rel["inputs"][0]["sha256"], sha(self.masters_pgn))
        self.assertTrue(rel["license"]["share_alike"])
        self.assertRegex(rel["index"]["cpi_sha256"], r"^[0-9a-f]{64}$")
        notice = (corpus.current_version_dir(self.data_dir / "releases/masters-otb") / "RELEASE/DATASET_LICENSE.txt").read_text()
        self.assertIn("CC BY-SA 4.0", notice)
        self.assertIn("ShareAlike", notice)
        # artifacts really match
        self.assertIsNone(corpus.verify_release_files(self.data_dir / "releases" / "masters-otb"))

    def test_elite_release_default_depth_and_independent_rating_notice(self):
        e = {"id": "e1", "collection": "elite-online", **self.add_archive("e1.pgn", self.elite_pgn)}
        cat = self.write_catalog([e])
        self.assertEqual(self.fetch_and_build(cat, "elite-online"), 0)
        rel = self.release("elite-online")
        self.assertEqual(rel["build_configuration"]["index_depth_plies"], 30)
        self.assertEqual(rel["accounting"]["accepted_unique"], 18)
        self.assertEqual(rel["accounting"]["rejected_illegal"], 1)
        reasons = json.loads((corpus.current_version_dir(self.data_dir / "releases/elite-online") / "RELEASE/build_report.json").read_text())[
            "policy_rejection_reasons"]
        self.assertEqual(reasons["bullet_or_ultrabullet"], 4)
        self.assertEqual(reasons["casual_or_unrated"], 1)
        notice = (corpus.current_version_dir(self.data_dir / "releases/elite-online") / "RELEASE/DATASET_LICENSE.txt").read_text()
        self.assertIn("Glicko-2", notice)
        self.assertIn("never be pooled", notice)
        pgn = self.export("elite-online")
        self.assertNotIn("Bullet", pgn)
        self.assertNotIn("Casual", pgn)

    def test_dedupe_across_archives_and_source_order_independence(self):
        # Archive B repeats the first 3 records of archive A (cross-archive duplicates).
        records = [r for r in self.masters_pgn.decode().split("\n\n[Event ")]
        b_data = ("[Event " + "\n\n[Event ".join(
            r[len("[Event "):] if r.startswith("[Event ") else r for r in records[:3]) + "\n\n").encode()
        a = {"id": "m-a", "collection": "masters-otb", "period": "2026-01", **self.add_archive("a.pgn", self.masters_pgn)}
        b = {"id": "m-b", "collection": "masters-otb", "period": "2026-02", **self.add_archive("b.pgn", b_data)}
        cat1 = self.write_catalog([a, b])
        self.assertEqual(self.fetch_and_build(cat1, "masters-otb"), 0)
        rel1 = self.release("masters-otb")
        self.assertGreaterEqual(rel1["accounting"]["confirmed_duplicates"], 1 + 3)
        self.assertEqual(rel1["accepted_games"], 16)  # nothing new from the overlapping archive
        ids1 = corpus.read_game_ids(corpus.current_version_dir(self.data_dir / "releases/masters-otb"))
        self.assertEqual([i["id"] for i in rel1["inputs"]], ["m-a", "m-b"])
        # Same sources listed in the opposite order in the catalog: identical inputs order and game set.
        shutil.rmtree(self.data_dir / "releases")
        cat2 = self.write_catalog([b, a])
        self.assertEqual(self.run_cli(cat2, "build", "--collection", "masters-otb"), 0)
        rel2 = self.release("masters-otb")
        self.assertEqual([i["id"] for i in rel2["inputs"]], ["m-a", "m-b"])
        ids2 = corpus.read_game_ids(corpus.current_version_dir(self.data_dir / "releases/masters-otb"))
        self.assertEqual(ids1, ids2)
        self.assertEqual(rel1["accounting"], rel2["accounting"])
        self.assertEqual(rel1["index"]["cpi_sha256"], rel2["index"]["cpi_sha256"],
                         "equal logical inputs and settings must give an identical CPI")

    def test_rebuild_is_deterministic_for_substantive_data(self):
        cat = self.build_masters()
        first = self.release("masters-otb")
        ids_first = corpus.read_game_ids(corpus.current_version_dir(self.data_dir / "releases/masters-otb"))
        self.assertEqual(self.run_cli(cat, "build", "--collection", "masters-otb"), 0)
        second = self.release("masters-otb")
        self.assertEqual(first["index"]["cpi_sha256"], second["index"]["cpi_sha256"])
        self.assertEqual(ids_first, corpus.read_game_ids(
            corpus.current_version_dir(self.data_dir / "releases/masters-otb")))
        # Build IDs/timestamps legitimately differ between builds.
        self.assertNotEqual(first["dataset_version"], second["dataset_version"])

    def test_mixed_collection_source_rejected_at_build(self):
        e = {"id": "e1", "collection": "elite-online", **self.add_archive("e1.pgn", self.elite_pgn)}
        cat = self.write_catalog([e], profiles={"masters-otb": {"baseline": ["e1"]}})
        # load_catalog itself refuses the mixed profile
        self.assertEqual(self.run_cli(cat, "fetch", "--collection", "masters-otb"), 2)
        self.assertEqual(self.run_cli(cat, "build", "--collection", "masters-otb"), 2)

    def test_build_refuses_unverified_sources(self):
        e = {"id": "m1", "collection": "masters-otb", **self.add_archive("m1.pgn", self.masters_pgn)}
        cat = self.write_catalog([e])
        self.assertEqual(self.run_cli(cat, "build", "--collection", "masters-otb"), 2)  # never fetched
        self.assertFalse((self.data_dir / "releases" / "masters-otb").exists())
        self.assertEqual(self.run_cli(cat, "fetch", "--collection", "masters-otb", "--min-free-bytes", "0"), 0)
        src = self.data_dir / "sources" / "masters-otb" / "m1.pgn"
        src.write_bytes(src.read_bytes() + b"\n")  # tamper after verification
        self.assertEqual(self.run_cli(cat, "build", "--collection", "masters-otb"), 2)
        self.assertFalse((self.data_dir / "releases" / "masters-otb").exists())


class PublicationTests(AssemblyBase):
    def setUp(self):
        super().setUp()
        e = {"id": "m1", "collection": "masters-otb", **self.add_archive("m1.pgn", self.masters_pgn)}
        self.cat = self.write_catalog([e])
        self.assertEqual(self.fetch_and_build(self.cat, "masters-otb"), 0)
        self.root_dir = self.data_dir / "releases" / "masters-otb"

    def fake_codb(self, fail_step: str) -> Path:
        # /tmp may be mounted noexec; keep the helper under the git-ignored target/ directory.
        exec_dir = REPO / "target" / "test-helpers"
        exec_dir.mkdir(parents=True, exist_ok=True)
        script = exec_dir / f"fake_codb_{fail_step}_{os.getpid()}.sh"
        self.addCleanup(lambda: script.unlink(missing_ok=True))
        script.write_text(
            "#!/usr/bin/env bash\n"
            f'if [ "$1" = "{fail_step}" ]; then echo "injected failure" >&2; exit 1; fi\n'
            f'exec "{self.codb}" "$@"\n'
        )
        script.chmod(0o755)
        return script

    def test_failed_build_does_not_overwrite_active_release(self):
        before = self.release("masters-otb")["dataset_version"]
        self.assertEqual(self.run_cli(self.cat, "build", "--collection", "masters-otb",
                                      codb=self.fake_codb("build")), 2)
        self.assertEqual(self.release("masters-otb")["dataset_version"], before)
        self.assertEqual(self.run_cli(self.cat, "verify", "--collection", "masters-otb"), 0)

    def test_failed_index_step_does_not_publish(self):
        before = self.release("masters-otb")["dataset_version"]
        self.assertEqual(self.run_cli(self.cat, "build", "--collection", "masters-otb",
                                      codb=self.fake_codb("build-index")), 2)
        self.assertEqual(self.release("masters-otb")["dataset_version"], before)

    def test_failed_verification_prevents_publication(self):
        before = self.release("masters-otb")["dataset_version"]
        self.assertEqual(self.run_cli(self.cat, "build", "--collection", "masters-otb",
                                      codb=self.fake_codb("verify")), 2)
        self.assertEqual(self.release("masters-otb")["dataset_version"], before)
        leftovers = list((self.data_dir / "work").glob("*"))
        self.assertTrue(leftovers, "failed workspace is kept for diagnosis")

    def test_repeated_build_preserves_previous_release_and_versions(self):
        old_version = self.release("masters-otb")["dataset_version"]
        self.assertEqual(self.run_cli(self.cat, "build", "--collection", "masters-otb"), 0)
        new_version = self.release("masters-otb")["dataset_version"]
        self.assertNotEqual(old_version, new_version)
        old_snapshot = self.root_dir / "versions" / old_version
        self.assertTrue(old_snapshot.is_dir(), "old immutable version preserved")
        self.assertEqual(self.run_cli(self.cat, "verify", "--collection", "masters-otb"), 0)
        r = subprocess.run([str(self.codb), "verify", "--dataset", str(old_snapshot), "--deep"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_verify_detects_artifact_tamper(self):
        shard = next((corpus.current_version_dir(self.root_dir) / "shards").glob("*.cgb3"))
        data = bytearray(shard.read_bytes())
        data[len(data) // 2] ^= 0xFF
        shard.write_bytes(bytes(data))
        self.assertEqual(self.run_cli(self.cat, "verify", "--collection", "masters-otb"), 2)

    def test_package_contains_everything_and_extracts_clean(self):
        out = self.root / "pkg"
        self.assertEqual(self.run_cli(self.cat, "package", "--collection", "masters-otb", "--output-dir", str(out)), 0)
        tars = list(out.glob("*.tar"))
        self.assertEqual(len(tars), 1)
        sums = Path(str(tars[0]) + ".sha256").read_text().split()
        self.assertEqual(sums[0], corpus.sha256_file(tars[0]))
        with tarfile.open(tars[0]) as tf:
            names = tf.getnames()
        for needed in ("RELEASE/RELEASE.json", "RELEASE/DATASET_LICENSE.txt", "current", "manifest.json",
                       "games.idx", "positions.cpi"):
            self.assertTrue(any(n.endswith("/" + needed) for n in names), f"package lacks {needed}")
        self.assertTrue(any(n.endswith(".cgb3") for n in names))
        self.assertFalse(any(".lock" in n or n.endswith(".part") for n in names),
                         "build-time temporary files must be excluded")
        # independent verification, then tamper detection
        self.assertEqual(self.run_cli(self.cat, "verify-package", "--archive", str(tars[0])), 0)
        raw = bytearray(tars[0].read_bytes())
        raw[len(raw) // 2] ^= 0xFF
        tars[0].write_bytes(bytes(raw))
        self.assertEqual(self.run_cli(self.cat, "verify-package", "--archive", str(tars[0])), 2)

    def test_package_is_byte_reproducible_for_a_release(self):
        out1, out2 = self.root / "p1", self.root / "p2"
        for o in (out1, out2):
            self.assertEqual(self.run_cli(self.cat, "package", "--collection", "masters-otb", "--output-dir", str(o)), 0)
        self.assertEqual(corpus.sha256_file(next(out1.glob("*.tar"))), corpus.sha256_file(next(out2.glob("*.tar"))))

    def test_refuses_to_package_unverified_release(self):
        shard = next((corpus.current_version_dir(self.root_dir) / "shards").glob("*.cgb3"))
        shard.write_bytes(shard.read_bytes()[:-3])
        self.assertEqual(self.run_cli(self.cat, "package", "--collection", "masters-otb",
                                      "--output-dir", str(self.root / "pkg")), 2)


class ServingTests(AssemblyBase):
    """Both collections served together from generated release datasets."""

    def setUp(self):
        super().setUp()
        m = {"id": "m1", "collection": "masters-otb", **self.add_archive("m1.pgn", self.masters_pgn)}
        e = {"id": "e1", "collection": "elite-online", **self.add_archive("e1.pgn", self.elite_pgn)}
        self.cat = self.write_catalog([m, e])
        for c in corpus.COLLECTIONS:
            self.assertEqual(self.fetch_and_build(self.cat, c), 0)

    def test_bootstrap_installs_both_verified_packages(self):
        package_dir = self.root / "release-packages"
        package_dir.mkdir()
        archives = {}
        for collection in corpus.COLLECTIONS:
            self.assertEqual(self.run_cli(self.cat, "package", "--collection", collection,
                                          "--output-dir", str(package_dir)), 0)
            archives[collection] = next(package_dir.glob(f"{collection}-*.tar"))

        provisioned = self.root / "provisioned-data"
        rc = corpus.main([
            "--catalog", str(self.cat), "--data-dir", str(provisioned), "--codb", str(self.codb),
            "bootstrap", "--masters-package", str(archives["masters-otb"]),
            "--elite-package", str(archives["elite-online"]),
        ])
        self.assertEqual(rc, 0)
        for collection in corpus.COLLECTIONS:
            root = provisioned / "releases" / collection
            self.assertTrue(corpus.current_version_dir(root).is_dir())
            self.assertIsNone(corpus.verify_release_files(root))
            report = subprocess.run([str(self.codb), "verify", "--dataset", str(root), "--deep"],
                                    capture_output=True, text=True)
            self.assertEqual(report.returncode, 0, report.stdout + report.stderr)

    def test_unchanged_release_package_can_be_reissued_from_full_release_files(self):
        elite_root = self.data_dir / "releases" / "elite-online"
        version = corpus.current_version_dir(elite_root)
        release_path = version / "RELEASE" / "RELEASE.json"
        release = json.loads(release_path.read_text())
        for name in ("RELEASE/DATASET_LICENSE.txt", "RELEASE/build_report.json",
                     "RELEASE/index_report.json", "RELEASE/quality_report.json",
                     "RELEASE/metrics.json", "RELEASE/sources.manifest.json"):
            release["artifacts"].pop(name, None)
        release_path.write_text(json.dumps(release, indent=2, sort_keys=True) + "\n")
        self.assertIsNone(corpus.verify_release_files(elite_root))
        self.assertIsNone(
            corpus.verify_release_compatibility(elite_root, "elite-online", corpus.load_catalog(self.cat))
        )

    def test_unrelated_masters_catalog_change_keeps_elite_release_compatible(self):
        changed = json.loads(self.cat.read_text())
        changed["catalog_id"] = "test-catalog-with-masters-policy-update"
        changed["collections"]["masters-otb"]["curation_policy_version"] = "new-masters-policy"
        elite_root = self.data_dir / "releases" / "elite-online"
        masters_root = self.data_dir / "releases" / "masters-otb"
        self.assertIsNone(
            corpus.verify_release_compatibility(elite_root, "elite-online", changed)
        )
        self.assertIn(
            "curation policy is incompatible",
            corpus.verify_release_compatibility(masters_root, "masters-otb", changed),
        )

    def test_bootstrap_rejects_bad_checksums_wrong_collection_and_incomplete_archive(self):
        package_dir = self.root / "release-packages"
        package_dir.mkdir()
        archives = {}
        for collection in corpus.COLLECTIONS:
            self.assertEqual(self.run_cli(self.cat, "package", "--collection", collection,
                                          "--output-dir", str(package_dir)), 0)
            archives[collection] = next(package_dir.glob(f"{collection}-*.tar"))

        def invoke(masters: Path, elite: Path, name: str) -> int:
            return corpus.main([
                "--catalog", str(self.cat), "--data-dir", str(self.root / name),
                "--codb", str(self.codb), "bootstrap", "--masters-package", str(masters),
                "--elite-package", str(elite),
            ])

        tampered = self.root / "tampered.tar"
        shutil.copy2(archives["masters-otb"], tampered)
        tampered.write_bytes(tampered.read_bytes() + b"tamper")
        original_digest = corpus.sha256_file(archives["masters-otb"])
        Path(str(tampered) + ".sha256").write_text(
            f"{original_digest}  {tampered.name}\n"
        )
        self.assertEqual(invoke(tampered, archives["elite-online"], "bad-checksum"), 2)
        self.assertFalse((self.root / "bad-checksum" / "releases").exists())

        self.assertEqual(invoke(archives["elite-online"], archives["elite-online"], "wrong-collection"), 2)
        self.assertFalse((self.root / "wrong-collection" / "releases").exists())

        incomplete = self.root / "incomplete.tar"
        with tarfile.open(incomplete, "w") as tf:
            data = b"incomplete"
            info = tarfile.TarInfo("masters-otb/README.txt")
            info.size = len(data)
            tf.addfile(info, io.BytesIO(data))
        Path(str(incomplete) + ".sha256").write_text(
            f"{corpus.sha256_file(incomplete)}  {incomplete.name}\n"
        )
        self.assertEqual(invoke(incomplete, archives["elite-online"], "incomplete"), 2)
        self.assertFalse((self.root / "incomplete" / "releases").exists())

        # A complete, checksum-valid package built under an obsolete curation policy is incompatible.
        incompatible_catalog = self.root / "incompatible-catalog.json"
        changed = json.loads(self.cat.read_text())
        changed["collections"]["masters-otb"]["curation_policy_version"] = "obsolete-policy"
        incompatible_catalog.write_text(json.dumps(changed))
        incompatible_data = self.root / "incompatible-policy"
        incompatible_rc = corpus.main([
            "--catalog", str(incompatible_catalog), "--data-dir", str(incompatible_data),
            "--codb", str(self.codb), "bootstrap",
            "--masters-package", str(archives["masters-otb"]),
            "--elite-package", str(archives["elite-online"]),
        ])
        self.assertEqual(incompatible_rc, 2)
        self.assertFalse((incompatible_data / "releases").exists())

    def test_http_semantic_checks_against_generated_releases(self):
        roots = {c: self.data_dir / "releases" / c for c in corpus.COLLECTIONS}
        depth = {"masters-otb": 40, "elite-online": 30}
        rep = corpus.run_http_checks(self.codb, roots, "127.0.0.1", corpus.free_port(),
                                     self.root / "serve.log", depth, requests_per_col=60, sample_games=100)
        self.assertTrue(rep["passed"], json.dumps(rep["failed"], indent=2))
        self.assertGreater(rep["checks_total"], 60)
        for c in corpus.COLLECTIONS:
            self.assertEqual(rep["collections"][c]["mismatches"], 0)
            self.assertGreater(rep["collections"][c]["cases"], 5)

    def test_collection_isolation_and_snapshot_reload(self):
        import http.client
        roots = {c: self.data_dir / "releases" / c for c in corpus.COLLECTIONS}
        port = corpus.free_port()
        cmd = corpus.serve_command(self.codb, roots["masters-otb"], roots["elite-online"], "127.0.0.1", port,
                                   "", ["--admin-token", "test-token"])
        with open(self.root / "serve.log", "wb") as lf:
            proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT)
        self.addCleanup(proc.terminate)
        h = corpus.Http("127.0.0.1", port)
        for _ in range(200):
            try:
                if h.json("/ready")[0] == 200:
                    break
            except OSError:
                pass
            import time
            time.sleep(0.05)
        st, colls = h.json("/v1/collections")
        before = {c["collection"]: c["dataset_version"] for c in colls["collections"]}
        # Each collection reports its own games count; ids do not leak across collections.
        ids_m = corpus.read_game_ids(corpus.current_version_dir(roots["masters-otb"]))
        self.assertEqual(h.get(f"/v1/collections/elite-online/games/{ids_m[0]}")[0], 404)
        self.assertEqual(h.get(f"/v1/collections/masters-otb/games/{ids_m[0]}")[0], 200)
        # Publish a newly built masters snapshot, then hot-reload.
        self.assertEqual(self.run_cli(self.cat, "build", "--collection", "masters-otb"), 0)
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=20)
        conn.request("POST", "/v1/admin/reload", headers={"Authorization": "Bearer test-token"})
        resp = conn.getresponse()
        resp.read()
        self.assertEqual(resp.status, 200)
        st, colls = h.json("/v1/collections")
        after = {c["collection"]: c["dataset_version"] for c in colls["collections"]}
        self.assertNotEqual(before["masters-otb"], after["masters-otb"])
        self.assertEqual(before["elite-online"], after["elite-online"])
        new_version = self.release("masters-otb")["dataset_version"]
        self.assertEqual(after["masters-otb"], new_version)
        st, body = h.json("/v1/explorer/position?collection=masters-otb&moves=e4")
        self.assertEqual((st, body["dataset_version"]), (200, new_version))


if __name__ == "__main__":
    unittest.main()
