#!/usr/bin/env python3
"""Reproducible corpus acquisition, build, release, and serving orchestration.

This tool is a thin orchestration layer around the existing `codb` Rust CLI. It
never reimplements ingestion, curation, deduplication, indexing, or dataset
publication: it pins and verifies source archives, invokes `codb build`,
`codb build-index`, `codb verify --deep`, and `codb report`, records release
provenance, packages releases, and drives `codb serve`.

The normal acquisition and release workflow uses only the Python standard library. The
opt-in local 7z extraction command requires the pinned py7zr dependency in
scripts/requirements-corpus-archive.txt.

Design rules (see docs/CORPUS_BUILD.md):
  * Fail closed: an archive without a publisher checksum, with an unapproved
    license, or with a checksum mismatch is never built into a release.
  * A partial download is never treated as a verified archive.
  * Nothing large is written under Git-tracked paths (default data dir: ./data,
    which is git-ignored).
"""

from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import hashlib
import http.client
import json
import os
import posixpath
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path, PurePosixPath
from typing import Any, Iterable

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CATALOG = REPO_ROOT / "corpus" / "catalog.json"
DEFAULT_MASTERS_EVENT_AUDIT = REPO_ROOT / "corpus" / "masters_event_qualification.json"
COLLECTIONS = ("masters-otb", "elite-online")
CATALOG_SCHEMA_VERSION = 1
CHUNK = 1 << 20
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
CONFIDENCE_LEVELS = ("high", "medium", "low")
USER_AGENT = "chess-openings-db-corpus/1 (+https://github.com/Nerver-zip/chess-openings-db)"


class CorpusError(Exception):
    """Fail-closed error with a clear diagnostic. Exit code 2."""


class TransientError(CorpusError):
    """A retryable failure (network error, truncated body, HTTP 5xx)."""


class ChecksumMismatch(CorpusError):
    """Downloaded bytes do not match the pinned checksum."""


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def log(msg: str) -> None:
    print(msg, flush=True)


# --------------------------------------------------------------------------- #
# Hashing and filesystem helpers
# --------------------------------------------------------------------------- #
def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(CHUNK):
            h.update(chunk)
    return h.hexdigest()


def safe_archive_member_name(name: str) -> bool:
    """Accept only a plain printable file name, never an archive path."""
    return bool(
        name
        and name not in {".", ".."}
        and not name.startswith(".")
        and "/" not in name
        and "\\" not in name
        and ":" not in name
        and not any(ord(char) < 32 or ord(char) == 127 for char in name)
    )


def _write_json_new(path: Path, value: dict[str, Any]) -> None:
    """Atomically create a JSON record without replacing an existing file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
        os.link(temporary, path)
        temporary.unlink()
        fsync_dir(path.parent)
    except Exception:
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()
        raise


def extract_local_7z(
    archive_path: Path,
    member_name: str,
    output_path: Path,
    record_path: Path | None = None,
    max_expanded_bytes: int = 64 * 1024**3,
    max_expansion_ratio: int = 1000,
    expected_local_archive_sha256: str | None = None,
    expected_member_sha256: str | None = None,
) -> dict[str, Any]:
    """Safely extract one regular 7z member and write local-only provenance.

    This intentionally does not make a source catalog entry or classify local hashes as
    publisher-authenticated. The generated record is for reproducible local evaluation.
    """
    archive_path = Path(archive_path)
    archive_lstat = archive_path.lstat()
    if not os.path.isfile(archive_path) or archive_path.is_symlink():
        raise CorpusError(f"7z archive must be a regular non-symlink file: {archive_path}")
    output_path = Path(os.path.abspath(output_path))
    output_parent = output_path.parent.resolve()
    output_path = output_parent / output_path.name
    assert_untracked_location(output_path)
    output_parent.mkdir(parents=True, exist_ok=True)
    if not safe_archive_member_name(member_name):
        raise CorpusError(f"requested 7z member must be a plain file name: {member_name!r}")
    if max_expanded_bytes <= 0 or max_expansion_ratio <= 0:
        raise CorpusError("7z expansion limits must be positive")
    if output_path.name != member_name:
        raise CorpusError("7z output file name must match the selected archive member")
    if os.path.lexists(output_path):
        raise CorpusError(f"7z output already exists; refusing to overwrite: {output_path}")
    record_path = Path(record_path) if record_path else output_path.with_name(
        output_path.name + ".local-source.json"
    )
    record_path = Path(os.path.abspath(record_path))
    record_path = record_path.parent.resolve() / record_path.name
    assert_untracked_location(record_path)
    if os.path.lexists(record_path):
        raise CorpusError(f"local source record already exists; refusing to overwrite: {record_path}")

    archive_sha256 = sha256_file(archive_path)
    if expected_local_archive_sha256 and archive_sha256 != expected_local_archive_sha256.lower():
        raise ChecksumMismatch(
            f"local archive SHA-256 mismatch: {archive_sha256} != expected local "
            f"{expected_local_archive_sha256}"
        )

    try:
        import py7zr
    except ImportError as error:
        raise CorpusError(
            "safe .7z extraction requires py7zr; install scripts/requirements-corpus-archive.txt"
        ) from error

    try:
        with py7zr.SevenZipFile(archive_path, mode="r") as seven_zip:
            members = seven_zip.list()
            if len(members) != 1:
                raise CorpusError(
                    f"7z archive must contain exactly one member; found {len(members)}"
                )
            member = members[0]
            if (
                member.filename != member_name
                or not safe_archive_member_name(member.filename)
                or not member.is_file
                or member.is_directory
                or member.is_symlink
                or not member.archivable
            ):
                raise CorpusError(
                    f"7z member must be the requested regular file only: {member.filename!r}"
                )
            declared_size = member.uncompressed
            compressed_size = member.compressed
            if not isinstance(declared_size, int) or declared_size < 0:
                raise CorpusError("7z member has an invalid declared uncompressed size")
            if declared_size > max_expanded_bytes:
                raise CorpusError(
                    f"7z member expands to {declared_size} bytes, above the configured limit "
                    f"{max_expanded_bytes}"
                )
            if not isinstance(compressed_size, int) or compressed_size <= 0:
                raise CorpusError("7z member has an invalid compressed size")
            if declared_size > compressed_size * max_expansion_ratio:
                raise CorpusError(
                    f"7z member expansion ratio exceeds the configured limit "
                    f"{max_expansion_ratio}:1"
                )

            with tempfile.TemporaryDirectory(prefix=".codb-7z-", dir=output_parent) as temp_name:
                temp_dir = Path(temp_name)
                seven_zip.extract(path=temp_dir, targets=[member.filename])
                extracted_path = temp_dir / member.filename
                extracted_stat = extracted_path.lstat()
                if not os.path.isfile(extracted_path) or extracted_path.is_symlink():
                    raise CorpusError("7z extraction did not produce a regular output file")
                if extracted_stat.st_size != declared_size:
                    raise CorpusError(
                        f"7z extracted size {extracted_stat.st_size} differs from declared "
                        f"size {declared_size}"
                    )
                if extracted_stat.st_size > max_expanded_bytes:
                    raise CorpusError("7z extracted output exceeds the configured expansion limit")
                member_sha256 = sha256_file(extracted_path)
                if expected_member_sha256 and member_sha256 != expected_member_sha256.lower():
                    raise ChecksumMismatch(
                        f"local member SHA-256 mismatch: {member_sha256} != expected local "
                        f"{expected_member_sha256}"
                    )
                os.chmod(extracted_path, 0o644)
                os.link(extracted_path, output_path)
                fsync_dir(output_parent)
    except CorpusError:
        raise
    except Exception as error:
        raise CorpusError(f"7z archive extraction failed closed: {error}") from error

    record = {
        "schema_version": 1,
        "source_kind": "user_supplied_local_archive",
        "archive_path": str(archive_path.resolve()),
        "archive_filename": archive_path.name,
        "archive_size_bytes": archive_lstat.st_size,
        "archive_sha256_local": archive_sha256,
        "publisher_checksum": None,
        "publisher_checksum_verified": False,
        "member_filename": member_name,
        "member_size_bytes": output_path.stat().st_size,
        "member_sha256_local": member_sha256,
        "extraction_tool": f"py7zr {py7zr.__version__}",
        "license_status": "publisher_claimed_CC-BY-NC-4.0_upstream_rights_not_independently_cleared",
        "release_eligible": False,
        "created_at": utc_now(),
    }
    try:
        _write_json_new(record_path, record)
    except Exception:
        with contextlib.suppress(FileNotFoundError):
            output_path.unlink()
        raise
    record["output_path"] = str(output_path)
    record["record_path"] = str(record_path)
    return record


def cmd_extract_local_7z(args: argparse.Namespace) -> int:
    record = extract_local_7z(
        Path(args.archive),
        args.member,
        Path(args.output),
        Path(args.record) if args.record else None,
        args.max_expanded_bytes,
        args.max_expansion_ratio,
        args.expected_local_archive_sha256,
        args.expected_member_sha256,
    )
    log(json.dumps(record, indent=2, sort_keys=True))
    return 0


def fsync_dir(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def dir_allocated_bytes(paths: Iterable[Path]) -> int:
    """Allocated bytes under `paths`, counting each inode once."""
    seen: set[tuple[int, int]] = set()
    total = 0
    for root in paths:
        stack = [root]
        while stack:
            cur = stack.pop()
            try:
                with os.scandir(cur) as it:
                    for e in it:
                        try:
                            st = e.stat(follow_symlinks=False)
                        except OSError:
                            continue
                        if e.is_dir(follow_symlinks=False):
                            stack.append(Path(e.path))
                        else:
                            key = (st.st_dev, st.st_ino)
                            if key not in seen:
                                seen.add(key)
                                total += st.st_blocks * 512
            except OSError:
                continue
    return total


def data_dir_from(args: argparse.Namespace) -> Path:
    d = args.data_dir or os.environ.get("CODB_DATA_DIR") or str(REPO_ROOT / "data")
    return Path(d).resolve()


def assert_untracked_location(path: Path) -> None:
    """Refuse to place archives or releases under a Git-tracked directory."""
    try:
        rel = path.resolve().relative_to(REPO_ROOT)
    except ValueError:
        return  # outside the repository: always fine
    top = rel.parts[0] if rel.parts else ""
    if top in {"data", "raw", "downloads", "scratch", "tmp", "target"}:
        return
    raise CorpusError(
        f"data directory {path} is inside the repository but not under a git-ignored "
        "directory (data/, raw/, downloads/, scratch/, tmp/). Choose another --data-dir "
        "so large artifacts cannot be committed by accident."
    )


# --------------------------------------------------------------------------- #
# Source catalog
# --------------------------------------------------------------------------- #
def load_catalog(path: Path) -> dict[str, Any]:
    try:
        cat = json.loads(Path(path).read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise CorpusError(f"source catalog not found: {path}")
    except json.JSONDecodeError as e:
        raise CorpusError(f"source catalog is not valid JSON ({path}): {e}")
    validate_catalog(cat)
    return cat


def _allow_http() -> bool:
    return os.environ.get("CODB_CORPUS_ALLOW_HTTP") == "1"


def validate_catalog(cat: dict[str, Any]) -> None:
    """Strict structural validation. Raises CorpusError on the first problem."""
    if not isinstance(cat, dict):
        raise CorpusError("catalog must be a JSON object")
    if cat.get("schema_version") != CATALOG_SCHEMA_VERSION:
        raise CorpusError(f"catalog schema_version must be {CATALOG_SCHEMA_VERSION}")
    for key in ("catalog_id", "licenses", "collections", "sources"):
        if key not in cat:
            raise CorpusError(f"catalog missing required field '{key}'")

    licenses = cat["licenses"]
    if not isinstance(licenses, dict) or not licenses:
        raise CorpusError("catalog 'licenses' must be a non-empty object")
    for lid, lic in licenses.items():
        for key in ("name", "url", "approved", "attribution_required", "share_alike"):
            if key not in lic:
                raise CorpusError(f"license '{lid}' missing field '{key}'")

    cols = cat["collections"]
    if set(cols) != set(COLLECTIONS):
        raise CorpusError(f"catalog collections must be exactly {sorted(COLLECTIONS)}")
    for cname, c in cols.items():
        for key in ("allowed_licenses", "default_depth_plies", "rating_system", "profiles"):
            if key not in c:
                raise CorpusError(f"collection '{cname}' missing field '{key}'")
        for lid in c["allowed_licenses"]:
            if lid not in licenses:
                raise CorpusError(f"collection '{cname}' references unknown license '{lid}'")

    seen_ids: set[str] = set()
    seen_files: set[str] = set()
    for s in cat["sources"]:
        sid = s.get("id", "<missing id>")
        for key in (
            "id", "collection", "publisher", "url", "filename", "period", "license",
            "expected_size_bytes", "publisher_sha256", "publisher_checksum_url",
            "provenance_confidence", "eligibility", "notes",
        ):
            if key not in s:
                raise CorpusError(f"source '{sid}' missing required field '{key}'")
        if sid in seen_ids:
            raise CorpusError(f"duplicate source id '{sid}'")
        seen_ids.add(sid)
        if s["collection"] not in COLLECTIONS:
            raise CorpusError(f"source '{sid}' has unknown collection '{s['collection']}'")
        if s["filename"] in seen_files:
            raise CorpusError(f"duplicate archive filename '{s['filename']}'")
        seen_files.add(s["filename"])
        if "/" in s["filename"] or s["filename"].startswith("."):
            raise CorpusError(f"source '{sid}' filename must be a plain file name")
        url = s["url"]
        if not (url.startswith("https://") or (_allow_http() and url.startswith("http://"))):
            raise CorpusError(f"source '{sid}' URL must use HTTPS: {url}")
        if url.rsplit("/", 1)[-1] != s["filename"]:
            raise CorpusError(f"source '{sid}' filename does not match its URL basename")
        lid = s["license"]
        if lid not in licenses:
            raise CorpusError(f"source '{sid}' uses unknown license '{lid}'")
        if not licenses[lid]["approved"]:
            raise CorpusError(f"source '{sid}' license '{lid}' is not approved for redistribution")
        if lid not in cols[s["collection"]]["allowed_licenses"]:
            raise CorpusError(
                f"source '{sid}': license '{lid}' is not allowed for collection '{s['collection']}'"
            )
        psha = s["publisher_sha256"]
        if not isinstance(psha, str) or not SHA256_RE.match(psha):
            raise CorpusError(
                f"source '{sid}' has a missing or malformed publisher_sha256; "
                "refusing to use an archive without an authoritative upstream checksum"
            )
        vsha = s.get("verified_sha256")
        if vsha is not None and vsha != psha:
            raise CorpusError(f"source '{sid}': verified_sha256 differs from publisher_sha256")
        if not isinstance(s["expected_size_bytes"], int) or s["expected_size_bytes"] <= 0:
            raise CorpusError(f"source '{sid}' expected_size_bytes must be a positive integer")
        if s["provenance_confidence"] not in CONFIDENCE_LEVELS:
            raise CorpusError(f"source '{sid}' provenance_confidence must be one of {CONFIDENCE_LEVELS}")

    for cname, c in cols.items():
        for pname, ids in c["profiles"].items():
            for sid in ids:
                match = [s for s in cat["sources"] if s["id"] == sid]
                if not match:
                    raise CorpusError(f"profile {cname}/{pname} references unknown source '{sid}'")
                if match[0]["collection"] != cname:
                    raise CorpusError(
                        f"profile {cname}/{pname} mixes collections: source '{sid}' belongs to "
                        f"'{match[0]['collection']}'"
                    )
            if len(set(ids)) != len(ids):
                raise CorpusError(f"profile {cname}/{pname} lists a source twice")


def select_sources(cat: dict[str, Any], collection: str, profile: str) -> list[dict[str, Any]]:
    """Sources of a profile in deterministic (period, id) order."""
    cols = cat["collections"]
    if collection not in cols:
        raise CorpusError(f"unknown collection '{collection}'")
    profiles = cols[collection]["profiles"]
    if profile not in profiles:
        raise CorpusError(
            f"collection '{collection}' has no profile '{profile}' (available: {sorted(profiles)})"
        )
    by_id = {s["id"]: s for s in cat["sources"]}
    chosen = [by_id[i] for i in profiles[profile]]
    for s in chosen:
        if s["collection"] != collection:
            raise CorpusError(f"mixed-collection source '{s['id']}' in {collection}/{profile}")
    return sorted(chosen, key=lambda s: (s["period"], s["id"]))


# --------------------------------------------------------------------------- #
# Source paths and local state
# --------------------------------------------------------------------------- #
def source_path(data_dir: Path, s: dict[str, Any]) -> Path:
    return data_dir / "sources" / s["collection"] / s["filename"]


def sidecar_path(p: Path) -> Path:
    return p.with_name(p.name + ".verified.json")


def read_sidecar(p: Path) -> dict[str, Any] | None:
    sc = sidecar_path(p)
    if not sc.exists():
        return None
    try:
        return json.loads(sc.read_text())
    except (OSError, json.JSONDecodeError):
        return None


def write_sidecar(p: Path, s: dict[str, Any], sha: str) -> dict[str, Any]:
    st = p.stat()
    rec = {
        "id": s["id"],
        "filename": s["filename"],
        "url": s["url"],
        "sha256": sha,
        "size_bytes": st.st_size,
        "mtime_ns": st.st_mtime_ns,
        "acquired_at": (read_sidecar(p) or {}).get("acquired_at") or utc_now(),
        "verified_at": utc_now(),
        "retrieval": "full-archive-https",
        "partial": False,
    }
    tmp = sidecar_path(p).with_suffix(".tmp")
    tmp.write_text(json.dumps(rec, indent=2, sort_keys=True) + "\n")
    os.replace(tmp, sidecar_path(p))
    return rec


def verify_local_source(
    data_dir: Path, s: dict[str, Any], rehash: bool = True
) -> tuple[bool, str]:
    """Offline verification of a local archive against the pinned checksum."""
    p = source_path(data_dir, s)
    if not p.exists():
        return False, "missing"
    size = p.stat().st_size
    if size != s["expected_size_bytes"]:
        return False, f"size mismatch: {size} != expected {s['expected_size_bytes']}"
    sc = read_sidecar(p)
    if (
        not rehash
        and sc
        and sc.get("sha256") == s["publisher_sha256"]
        and sc.get("size_bytes") == size
        and sc.get("mtime_ns") == p.stat().st_mtime_ns
    ):
        return True, "verified (sidecar)"
    actual = sha256_file(p)
    if actual != s["publisher_sha256"]:
        return False, f"sha256 mismatch: {actual} != publisher {s['publisher_sha256']}"
    write_sidecar(p, s, actual)
    return True, "verified"


# --------------------------------------------------------------------------- #
# Download engine
# --------------------------------------------------------------------------- #
class FetchOptions:
    def __init__(
        self,
        retries: int = 4,
        backoff_base: float = 2.0,
        timeout: float = 60.0,
        min_free_bytes: int = 2 * 1024**3,
        max_download_bytes: int | None = None,
        offline: bool = False,
        rehash: bool = False,
    ) -> None:
        self.retries = retries
        self.backoff_base = backoff_base
        self.timeout = timeout
        self.min_free_bytes = min_free_bytes
        self.max_download_bytes = max_download_bytes
        self.offline = offline
        self.rehash = rehash


def _http_stream(url: str, resume_from: int, timeout: float):
    headers = {"User-Agent": USER_AGENT, "Accept-Encoding": "identity"}
    if resume_from:
        headers["Range"] = f"bytes={resume_from}-"
    req = urllib.request.Request(url, headers=headers)
    try:
        return urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.HTTPError as e:
        if e.code == 416:
            raise TransientError("HTTP 416 (range not satisfiable); restarting from scratch")
        if e.code in (408, 429) or e.code >= 500:
            raise TransientError(f"HTTP {e.code} from server")
        raise CorpusError(f"unexpected HTTP {e.code} for {url}")
    except (urllib.error.URLError, socket.timeout, ConnectionError, TimeoutError) as e:
        raise TransientError(f"network error: {e}")


def _download_once(s: dict[str, Any], part: Path, opts: FetchOptions) -> None:
    expected = s["expected_size_bytes"]
    resume = part.stat().st_size if part.exists() else 0
    if resume > expected:
        part.unlink()
        resume = 0
    if resume < expected:
        try:
            resp = _http_stream(s["url"], resume, opts.timeout)
        except TransientError as e:
            if "416" in str(e):
                part.unlink(missing_ok=True)
            raise
        with contextlib.closing(resp):
            status = resp.status
            length = resp.headers.get("Content-Length")
            if resume:
                if status == 206:
                    cr = resp.headers.get("Content-Range", "")
                    m = re.match(r"bytes (\d+)-(\d+)/(\d+|\*)$", cr)
                    if not m or int(m.group(1)) != resume:
                        raise CorpusError(f"unexpected Content-Range '{cr}' for resumed download")
                    if m.group(3) != "*" and int(m.group(3)) != expected:
                        raise CorpusError(
                            f"remote size {m.group(3)} differs from pinned size {expected}"
                        )
                elif status == 200:
                    resume = 0  # server ignored Range; restart
                else:
                    raise CorpusError(f"unexpected HTTP status {status} for ranged request")
            elif status != 200:
                raise CorpusError(f"unexpected HTTP status {status} for {s['url']}")
            if length is not None and int(length) != expected - resume:
                raise CorpusError(
                    f"remote Content-Length {length} does not match pinned size "
                    f"{expected} (resume offset {resume}); upstream archive changed or wrong response"
                )
            mode = "ab" if resume else "wb"
            try:
                with open(part, mode) as out:
                    if resume == 0 and mode == "wb":
                        out.truncate(0)
                    while True:
                        chunk = resp.read(CHUNK)
                        if not chunk:
                            break
                        out.write(chunk)
                    out.flush()
                    os.fsync(out.fileno())
            except (http.client.IncompleteRead, ConnectionError, socket.timeout, TimeoutError) as e:
                raise TransientError(f"transfer interrupted: {e}")
    size = part.stat().st_size
    if size < expected:
        raise TransientError(f"incomplete download: {size} of {expected} bytes")
    if size > expected:
        part.unlink()
        raise CorpusError(f"downloaded {size} bytes, more than pinned size {expected}")


def fetch_source(data_dir: Path, s: dict[str, Any], opts: FetchOptions) -> dict[str, Any]:
    """Fetch one archive, fully verified against the publisher checksum, atomically finalized."""
    final = source_path(data_dir, s)
    final.parent.mkdir(parents=True, exist_ok=True)

    if final.exists():
        ok, why = verify_local_source(data_dir, s, rehash=opts.rehash)
        if ok:
            return {"id": s["id"], "action": "already-verified", "bytes_downloaded": 0}
        quarantine = final.with_name(final.name + f".corrupt-{int(time.time())}")
        os.replace(final, quarantine)
        sidecar_path(final).unlink(missing_ok=True)
        log(f"  existing file failed verification ({why}); moved to {quarantine.name}")

    if opts.offline:
        raise CorpusError(f"offline mode: archive {s['filename']} is not present and verified")

    part = final.with_name(final.name + ".part")
    already = part.stat().st_size if part.exists() else 0
    need = max(s["expected_size_bytes"] - already, 0)
    free = shutil.disk_usage(final.parent).free
    if free < need + opts.min_free_bytes:
        raise CorpusError(
            f"insufficient disk space for {s['filename']}: need {need} + {opts.min_free_bytes} "
            f"safety bytes, only {free} free in {final.parent}"
        )

    attempt = 0
    restarted_after_mismatch = False
    downloaded = 0
    while True:
        before = part.stat().st_size if part.exists() else 0
        try:
            _download_once(s, part, opts)
            downloaded += part.stat().st_size - before
            actual = sha256_file(part)
            if actual != s["publisher_sha256"]:
                part.unlink(missing_ok=True)
                if before and not restarted_after_mismatch:
                    restarted_after_mismatch = True
                    log("  checksum mismatch after resume; restarting from scratch once")
                    continue
                raise ChecksumMismatch(
                    f"{s['filename']}: sha256 {actual} does not match publisher checksum "
                    f"{s['publisher_sha256']}; the partial file was deleted"
                )
            break
        except TransientError as e:
            downloaded += max((part.stat().st_size if part.exists() else 0) - before, 0)
            attempt += 1
            if attempt > opts.retries:
                raise CorpusError(
                    f"{s['filename']}: giving up after {opts.retries} retries ({e}); "
                    f"partial data kept in {part.name} for resume"
                )
            delay = opts.backoff_base * (2 ** (attempt - 1))
            log(f"  transient failure ({e}); retry {attempt}/{opts.retries} in {delay:.1f}s")
            time.sleep(delay)
    os.replace(part, final)
    fsync_dir(final.parent)
    write_sidecar(final, s, actual)
    return {"id": s["id"], "action": "downloaded", "bytes_downloaded": downloaded}


def cmd_fetch(args: argparse.Namespace) -> int:
    cat = load_catalog(Path(args.catalog))
    data_dir = data_dir_from(args)
    assert_untracked_location(data_dir)
    collections = COLLECTIONS if args.collection == "all" else (args.collection,)
    sources = [s for c in collections for s in select_sources(cat, c, args.profile or "baseline")]
    if args.source:
        known = {s["id"] for s in sources}
        unknown = sorted(set(args.source) - known)
        if unknown:
            raise CorpusError(f"--source id(s) not in the selected profile: {unknown}")
        sources = [s for s in sources if s["id"] in set(args.source)]

    max_dl = args.max_download_bytes
    remaining = 0
    for s in sources:
        p = source_path(data_dir, s)
        if p.exists() and p.stat().st_size == s["expected_size_bytes"]:
            continue
        part = p.with_name(p.name + ".part")
        remaining += s["expected_size_bytes"] - (part.stat().st_size if part.exists() else 0)
    if max_dl is not None and remaining > max_dl and not args.offline:
        raise CorpusError(
            f"planned download of {remaining} bytes exceeds --max-download-bytes {max_dl}"
        )

    opts = FetchOptions(
        retries=args.retries,
        backoff_base=args.backoff,
        min_free_bytes=args.min_free_bytes,
        offline=args.offline,
        rehash=args.rehash,
    )
    total = 0
    results = []
    for s in sources:
        log(f"[fetch] {s['id']} ({s['filename']}, {s['expected_size_bytes'] / 1e6:.1f} MB)")
        r = fetch_source(data_dir, s, opts)
        total += r["bytes_downloaded"]
        results.append(r)
        log(f"  -> {r['action']}")
    log(f"[fetch] done: {len(results)} archives, {total} bytes downloaded this run")

    if args.update_catalog:
        update_catalog_with_verified(Path(args.catalog), data_dir, sources)
    return 0


def update_catalog_with_verified(catalog_path: Path, data_dir: Path, sources: list[dict[str, Any]]) -> None:
    """Record verified_sha256 and acquired_at in the pinned catalog after a verified fetch."""
    raw = json.loads(catalog_path.read_text())
    by_id = {s["id"]: s for s in raw["sources"]}
    for s in sources:
        sc = read_sidecar(source_path(data_dir, s))
        if not sc:
            continue
        entry = by_id[s["id"]]
        if entry.get("verified_sha256") not in (None, sc["sha256"]):
            raise CorpusError(f"catalog verified_sha256 conflicts with local verification for {s['id']}")
        entry["verified_sha256"] = sc["sha256"]
        entry["acquired_at"] = entry.get("acquired_at") or sc["acquired_at"]
    catalog_path.write_text(json.dumps(raw, indent=2) + "\n")
    log(f"[fetch] catalog updated: {catalog_path}")


def cmd_verify_sources(args: argparse.Namespace) -> int:
    cat = load_catalog(Path(args.catalog))
    data_dir = data_dir_from(args)
    collections = COLLECTIONS if args.collection == "all" else (args.collection,)
    bad = 0
    for c in collections:
        for s in select_sources(cat, c, args.profile or "baseline"):
            ok, why = verify_local_source(data_dir, s, rehash=True)
            log(f"[verify-sources] {s['id']}: {'OK' if ok else 'FAIL'} ({why})")
            bad += 0 if ok else 1
    if bad:
        raise CorpusError(f"{bad} source archive(s) failed verification")
    log("[verify-sources] all selected archives verified against publisher checksums")
    return 0


def cmd_sources(args: argparse.Namespace) -> int:
    cat = load_catalog(Path(args.catalog))
    data_dir = data_dir_from(args)
    collections = COLLECTIONS if args.collection == "all" else (args.collection,)
    rows = []
    for c in collections:
        for s in select_sources(cat, c, args.profile or "baseline"):
            ok, why = verify_local_source(data_dir, s, rehash=False) if source_path(data_dir, s).exists() else (False, "missing")
            rows.append({
                "id": s["id"], "collection": c, "period": s["period"], "license": s["license"],
                "size_bytes": s["expected_size_bytes"], "publisher_sha256": s["publisher_sha256"],
                "verified_sha256": s.get("verified_sha256"), "acquired_at": s.get("acquired_at"),
                "provenance_confidence": s["provenance_confidence"], "local": why,
            })
    if args.json:
        print(json.dumps(rows, indent=2))
    else:
        for r in rows:
            log(f"{r['id']:<34} {r['collection']:<12} {r['period']:<8} {r['license']:<12} "
                f"{r['size_bytes'] / 1e6:>9.1f} MB  {r['provenance_confidence']:<6} local={r['local']}")
    return 0


# --------------------------------------------------------------------------- #
# Pinning (extend the catalog from the publisher's own checksum list)
# --------------------------------------------------------------------------- #
FAMILIES = {
    "broadcast": {
        "collection": "masters-otb",
        "base": "https://database.lichess.org/broadcast/",
        "name": "lichess_db_broadcast_{period}.pgn.zst",
        "license": "CC-BY-SA-4.0",
    },
    "standard": {
        "collection": "elite-online",
        "base": "https://database.lichess.org/standard/",
        "name": "lichess_db_standard_rated_{period}.pgn.zst",
        "license": "CC0-1.0",
    },
}


def _http_get(url: str, method: str = "GET", timeout: float = 60.0) -> tuple[bytes, Any]:
    if not (url.startswith("https://") or (_allow_http() and url.startswith("http://"))):
        raise CorpusError(f"refusing non-HTTPS URL {url}")
    req = urllib.request.Request(url, method=method, headers={"User-Agent": USER_AGENT})
    try:
        with contextlib.closing(urllib.request.urlopen(req, timeout=timeout)) as r:
            return (r.read() if method == "GET" else b""), r.headers
    except (urllib.error.URLError, socket.timeout) as e:
        raise CorpusError(f"could not reach {url}: {e}")


def cmd_pin(args: argparse.Namespace) -> int:
    """Add catalog entries using the publisher's SHA-256 list and HEAD sizes."""
    fam = FAMILIES[args.family]
    catalog_path = Path(args.catalog)
    raw = json.loads(catalog_path.read_text())
    sums_url = fam["base"] + "sha256sums.txt"
    body, _ = _http_get(sums_url)
    sums = {}
    for line in body.decode().splitlines():
        parts = line.split()
        if len(parts) == 2:
            sums[parts[1].lstrip("*")] = parts[0].lower()
    existing = {s["id"] for s in raw["sources"]}
    added = []
    for period in args.periods.split(","):
        period = period.strip()
        fname = fam["name"].format(period=period)
        sid = f"lichess-{args.family}-{period}"
        if sid in existing:
            log(f"[pin] {sid} already pinned")
            continue
        if fname not in sums:
            raise CorpusError(f"publisher checksum list has no entry for {fname}")
        url = fam["base"] + fname
        _, headers = _http_get(url, method="HEAD")
        size = int(headers["Content-Length"])
        raw["sources"].append({
            "id": sid,
            "collection": fam["collection"],
            "publisher": "Lichess.org",
            "url": url,
            "filename": fname,
            "period": period,
            "license": fam["license"],
            "expected_size_bytes": size,
            "publisher_sha256": sums[fname],
            "publisher_checksum_url": sums_url,
            "verified_sha256": None,
            "acquired_at": None,
            "provenance_confidence": args.confidence,
            "eligibility": args.eligibility,
            "notes": args.notes,
        })
        added.append(sid)
        log(f"[pin] {sid}: {size} bytes, sha256 {sums[fname]}")
    if args.profile:
        profiles = raw["collections"][fam["collection"]]["profiles"]
        profiles.setdefault(args.profile, [])
        for sid in added:
            if sid not in profiles[args.profile]:
                profiles[args.profile].append(sid)
    validate_catalog(raw)
    catalog_path.write_text(json.dumps(raw, indent=2) + "\n")
    return 0


# --------------------------------------------------------------------------- #
# Resource planning
# --------------------------------------------------------------------------- #
def cmd_plan(args: argparse.Namespace) -> int:
    cat = load_catalog(Path(args.catalog))
    data_dir = data_dir_from(args)
    free = shutil.disk_usage(data_dir if data_dir.exists() else data_dir.parent).free
    collections = COLLECTIONS if args.collection == "all" else (args.collection,)
    plan = {}
    for c in collections:
        srcs = select_sources(cat, c, args.profile or "baseline")
        pl = cat["collections"][c].get("planning", {})
        download = sum(
            s["expected_size_bytes"] for s in srcs
            if not (source_path(data_dir, s).exists()
                    and source_path(data_dir, s).stat().st_size == s["expected_size_bytes"])
        )
        total_input = sum(s["expected_size_bytes"] for s in srcs)
        accepted = pl.get("expected_accepted_games", 0)
        cgb3 = int(accepted * pl.get("cgb3_bytes_per_game", 0))
        cpi = int(accepted * pl.get("cpi_bytes_per_game", 0))
        plan[c] = {
            "profile": args.profile or "baseline",
            "archives": len(srcs),
            "network_transfer_bytes_remaining": download,
            "input_archive_storage_bytes": total_input,
            "expected_accepted_games_estimate": accepted,
            "expected_cgb3_bytes_estimate": cgb3,
            "expected_cpi_bytes_estimate": cpi,
            "temporary_workspace_bytes_estimate": int(accepted * pl.get("temp_bytes_per_accepted_game", 0)),
            "memory_limits": {
                "ingest_rss_measurement": "per-process VmHWM is recorded for each build step; no fixed ceiling",
                "index_memory_limit_mib_default": 128,
            },
            "rebuild_time_seconds_estimate": int(
                total_input / max(pl.get("input_bytes_per_second", 1), 1)
            ),
            "note": "Estimates come from catalog planning figures derived from measured pilot builds.",
        }
    out = {"free_bytes_in_data_dir": free, "data_dir": str(data_dir), "plan": plan}
    print(json.dumps(out, indent=2))
    return 0


# --------------------------------------------------------------------------- #
# codb invocation helpers
# --------------------------------------------------------------------------- #
def find_codb(explicit: str | None) -> Path:
    for cand in (explicit, os.environ.get("CODB_BIN")):
        if cand:
            p = Path(cand)
            if p.is_file():
                return p.resolve()
            raise CorpusError(f"codb binary not found: {cand}")
    for sub in ("release", "debug"):
        p = REPO_ROOT / "target" / sub / "codb"
        if p.is_file():
            return p
    raise CorpusError(
        "codb binary not found. Build it with `cargo build --release -p codb-cli` "
        "or pass --codb / CODB_BIN."
    )


class DiskSampler(threading.Thread):
    """Samples allocated bytes of directories to report peak temporary disk use."""

    def __init__(self, paths: list[Path], interval: float = 0.25) -> None:
        super().__init__(daemon=True)
        self.paths = paths
        self.interval = interval
        self.peak = 0
        self._halt = threading.Event()

    def run(self) -> None:
        while not self._halt.is_set():
            self.peak = max(self.peak, dir_allocated_bytes(self.paths))
            self._halt.wait(self.interval)

    def finish(self) -> int:
        self._halt.set()
        self.join()
        self.peak = max(self.peak, dir_allocated_bytes(self.paths))
        return self.peak


def process_peak_rss_bytes(pid: int, executable: Path) -> int:
    """Read the executed process VmHWM without counting inherited Python memory."""
    proc_dir = Path("/proc") / str(pid)
    try:
        if (proc_dir / "exe").resolve() != executable.resolve():
            return 0
        for line in (proc_dir / "status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1]) * 1024  # Linux reports KiB.
    except (OSError, ValueError):
        return 0
    return 0


def process_rss_components_bytes(pid: int, executable: Path) -> tuple[int, int, int]:
    """Sample current anonymous, file-backed, and shared RSS for the executed process."""
    proc_dir = Path("/proc") / str(pid)
    try:
        if (proc_dir / "exe").resolve() != executable.resolve():
            return (0, 0, 0)
        fields = {}
        for line in (proc_dir / "status").read_text().splitlines():
            name, sep, value = line.partition(":")
            if sep and name in {"RssAnon", "RssFile", "RssShmem"}:
                fields[name] = int(value.split()[0]) * 1024  # Linux reports KiB.
        return (
            fields.get("RssAnon", 0),
            fields.get("RssFile", 0),
            fields.get("RssShmem", 0),
        )
    except (OSError, ValueError, IndexError):
        return (0, 0, 0)


def run_measured(
    cmd: list[str], log_path: Path, env: dict[str, str], sample_paths: list[Path]
) -> dict[str, Any]:
    """Run a command, capturing wall time, process VmHWM, RSS components, and peak disk use."""
    sampler = DiskSampler(sample_paths)
    executable = Path(cmd[0]).resolve()
    t0 = time.monotonic()
    peak_rss_bytes = 0
    peak_rss_anon_bytes = 0
    peak_rss_file_bytes = 0
    peak_rss_shmem_bytes = 0
    component_samples = 0
    with open(log_path, "wb") as out:
        sampler.start()
        proc = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, env=env)
        while True:
            peak_rss_bytes = max(peak_rss_bytes, process_peak_rss_bytes(proc.pid, executable))
            anon_bytes, file_bytes, shmem_bytes = process_rss_components_bytes(
                proc.pid, executable
            )
            if anon_bytes or file_bytes or shmem_bytes:
                component_samples += 1
            peak_rss_anon_bytes = max(peak_rss_anon_bytes, anon_bytes)
            peak_rss_file_bytes = max(peak_rss_file_bytes, file_bytes)
            peak_rss_shmem_bytes = max(peak_rss_shmem_bytes, shmem_bytes)
            finished_pid, status, ru = os.wait4(proc.pid, os.WNOHANG)
            if finished_pid:
                proc.returncode = os.waitstatus_to_exitcode(status)
                break
            time.sleep(0.005)
    wall = time.monotonic() - t0
    peak_disk = sampler.finish()
    if peak_rss_bytes == 0:
        # Short-lived commands can exit before procfs is sampled; wait4 returns
        # rusage for this child alone and does not include the Python orchestrator.
        peak_rss_bytes = ru.ru_maxrss * (1024 if sys.platform.startswith("linux") else 1)
        if peak_rss_bytes == 0 and Path("/proc").is_dir():
            raise CorpusError(f"Could not sample RSS for executed process: {cmd[0]}")
    return {
        "command": " ".join(cmd),
        "returncode": proc.returncode,
        "wall_seconds": round(wall, 3),
        "peak_rss_bytes": peak_rss_bytes,
        "peak_rss_anon_bytes": peak_rss_anon_bytes,
        "peak_rss_file_bytes": peak_rss_file_bytes,
        "peak_rss_shmem_bytes": peak_rss_shmem_bytes,
        "rss_component_sample_count": component_samples,
        "rss_component_sample_interval_seconds": 0.005,
        "rss_sampling_note": (
            "VmHWM is the kernel high-water mark of total process RSS. Anonymous, file-backed, "
            "and shared values are 5 ms procfs samples and may understate their true peaks."
        ),
        "peak_workspace_disk_bytes": peak_disk,
        "log": str(log_path),
    }


def codb_json(codb: Path, args: list[str]) -> Any:
    r = subprocess.run([str(codb), *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise CorpusError(f"codb {' '.join(args)} failed: {r.stderr.strip() or r.stdout.strip()}")
    return json.loads(r.stdout)


def git_commit() -> str:
    try:
        r = subprocess.run(["git", "-C", str(REPO_ROOT), "rev-parse", "HEAD"],
                           capture_output=True, text=True, timeout=10)
        return r.stdout.strip() if r.returncode == 0 else "unknown"
    except (OSError, subprocess.SubprocessError):
        return "unknown"


def git_dirty() -> bool:
    try:
        r = subprocess.run(["git", "-C", str(REPO_ROOT), "status", "--porcelain", "--untracked-files=no"],
                           capture_output=True, text=True, timeout=10)
        return bool(r.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        return True


def release_root(data_dir: Path, collection: str) -> Path:
    return data_dir / "releases" / collection


def current_version_dir(root: Path) -> Path:
    cur = root / "current"
    if not cur.is_symlink():
        raise CorpusError(f"{root} is not a published dataset root (no 'current' symlink)")
    return (root / os.readlink(cur)).resolve() if not os.path.isabs(os.readlink(cur)) else Path(os.readlink(cur))


# --------------------------------------------------------------------------- #
# Build
# --------------------------------------------------------------------------- #
def cmd_build(args: argparse.Namespace) -> int:
    collection = args.collection
    if collection not in COLLECTIONS:
        raise CorpusError("build requires --collection masters-otb|elite-online")
    cat = load_catalog(Path(args.catalog))
    data_dir = data_dir_from(args)
    assert_untracked_location(data_dir)
    codb = find_codb(args.codb)
    profile = args.profile or "baseline"
    ccfg = cat["collections"][collection]
    masters_event_audit = None
    if collection == "masters-otb":
        audit_arg = getattr(args, "masters_event_audit", None)
        requires_event_audit = "exact event-level verified OTB allowlist" in ccfg[
            "curation_policy_version"
        ]
        if requires_event_audit and audit_arg is None:
            audit_arg = DEFAULT_MASTERS_EVENT_AUDIT
        if audit_arg is not None:
            masters_event_audit = Path(audit_arg).resolve()
            if not masters_event_audit.is_file():
                raise CorpusError(
                    f"Masters event qualification audit is missing: {masters_event_audit}; "
                    "run scripts/audit_masters_events.py before building"
                )
    depth = args.depth_plies or ccfg["default_depth_plies"]
    sources = select_sources(cat, collection, profile)

    # 1. Fail closed on any unverified source.
    for s in sources:
        ok, why = verify_local_source(data_dir, s, rehash=True)
        if not ok:
            raise CorpusError(
                f"source {s['id']} is not verified ({why}); run `corpus.sh fetch --collection "
                f"{collection}` first. Refusing to build from untrusted bytes."
            )

    run_id = f"{collection}-{profile}-{dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ')}"
    work = data_dir / "work" / run_id
    ds_root = work / "dataset"
    tmp = work / "tmp"
    logs = work / "logs"
    for d in (tmp, logs):
        d.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, TMPDIR=str(tmp))
    log(f"[build] {run_id}: {len(sources)} sources, depth {depth}, workspace {work}")

    # 2. Pinned manifest, deterministic order.
    manifest = {
        "collection": collection,
        "sources": [
            {
                "path": str(source_path(data_dir, s)),
                "url": s["url"],
                "license": s["license"],
                "sha256": s["publisher_sha256"],
            }
            for s in sources
        ],
    }
    manifest_path = work / "sources.manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")

    metrics: dict[str, Any] = {"steps": {}}
    t_all = time.monotonic()

    def step(name: str, cmd: list[str]) -> dict[str, Any]:
        log(f"[build] step {name}: {' '.join(cmd)}")
        m = run_measured(cmd, logs / f"{name}.log", env, [work])
        metrics["steps"][name] = m
        if m["returncode"] != 0:
            raise CorpusError(
                f"step '{name}' failed (exit {m['returncode']}); see {m['log']}. "
                f"The active release (if any) was not modified; workspace kept at {work}."
            )
        return m

    build_report = work / "build_report.json"
    build_command = [str(codb), "build", "--manifest", str(manifest_path), "--collection", collection,
                     "--output", str(ds_root), "--report-json", str(build_report)]
    if masters_event_audit is not None:
        build_command.extend(["--masters-event-audit", str(masters_event_audit)])
    step("build", build_command)
    step("build-index", [str(codb), "build-index", "--dataset", str(ds_root), "--collection", collection,
                         "--depth-plies", str(depth), "--memory-limit-mib", str(args.memory_limit_mib),
                         "--json"])
    index_report = json.loads(_extract_json((logs / "build-index.log").read_text()))
    (work / "index_report.json").write_text(json.dumps(index_report, indent=2) + "\n")
    step("verify", [str(codb), "verify", "--dataset", str(ds_root), "--deep"])
    quality = work / "quality_report.json"
    step("report", [str(codb), "report", "--dataset", str(ds_root), "--output", str(quality)])
    metrics["total_wall_seconds"] = round(time.monotonic() - t_all, 3)

    # 3. Release provenance and legal notices.
    bj = json.loads(build_report.read_text())
    if not (bj["is_reconciled"] and bj["retained_matches_accepted"]):
        raise CorpusError("accounting did not reconcile; failing closed")
    qj = json.loads(quality.read_text())
    version_dir = current_version_dir(ds_root)
    write_release_files(cat, collection, profile, sources, ds_root, version_dir, bj, index_report,
                        qj, metrics, depth, args, codb, masters_event_audit)
    release_dir = version_dir / "RELEASE"
    shutil.copy(build_report, release_dir / "build_report.json")
    shutil.copy(work / "index_report.json", release_dir / "index_report.json")
    shutil.copy(quality, release_dir / "quality_report.json")
    shutil.copy(logs / "verify.log", release_dir / "verify_deep.txt")
    (release_dir / "metrics.json").write_text(json.dumps(metrics, indent=2) + "\n")
    # Keep the pinned input manifest with the immutable release snapshot.
    shutil.copy(manifest_path, release_dir / "sources.manifest.json")
    refresh_release_artifacts(version_dir)

    problem = verify_release_files(ds_root)
    if problem:
        raise CorpusError(f"candidate release artifact verification failed before publication: {problem}")
    problem = verify_release_compatibility(ds_root, collection, cat)
    if problem:
        raise CorpusError(f"candidate release is incompatible before publication: {problem}")

    # 4. Publish through the Rust storage layer's atomic current-symlink transition.
    final = release_root(data_dir, collection)
    final.parent.mkdir(parents=True, exist_ok=True)
    shutil.rmtree(tmp, ignore_errors=True)
    publish = subprocess.run(
        [str(codb), "publish", "--candidate", str(ds_root), "--target", str(final),
         "--collection", collection],
        capture_output=True, text=True,
    )
    if publish.returncode != 0:
        raise CorpusError(
            f"atomic release publication failed; inspect {work} and {final}; "
            "the active pointer is preserved when a prior version exists: "
            f"{publish.stdout[-500:]}{publish.stderr[-500:]}"
        )
    r = subprocess.run([str(codb), "verify", "--dataset", str(final), "--deep"],
                       capture_output=True, text=True)
    problem = verify_release_files(final)
    if r.returncode != 0 or problem:
        raise CorpusError(
            f"post-publication verification failed; active pointer is preserved at {final}: "
            f"{problem or ''} {r.stdout[-500:]}{r.stderr[-500:]}"
        )
    problem = verify_release_compatibility(final, collection, cat)
    if problem:
        raise CorpusError(f"published release is incompatible: {problem}")
    shutil.rmtree(work, ignore_errors=True)
    log(f"[build] atomically published {final}")
    return 0


def _extract_json(text: str) -> str:
    start = text.index("{")
    end = text.rindex("}") + 1
    return text[start:end]


ATTRIBUTION = {
    "masters-otb": (
        "This dataset is derived from the Lichess broadcast PGN archives published at "
        "https://database.lichess.org/#broadcasts under the Creative Commons "
        "Attribution-ShareAlike 4.0 International license (CC BY-SA 4.0), "
        "https://creativecommons.org/licenses/by-sa/4.0/ .\n"
        "Changes made: games were filtered (FIDE-rating thresholds, standard chess only, "
        "event-level verified OTB tournament allowlist, online/unresolved event exclusion, "
        "minimum length), deduplicated, and re-encoded into the CGB3 "
        "binary container; position statistics were aggregated into a CPI v2 index.\n"
        "ShareAlike: redistributed adaptations of this dataset must be licensed under CC BY-SA 4.0 "
        "or a compatible license.\n"
    ),
    "elite-online": (
        "This dataset is derived from the Lichess standard rated games database published at "
        "https://database.lichess.org/ , which Lichess releases under Creative Commons CC0 1.0 "
        "(public domain dedication). Attribution is not legally required; provenance is preserved "
        "here for transparency.\n"
        "Ratings in this dataset are Lichess Glicko-2 ratings, not FIDE Elo, and must never be "
        "pooled or compared with the masters-otb collection.\n"
    ),
}


def write_release_files(cat, collection, profile, sources, ds_root, version_dir, bj, index_report,
                        qj, metrics, depth, args, codb, masters_event_audit=None) -> None:
    lic = cat["licenses"][sources[0]["license"]]
    rel_dir = version_dir / "RELEASE"
    rel_dir.mkdir(exist_ok=True)
    event_audit = None
    if collection == "masters-otb" and masters_event_audit is not None:
        if not masters_event_audit.is_file():
            raise CorpusError("Masters release is missing its event-level qualification audit")
        shutil.copy2(masters_event_audit, rel_dir / "MASTERS_EVENT_QUALIFICATION.json")
        event_audit = json.loads(masters_event_audit.read_text())
    elif (collection == "masters-otb"
          and "exact event-level verified OTB allowlist" in cat["collections"][collection]["curation_policy_version"]):
        raise CorpusError("Current Masters release policy requires an event-level qualification audit")
    artifacts = {}
    for p in sorted(version_dir.rglob("*")):
        if p.is_file() and not p.name.startswith("."):
            artifacts[str(p.relative_to(version_dir))] = {
                "sha256": sha256_file(p), "size_bytes": p.stat().st_size,
            }
    manifest = json.loads((version_dir / "manifest.json").read_text())
    cat_bytes = json.dumps(cat, sort_keys=True).encode()
    release = {
        "release_schema_version": 1,
        "collection": collection,
        "dataset_version": manifest["dataset_build_id"],
        "built_at": utc_now(),
        "build_configuration": {
            "profile": profile,
            "index_depth_plies": depth,
            "memory_limit_mib": args.memory_limit_mib,
            "dedupe_mode": "disk",
            "block_capacity": 512,
            "zstd_level": 3,
            "source_order": [s["id"] for s in sources],
        },
        "source_catalog": {
            "catalog_id": cat["catalog_id"],
            "catalog_content_sha256": hashlib.sha256(cat_bytes).hexdigest(),
        },
        "inputs": [
            {"id": s["id"], "filename": s["filename"], "period": s["period"], "url": s["url"],
             "sha256": s["publisher_sha256"], "size_bytes": s["expected_size_bytes"],
             "license": s["license"], "provenance_confidence": s["provenance_confidence"]}
            for s in sources
        ],
        "curation_policy": {
            "version": cat["collections"][collection]["curation_policy_version"],
            "metadata": manifest["curation_policy"],
            "deduplication_policy_version": manifest["deduplication_policy_version"],
        },
        "accounting": manifest["accounting"],
        "accepted_games": manifest["total_games"],
        "index": {
            "depth_plies": depth,
            "unique_positions": index_report.get("unique_positions"),
            "total_move_records": index_report.get("total_move_records"),
            "cpi_sha256": index_report.get("cpi_sha256"),
            "cpi_file_size_bytes": index_report.get("cpi_file_size_bytes"),
        },
        "artifacts": artifacts,
        "license": {
            "id": sources[0]["license"], "name": lic["name"], "url": lic["url"],
            "attribution_required": lic["attribution_required"],
            "share_alike": lic["share_alike"],
            "notice_file": "RELEASE/DATASET_LICENSE.txt",
            "software_license": "MIT OR Apache-2.0 (software only; the dataset license is independent)",
        },
        "tool": {
            "git_commit": git_commit(),
            "git_tree_dirty": git_dirty(),
            "codb_version": _codb_version(codb),
        },
        "commands": {
            "fetch": f"./scripts/corpus.sh fetch --collection {collection} --profile {profile}",
            "build": f"./scripts/corpus.sh build --collection {collection} --profile {profile}",
            "verify": f"codb verify --dataset <release-dir> --deep",
            "serve": "./scripts/corpus.sh serve",
        },
        "build_metrics_summary": {
            "total_wall_seconds": metrics["total_wall_seconds"],
            "peak_rss_bytes_by_step": {k: v["peak_rss_bytes"] for k, v in metrics["steps"].items()},
        },
    }
    if event_audit is not None:
        release["masters_event_qualification"] = {
            "status": "verified_otb_subset",
            "audit_file": "RELEASE/MASTERS_EVENT_QUALIFICATION.json",
            "audit_sha256": sha256_file(rel_dir / "MASTERS_EVENT_QUALIFICATION.json"),
            "baseline_audit_game_counts": event_audit["totals"],
            "release_rejections": {
                "verified_online_event": bj.get("policy_rejection_reasons", {}).get(
                    "online_event_in_masters_source", 0
                ),
                "unresolved_masters_provenance": bj.get("policy_rejection_reasons", {}).get(
                    "unresolved_masters_provenance", 0
                ),
            },
        }
    (rel_dir / "RELEASE.json").write_text(json.dumps(release, indent=2, sort_keys=True) + "\n")
    notice = (
        f"Dataset: {collection} ({release['dataset_version']})\n"
        f"License: {lic['name']} ({lic['url']})\n\n{ATTRIBUTION[collection]}\n"
        "The chess-openings-db software is licensed MIT OR Apache-2.0; that license does not\n"
        "apply to this dataset. Source archives are listed with checksums in RELEASE/RELEASE.json.\n"
        "No claim is made that this license covers material outside the listed Lichess archives.\n"
    )
    (rel_dir / "DATASET_LICENSE.txt").write_text(notice)


def _codb_version(codb: Path) -> str:
    r = subprocess.run([str(codb), "--version"], capture_output=True, text=True)
    return r.stdout.strip() or "unknown"


# --------------------------------------------------------------------------- #
# Verify releases / reports
# --------------------------------------------------------------------------- #
def refresh_release_artifacts(version_dir: Path) -> None:
    """Hash all immutable release files except RELEASE.json, which contains this inventory."""
    release_path = version_dir / "RELEASE" / "RELEASE.json"
    release = json.loads(release_path.read_text())
    artifacts = {}
    for path in sorted(version_dir.rglob("*")):
        relative = path.relative_to(version_dir).as_posix()
        if (path.is_file() and relative != "RELEASE/RELEASE.json"
                and not path.name.startswith(".") and not path.name.endswith(".lock")):
            artifacts[relative] = {"sha256": sha256_file(path), "size_bytes": path.stat().st_size}
    release["artifacts"] = artifacts
    release_path.write_text(json.dumps(release, indent=2, sort_keys=True) + "\n")


def verify_release_compatibility(root: Path, expected_collection: str,
                                 catalog: dict[str, Any]) -> str | None:
    """Check the complete release envelope against the active catalog contract."""
    try:
        version_dir = current_version_dir(root)
        release = json.loads((version_dir / "RELEASE" / "RELEASE.json").read_text())
        if release.get("release_schema_version") != 1:
            return "unsupported or missing release schema version"
        if release.get("collection") != expected_collection:
            return "release collection identity does not match the requested collection"
        if release.get("dataset_version") != version_dir.name:
            return "release dataset version does not match its immutable version directory"
        expected_policy = catalog["collections"][expected_collection]["curation_policy_version"]
        if release.get("curation_policy", {}).get("version") != expected_policy:
            return f"release curation policy is incompatible with catalog policy {expected_policy}"
        source_catalog = release.get("source_catalog")
        if (not isinstance(source_catalog, dict)
                or not isinstance(source_catalog.get("catalog_id"), str)
                or not source_catalog.get("catalog_id")
                or not SHA256_RE.fullmatch(source_catalog.get("catalog_content_sha256", ""))):
            return "release source catalog provenance is missing or malformed"

        required = (
            "manifest.json", "games.idx", "positions.cpi",
            "RELEASE/DATASET_LICENSE.txt", "RELEASE/build_report.json",
            "RELEASE/index_report.json", "RELEASE/quality_report.json",
            "RELEASE/metrics.json", "RELEASE/sources.manifest.json",
        )
        missing = [name for name in required if not (version_dir / name).is_file()]
        if missing:
            return f"release is incomplete; missing {', '.join(missing)}"
        artifact_names = release.get("artifacts")
        core_artifacts = ("manifest.json", "games.idx", "positions.cpi")
        if not isinstance(artifact_names, dict) or any(name not in artifact_names for name in core_artifacts):
            return "release checksum inventory omits a core dataset artifact"
        if not list((version_dir / "shards").glob("*.cgb3")):
            return "release is incomplete; no CGB3 shard is present"

        manifest = json.loads((version_dir / "manifest.json").read_text())
        if (manifest.get("collection") != expected_collection
                or manifest.get("dataset_build_id") != version_dir.name
                or manifest.get("is_complete") is not True):
            return "dataset manifest is incomplete or has a mismatched identity"
        position_index = manifest.get("position_index")
        index = release.get("index", {})
        if (not isinstance(position_index, dict)
                or position_index.get("filename") != "positions.cpi"
                or position_index.get("sha256") != index.get("cpi_sha256")
                or position_index.get("indexed_depth_plies") != index.get("depth_plies")):
            return "release CPI metadata is missing or does not match the dataset manifest"
        if (manifest.get("total_games") != release.get("accepted_games")
                or manifest.get("accounting") != release.get("accounting")):
            return "release game counts or accounting do not match the dataset manifest"

        source_rows = {source["id"]: source for source in catalog["sources"]}
        inputs = release.get("inputs")
        if not isinstance(inputs, list) or not inputs:
            return "release has no pinned source inputs"
        for item in inputs:
            source = source_rows.get(item.get("id"))
            if (source is None or source.get("collection") != expected_collection
                    or source.get("publisher_sha256") != item.get("sha256")
                    or source.get("license") != item.get("license")):
                return f"release input {item.get('id')} is not compatible with the configured source catalog"
        allowed_licenses = catalog["collections"][expected_collection]["allowed_licenses"]
        if any(item.get("license") not in allowed_licenses for item in inputs):
            return "release uses a license that is not approved for this collection"

        strict_masters = (
            expected_collection == "masters-otb"
            and "exact event-level verified OTB allowlist" in expected_policy
        )
        if strict_masters:
            qualification = release.get("masters_event_qualification")
            if (not isinstance(qualification, dict)
                    or qualification.get("status") != "verified_otb_subset"
                    or qualification.get("audit_file") != "RELEASE/MASTERS_EVENT_QUALIFICATION.json"):
                return "Masters release is missing its verified OTB event qualification"
            audit_path = version_dir / qualification["audit_file"]
            audit = json.loads(audit_path.read_text())
            if audit.get("schema_version") != 1:
                return "Masters event audit has an unsupported schema"
            audit_input = audit.get("input")
            if (not isinstance(audit_input, dict)
                    or not isinstance(audit_input.get("source_dataset_version"), str)
                    or not audit_input.get("source_dataset_version")
                    or not SHA256_RE.fullmatch(audit_input.get("sha256", ""))
                    or not isinstance(audit_input.get("games"), int)
                    or audit_input["games"] <= 0):
                return "Masters event audit source release provenance is missing or malformed"
            audit_method = audit.get("method", {})
            if (not isinstance(audit_method, dict)
                    or audit_method.get("metadata_source") != "Lichess Broadcast API tournament metadata plus source PGN event, site, and broadcast URL tags"
                    or not audit_method.get("external_sources")):
                return "Masters event audit does not identify its official and linked evidence sources"
            policy = audit.get("release_policy", {})
            online = policy.get("verified_online_tournament_slugs")
            otb = policy.get("verified_otb_tournament_slugs")
            if (policy.get("require_verified_otb_event") is not True
                    or not isinstance(online, list) or not isinstance(otb, list) or not otb
                    or len(set(online)) != len(online) or len(set(otb)) != len(otb)
                    or set(online).intersection(otb)):
                return "Masters event audit allowlists are missing, duplicated, or conflicting"
            classes = {"verified_otb", "verified_online", "unresolved"}
            totals = audit.get("totals", {})
            events = audit.get("events")
            if (set(totals) != classes or not isinstance(events, list)
                    or any(event.get("classification") not in classes for event in events)):
                return "Masters event audit does not report all three required classifications"
            derived_totals = {name: {"games": 0, "events": 0} for name in classes}
            for event in events:
                game_count = event.get("game_count")
                if not isinstance(game_count, int) or game_count < 0:
                    return "Masters event audit contains an invalid game count"
                classification = event["classification"]
                if classification in ("verified_otb", "verified_online"):
                    metadata = event.get("metadata")
                    metadata_hash = event.get("metadata_sha256")
                    canonical = json.dumps(
                        metadata, ensure_ascii=False, sort_keys=True, separators=(",", ":")
                    ).encode() if isinstance(metadata, dict) else b""
                    if (not isinstance(metadata, dict)
                            or not SHA256_RE.fullmatch(metadata_hash or "")
                            or hashlib.sha256(canonical).hexdigest() != metadata_hash
                            or not str(event.get("metadata_url", "")).startswith(
                                "https://lichess.org/api/broadcast/"
                            )
                            or not event.get("evidence")):
                        return "Masters event audit classification lacks hashed official event evidence"
                if classification == "verified_otb":
                    info = event.get("metadata", {}).get("tour", {}).get("info", {})
                    external = event.get("external_event_evidence")
                    linked = event.get("linked_event_evidence")
                    if (not isinstance(info, dict) or not isinstance(info.get("location"), str)
                            or not info["location"].strip() or not isinstance(external, dict)
                            or external.get("verified") is not True
                            or not SHA256_RE.fullmatch(external.get("sha256", ""))
                            or not isinstance(linked, list)
                            or not any(
                                isinstance(page, dict) and page.get("verified") is True
                                and page.get("sha256") == external.get("sha256")
                                for page in linked
                            )):
                        return "Masters verified OTB event lacks a corroborated venue and linked source hash"
                derived_totals[classification]["games"] += game_count
                derived_totals[classification]["events"] += 1
            if (derived_totals != totals
                    or sum(item["games"] for item in derived_totals.values())
                    != audit.get("input", {}).get("games")):
                return "Masters event audit game and event totals do not reconcile"
            expected_online = sorted(
                event["tournament_slug"] for event in events
                if event["classification"] == "verified_online" and event.get("tournament_slug")
            )
            expected_otb = sorted(
                event["tournament_slug"] for event in events
                if event["classification"] == "verified_otb" and event.get("tournament_slug")
            )
            if sorted(online) != expected_online or sorted(otb) != expected_otb:
                return "Masters event audit allowlists do not match its event classifications"
            if qualification.get("baseline_audit_game_counts") != totals:
                return "Masters release event totals do not match the attached audit"
            if qualification.get("audit_sha256") != sha256_file(audit_path):
                return "Masters event audit checksum does not match release provenance"
            build_report = json.loads((version_dir / "RELEASE" / "build_report.json").read_text())
            reasons = build_report.get("policy_rejection_reasons", {})
            expected_rejections = {
                "verified_online_event": reasons.get("online_event_in_masters_source", 0),
                "unresolved_masters_provenance": reasons.get("unresolved_masters_provenance", 0),
            }
            if qualification.get("release_rejections") != expected_rejections:
                return "Masters event rejection totals do not match the build report"
        return None
    except (AttributeError, KeyError, OSError, TypeError, ValueError, json.JSONDecodeError) as error:
        return f"release metadata is invalid: {error}"


def cmd_verify(args: argparse.Namespace) -> int:
    data_dir = data_dir_from(args)
    codb = find_codb(args.codb)
    catalog = load_catalog(Path(args.catalog))
    collections = COLLECTIONS if args.collection == "all" else (args.collection,)
    failures = 0
    for c in collections:
        root = release_root(data_dir, c)
        r = subprocess.run([str(codb), "verify", "--dataset", str(root), "--deep"],
                           capture_output=True, text=True)
        ok = r.returncode == 0 and "Result: SUCCESS" in r.stdout
        verdict = verify_release_files(root)
        if verdict is None:
            verdict = verify_release_compatibility(root, c, catalog)
        log(f"[verify] {c}: codb verify --deep {'PASS' if ok else 'FAIL'}; "
            f"release artifacts {'PASS' if verdict is None else 'FAIL: ' + verdict}")
        failures += 0 if ok and verdict is None else 1
    if failures:
        raise CorpusError(f"{failures} release(s) failed verification")
    return 0


def verify_release_files(root: Path) -> str | None:
    """Re-hash every artifact listed in RELEASE.json. Returns an error string or None."""
    try:
        vdir = current_version_dir(root)
        rj = vdir / "RELEASE" / "RELEASE.json"
        if not rj.is_file():
            return "current/RELEASE/RELEASE.json missing"
        release = json.loads(rj.read_text())
        version = release.get("dataset_version")
        if not isinstance(version, str) or vdir.name != version:
            return f"current version {vdir.name} does not match release {version}"
        artifacts = release.get("artifacts")
        if not isinstance(artifacts, dict) or not artifacts:
            return "release artifact checksum inventory is missing or empty"
        root_resolved = vdir.resolve()
        for rel, meta in artifacts.items():
            relative = PurePosixPath(rel)
            if relative.is_absolute() or not relative.parts or any(part in ("", ".", "..") for part in relative.parts):
                return f"unsafe release artifact path: {rel}"
            path = vdir.joinpath(*relative.parts)
            resolved = path.resolve(strict=False)
            if not resolved.is_relative_to(root_resolved):
                return f"release artifact path escapes the immutable version: {rel}"
            if not path.is_file():
                return f"artifact missing: {rel}"
            if (not isinstance(meta, dict) or path.stat().st_size != meta.get("size_bytes")
                    or sha256_file(path) != meta.get("sha256")):
                return f"artifact checksum mismatch: {rel}"
        return None
    except (KeyError, OSError, TypeError, ValueError, json.JSONDecodeError) as error:
        return f"release artifact metadata is invalid: {error}"


def cmd_report(args: argparse.Namespace) -> int:
    data_dir = data_dir_from(args)
    codb = find_codb(args.codb)
    collections = COLLECTIONS if args.collection == "all" else (args.collection,)
    for c in collections:
        out = data_dir / "reports" / f"{c}.quality.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run([str(codb), "report", "--dataset", str(release_root(data_dir, c)),
                        "--output", str(out)], check=True)
        log(f"[report] wrote {out}")
    return 0


# --------------------------------------------------------------------------- #
# Packaging
# --------------------------------------------------------------------------- #
def cmd_package(args: argparse.Namespace) -> int:
    data_dir = data_dir_from(args)
    codb = find_codb(args.codb)
    c = args.collection
    root = release_root(data_dir, c)
    problem = verify_release_files(root)
    catalog = load_catalog(Path(args.catalog))
    if problem is None:
        problem = verify_release_compatibility(root, c, catalog)
    if problem:
        raise CorpusError(f"refusing to package an unverified or incompatible release: {problem}")
    vdir = current_version_dir(root)
    release = json.loads((vdir / "RELEASE" / "RELEASE.json").read_text())
    out_dir = Path(args.output_dir) if args.output_dir else data_dir / "packages"
    out_dir.mkdir(parents=True, exist_ok=True)
    archive = out_dir / f"{c}-{vdir.name}.tar"
    tmp = archive.with_suffix(".tar.part")

    def add(tf: tarfile.TarFile, src: Path, arcname: str) -> None:
        ti = tf.gettarinfo(str(src), arcname=arcname)
        ti.uid = ti.gid = 0
        ti.uname = ti.gname = ""
        ti.mtime = 0
        ti.mode = 0o755 if (ti.isdir() or ti.issym()) else 0o644
        if ti.isreg():
            with open(src, "rb") as f:
                tf.addfile(ti, f)
        else:
            tf.addfile(ti)

    with tarfile.open(tmp, "w", format=tarfile.PAX_FORMAT) as tf:
        names: list[tuple[Path, str]] = []
        for p in sorted(vdir.rglob("*")):
            if p.name.startswith(".") or p.name.endswith(".lock"):
                continue  # exclude lock files and temporary build files
            names.append((p, f"{c}/versions/{vdir.name}/{p.relative_to(vdir)}"))
        for p in sorted((vdir / "RELEASE").rglob("*")):
            names.append((p, f"{c}/RELEASE/{p.relative_to(vdir / 'RELEASE')}"))
        # symlinks that make the release root a valid dataset root
        for link in ("current", "games.idx", "manifest.json", "positions.cpi", "shards"):
            lp = root / link
            if lp.is_symlink():
                names.append((lp, f"{c}/{link}"))
        for src, arc in sorted(names, key=lambda t: t[1]):
            add(tf, src, arc)
    os.replace(tmp, archive)
    digest = sha256_file(archive)
    (out_dir / (archive.name + ".sha256")).write_text(f"{digest}  {archive.name}\n")
    log(f"[package] {archive} ({archive.stat().st_size} bytes) sha256 {digest}")

    # Independent verification: extract into a fresh directory and re-open it.
    verify_package(archive, codb, expected_games=release["accepted_games"], catalog=catalog)
    log(f"[package] extraction and deep verification PASSED for {archive.name}")
    return 0


def info_collection(root: Path) -> str:
    release_path = current_version_dir(root) / "RELEASE" / "RELEASE.json"
    return json.loads(release_path.read_text())["collection"]


def verify_package(archive: Path, codb: Path, expected_games: int | None = None,
                   catalog: dict[str, Any] | None = None) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="codb-pkg-", dir=archive.parent) as td:
        root = extract_release_archive(archive, Path(td))
        problem = verify_release_files(root)
        if problem is None and catalog is not None:
            problem = verify_release_compatibility(root, info_collection(root), catalog)
        if problem:
            raise CorpusError(f"extracted package failed artifact or compatibility verification: {problem}")
        r = subprocess.run([str(codb), "verify", "--dataset", str(root), "--deep"],
                           capture_output=True, text=True)
        if r.returncode != 0 or "Result: SUCCESS" not in r.stdout:
            raise CorpusError(f"extracted package failed codb verify --deep:\n{r.stdout}{r.stderr}")
        info = json.loads((current_version_dir(root) / "RELEASE" / "RELEASE.json").read_text())
        if expected_games is not None and info["accepted_games"] != expected_games:
            raise CorpusError("extracted release game count differs from the source release")
        q = codb_json(codb, ["query", "--dataset", str(root), "--moves", "e4", "--json"])
        if "total_games" not in q:
            raise CorpusError("extracted package query returned no statistics")
        return info


def extract_release_archive(archive: Path, destination: Path,
                            expected_collection: str | None = None) -> Path:
    """Convert package corruption and I/O errors into a fail-closed diagnostic."""
    try:
        return _extract_release_archive_unchecked(archive, destination, expected_collection)
    except CorpusError:
        raise
    except (OSError, tarfile.TarError, ValueError, TypeError, KeyError) as error:
        raise CorpusError(f"invalid release package {archive}: {error}") from error


def _extract_package_members_safely(tf: tarfile.TarFile, members: list[tarfile.TarInfo],
                                    destination: Path) -> None:
    directory_members = [member for member in members if member.isdir()]
    file_members = [
        member for member in members
        if member.type in (tarfile.REGTYPE, tarfile.AREGTYPE)
    ]
    symlink_members = [member for member in members if member.issym()]

    def path_for(member: tarfile.TarInfo) -> Path:
        return destination.joinpath(*PurePosixPath(member.name).parts)

    def ensure_parent_directories(member: tarfile.TarInfo) -> None:
        parts = PurePosixPath(member.name).parts
        parent = destination
        for part in parts[:-1]:
            parent = parent / part
            if os.path.lexists(parent):
                if parent.is_symlink() or not parent.is_dir():
                    raise CorpusError(f"package member traverses a non-directory path: {member.name}")
            else:
                parent.mkdir(mode=0o755)

    for member in sorted(directory_members, key=lambda item: (len(PurePosixPath(item.name).parts), item.name)):
        ensure_parent_directories(member)
        target = path_for(member)
        if os.path.lexists(target):
            if target.is_symlink() or not target.is_dir():
                raise CorpusError(f"package directory conflicts with an existing entry: {member.name}")
        else:
            target.mkdir(mode=0o755)

    for member in file_members:
        ensure_parent_directories(member)
        target = path_for(member)
        if os.path.lexists(target):
            raise CorpusError(f"package file conflicts with an existing entry: {member.name}")
        source = tf.extractfile(member)
        if source is None:
            raise CorpusError(f"package file has no extractable data: {member.name}")
        with source, target.open("xb") as output:
            remaining = member.size
            while remaining:
                block = source.read(min(CHUNK, remaining))
                if not block:
                    raise CorpusError(f"package file is truncated: {member.name}")
                output.write(block)
                remaining -= len(block)
        os.chmod(target, 0o644)

    for member in symlink_members:
        ensure_parent_directories(member)
        target = path_for(member)
        if os.path.lexists(target):
            raise CorpusError(f"package symlink conflicts with an existing entry: {member.name}")
        os.symlink(member.linkname, target)


def _extract_release_archive_unchecked(archive: Path, destination: Path,
                                      expected_collection: str | None = None) -> Path:
    '''Verify the detached checksum and safely extract one complete release package.'''
    sums = archive.with_name(archive.name + ".sha256")
    if not sums.is_file():
        raise CorpusError(f"checksum file missing: {sums}")
    fields = sums.read_text().split()
    if len(fields) != 2 or not SHA256_RE.fullmatch(fields[0]) or fields[1] != archive.name:
        raise CorpusError(f"malformed package checksum file: {sums}")
    got = sha256_file(archive)
    if fields[0] != got:
        raise CorpusError(f"package checksum mismatch: {got} != {fields[0]}")

    with tarfile.open(archive, "r:*") as tf:
        members = tf.getmembers()
        if not members:
            raise CorpusError("package archive is empty")
        roots: set[str] = set()
        entry_kinds: dict[str, str] = {}
        for member in members:
            raw_parts = member.name.split("/")
            if member.isdir() and raw_parts and raw_parts[-1] == "":
                raw_parts.pop()
            if member.name.startswith("/") or not raw_parts or any(
                part in ("", ".", "..") for part in raw_parts
            ):
                raise CorpusError(f"unsafe package path: {member.name}")
            path = PurePosixPath(*raw_parts)
            roots.add(path.parts[0])
            normalized_name = "/".join(raw_parts)
            if normalized_name in entry_kinds:
                raise CorpusError(f"duplicate package path: {member.name}")
            if member.isdir():
                entry_kind = "directory"
            elif member.type in (tarfile.REGTYPE, tarfile.AREGTYPE):
                if member.size < 0:
                    raise CorpusError(f"package file has an invalid size: {member.name}")
                entry_kind = "file"
            elif member.issym():
                entry_kind = "symlink"
                link = PurePosixPath(member.linkname)
                resolved = posixpath.normpath(posixpath.join(str(path.parent), member.linkname))
                if link.is_absolute() or resolved == ".." or resolved.startswith("../"):
                    raise CorpusError(f"package symlink escapes release root: {member.name}")
            else:
                raise CorpusError(f"unsupported package entry type: {member.name}")
            entry_kinds[normalized_name] = entry_kind

        for name in entry_kinds:
            parts = name.split("/")
            for index in range(1, len(parts)):
                ancestor = "/".join(parts[:index])
                if entry_kinds.get(ancestor) in ("file", "symlink"):
                    raise CorpusError(
                        f"package member traverses a non-directory entry: {name}"
                    )
        if len(roots) != 1:
            raise CorpusError("package must contain exactly one top-level release directory")
        root_name = next(iter(roots))
        if root_name not in COLLECTIONS:
            raise CorpusError(f"unknown collection directory in package: {root_name}")
        if expected_collection and root_name != expected_collection:
            raise CorpusError(
                f"package collection '{root_name}' does not match requested '{expected_collection}'"
            )
        if destination.is_symlink():
            raise CorpusError("package extraction destination must not be a symlink")
        destination.mkdir(parents=True, exist_ok=True)
        if not destination.is_dir() or any(destination.iterdir()):
            raise CorpusError("package extraction destination must be an empty directory")
        destination = destination.resolve()
        try:
            _extract_package_members_safely(tf, members, destination)
        except (OSError, tarfile.TarError, ValueError) as error:
            raise CorpusError(f"could not safely extract release package: {error}") from error

    root = destination / root_name
    if not root.is_dir():
        raise CorpusError("package did not create its release directory")
    current = root / "current"
    if not current.is_symlink() or Path(os.readlink(current)).is_absolute():
        raise CorpusError("package is missing a relative current symlink")
    active = current_version_dir(root)
    if active.parent != (root / "versions").resolve():
        raise CorpusError("package current symlink does not point into its immutable versions directory")
    release_path = active / "RELEASE" / "RELEASE.json"
    legacy_release = root / "RELEASE"
    if not release_path.is_file() and legacy_release.is_dir() and not legacy_release.is_symlink():
        # Packages produced before immutable release metadata used a root RELEASE directory.
        # Normalize it into the extracted candidate snapshot before any publication.
        shutil.move(str(legacy_release), str(active / "RELEASE"))
        release_path = active / "RELEASE" / "RELEASE.json"
    if not release_path.is_file():
        raise CorpusError("package is missing immutable RELEASE/RELEASE.json metadata")
    release = json.loads(release_path.read_text())
    if release.get("release_schema_version") != 1:
        raise CorpusError("package has an unsupported release schema")
    if release.get("collection") != root_name or release.get("dataset_version") != active.name:
        raise CorpusError("package release identity does not match its collection and active version")
    if not (active / "RELEASE" / "DATASET_LICENSE.txt").is_file():
        raise CorpusError("package is missing its dataset licensing notice")
    return root


def cmd_bootstrap(args: argparse.Namespace) -> int:
    '''Install verified packages or explicitly fetch and rebuild the pinned source corpus.'''
    data_dir = data_dir_from(args)
    assert_untracked_location(data_dir)
    codb = find_codb(args.codb)
    catalog = load_catalog(Path(args.catalog))
    if args.rebuild:
        log("[bootstrap] fetching the pinned baseline sources; this downloads approximately 8.66 GB")
        fetch_args = argparse.Namespace(
            catalog=args.catalog, data_dir=str(data_dir), codb=args.codb, collection="all",
            profile=args.profile, source=None, max_download_bytes=args.max_download_bytes,
            offline=False, retries=4, backoff=2.0, min_free_bytes=2 * 1024**3,
            rehash=True, update_catalog=False,
        )
        cmd_fetch(fetch_args)
        for collection in COLLECTIONS:
            build_args = argparse.Namespace(
                catalog=args.catalog, data_dir=str(data_dir), codb=args.codb,
                collection=collection, profile=args.profile, depth_plies=None,
                memory_limit_mib=args.memory_limit_mib,
                masters_event_audit=DEFAULT_MASTERS_EVENT_AUDIT,
            )
            cmd_build(build_args)
        verify_args = argparse.Namespace(
            catalog=args.catalog, data_dir=str(data_dir), codb=args.codb, collection="all",
        )
        return cmd_verify(verify_args)

    if not args.masters_package or not args.elite_package:
        raise CorpusError("provide both --masters-package and --elite-package, or use --rebuild")
    packages = {
        "masters-otb": Path(args.masters_package),
        "elite-online": Path(args.elite_package),
    }
    staging = data_dir / "work" / f"bootstrap-{os.getpid()}-{time.time_ns()}"
    staging.mkdir(parents=True, exist_ok=False)
    try:
        extracted: dict[str, Path] = {}
        for collection, archive in packages.items():
            log(f"[bootstrap] verifying and extracting {collection} package {archive}")
            root = extract_release_archive(archive, staging / collection, collection)
            problem = verify_release_files(root)
            if problem is None:
                problem = verify_release_compatibility(root, collection, catalog)
            if problem:
                raise CorpusError(f"{collection} package failed release compatibility verification: {problem}")
            result = subprocess.run(
                [str(codb), "verify", "--dataset", str(root), "--deep"],
                capture_output=True, text=True,
            )
            if result.returncode != 0 or "Result: SUCCESS" not in result.stdout:
                raise CorpusError(
                    f"{collection} package failed deep verification:\n{result.stdout}{result.stderr}"
                )
            extracted[collection] = root

        for collection, root in extracted.items():
            target = release_root(data_dir, collection)
            target.parent.mkdir(parents=True, exist_ok=True)
            result = subprocess.run(
                [str(codb), "publish", "--candidate", str(root), "--target", str(target),
                 "--collection", collection],
                capture_output=True, text=True,
            )
            if result.returncode != 0:
                raise CorpusError(
                    f"could not publish {collection}; previous active version remains available: "
                    f"{result.stdout}{result.stderr}"
                )
            problem = verify_release_files(target)
            if problem:
                raise CorpusError(f"published {collection} failed release verification: {problem}")
            log(f"[bootstrap] installed {collection} at {current_version_dir(target)}")

        verify_args = argparse.Namespace(
            catalog=args.catalog, data_dir=str(data_dir), codb=args.codb, collection="all",
        )
        return cmd_verify(verify_args)
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def cmd_verify_package(args: argparse.Namespace) -> int:
    info = verify_package(Path(args.archive), find_codb(args.codb),
                          catalog=load_catalog(Path(args.catalog)))
    log(f"[verify-package] OK: {info['collection']} {info['dataset_version']} "
        f"({info['accepted_games']} games)")
    return 0


# --------------------------------------------------------------------------- #
# Serving
# --------------------------------------------------------------------------- #
DEFAULT_ORIGINS = "http://localhost:3000,http://localhost:5173"


def serve_command(codb: Path, masters: Path, elite: Path, host: str, port: int, origins: str,
                  extra: list[str] | None = None) -> list[str]:
    cmd = [str(codb), "serve", "--masters-dataset", str(masters), "--elite-dataset", str(elite),
           "--host", host, "--port", str(port)]
    if origins:
        cmd += ["--allowed-origins", origins]
    return cmd + (extra or [])


def cmd_serve(args: argparse.Namespace) -> int:
    data_dir = data_dir_from(args)
    codb = find_codb(args.codb)
    cmd = serve_command(codb, release_root(data_dir, "masters-otb"), release_root(data_dir, "elite-online"),
                        args.host, args.port, args.allowed_origins)
    log("[serve] " + " ".join(cmd))
    os.execv(cmd[0], cmd)
    return 0  # unreachable


# --------------------------------------------------------------------------- #
# HTTP integration verification (Phase H/I)
# --------------------------------------------------------------------------- #
class Http:
    def __init__(self, host: str, port: int) -> None:
        self.host, self.port = host, port
        self.conn = http.client.HTTPConnection(host, port, timeout=30)

    def get(self, path: str, headers: dict[str, str] | None = None) -> tuple[int, dict[str, str], bytes]:
        for attempt in (0, 1):
            try:
                self.conn.request("GET", path, headers=headers or {})
                r = self.conn.getresponse()
                body = r.read()
                return r.status, {k.lower(): v for k, v in r.getheaders()}, body
            except (http.client.HTTPException, ConnectionError, OSError):
                self.conn.close()
                self.conn = http.client.HTTPConnection(self.host, self.port, timeout=30)
                if attempt:
                    raise
        raise AssertionError("unreachable")

    def json(self, path: str) -> tuple[int, Any]:
        st, _, body = self.get(path)
        try:
            return st, json.loads(body)
        except json.JSONDecodeError:
            return st, body.decode("utf-8", "replace")


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def read_game_ids(version_dir: Path) -> list[int]:
    """Reads stable game IDs from games.idx (documented layout: 32B header, 16B records, 32B footer)."""
    data = (version_dir / "games.idx").read_bytes()
    n = int.from_bytes(data[24:32], "big")
    return [int.from_bytes(data[32 + i * 16: 40 + i * 16], "big") for i in range(n)]


SAN_RE = re.compile(r"^(?:O-O(?:-O)?|[KQRBN]?[a-h]?[1-8]?x?[a-h][1-8](?:=[QRBN])?)[+#]?$")


def parse_export_blocks(pgn: str) -> list[list[str]]:
    """Extracts SAN move lists from `codb export` output.

    Only the last paragraph of each record is read as movetext, and a record is
    skipped unless every token is a well-formed SAN move (this protects against
    multi-line tag values and stray annotations).
    """
    games: list[list[str]] = []
    for block in re.split(r"\n\n(?=\[Event )", pgn):
        paragraphs = [p for p in block.strip().split("\n\n") if p.strip()]
        if not paragraphs or paragraphs[-1].lstrip().startswith("["):
            continue
        movetext = re.sub(r"\{[^}]*\}", " ", paragraphs[-1])
        toks = [t for t in movetext.split() if not re.match(r"^\d+\.(\.\.)?$", t)
                and t not in ("1-0", "0-1", "1/2-1/2", "*")]
        if toks and all(SAN_RE.match(t) for t in toks):
            games.append(toks)
    return games


def sample_move_sequences(codb: Path, root: Path, max_games: int) -> list[list[str]]:
    r = subprocess.run([str(codb), "export", "--dataset", str(root), "--max-games", str(max_games)],
                       capture_output=True, text=True, check=True)
    return parse_export_blocks(r.stdout)


def percentile(sorted_vals: list[float], p: float) -> float:
    if not sorted_vals:
        return 0.0
    k = min(len(sorted_vals) - 1, max(0, int(round(p / 100 * (len(sorted_vals) - 1)))))
    return sorted_vals[k]


def lat_summary(samples_s: list[float]) -> dict[str, float]:
    v = sorted(x * 1e6 for x in samples_s)
    return {"count": len(v), "p50_us": round(percentile(v, 50), 1), "p95_us": round(percentile(v, 95), 1),
            "p99_us": round(percentile(v, 99), 1), "max_us": round(v[-1], 1) if v else 0.0}


def _load_worker(args: tuple) -> tuple[int, list[float]]:
    host, port, paths, seed, n = args
    cl = Http(host, port)
    errors, lat = 0, []
    for j in range(n):
        t = time.perf_counter()
        st, _, _ = cl.get(paths[(seed + j) % len(paths)])
        lat.append(time.perf_counter() - t)
        errors += st != 200
    return errors, lat


def run_load(host: str, port: int, paths: list[str], workers: int, per_worker: int) -> dict[str, Any]:
    """Closed-loop load from `workers` separate processes; returns throughput and latency percentiles."""
    import multiprocessing as mp
    t0 = time.perf_counter()
    with mp.get_context("fork").Pool(workers) as pool:
        parts = pool.map(_load_worker, [(host, port, paths, w * 31, per_worker) for w in range(workers)])
    elapsed = time.perf_counter() - t0
    lat = [x for _, l in parts for x in l]
    out = {"processes": workers, "requests": workers * per_worker, "errors": sum(e for e, _ in parts),
           "req_per_sec": round(workers * per_worker / elapsed, 1)}
    out.update({k: v for k, v in lat_summary(lat).items() if k != "count"})
    return out


class Checks:
    def __init__(self) -> None:
        self.results: list[dict[str, Any]] = []

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        self.results.append({"name": name, "ok": bool(ok), "detail": detail})
        if not ok:
            log(f"  FAIL {name}: {detail}")

    @property
    def failed(self) -> list[dict[str, Any]]:
        return [r for r in self.results if not r["ok"]]


def pos_url(collection: str, moves: list[str] | None = None, fen: str | None = None) -> str:
    from urllib.parse import quote
    if fen is not None:
        return f"/v1/explorer/position?collection={collection}&fen={quote(fen)}"
    return f"/v1/explorer/position?collection={collection}&moves={quote(','.join(moves or []))}"


START_FEN = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"


def cli_query(codb: Path, root: Path, moves: list[str]) -> dict[str, Any]:
    if moves:
        return codb_json(codb, ["query", "--dataset", str(root), "--moves", ",".join(moves), "--json"])
    return codb_json(codb, ["query", "--dataset", str(root), "--fen", START_FEN, "--json"])


def normalize_moves(ms: list[dict[str, Any]]) -> list[tuple]:
    return sorted((m["uci"], m["white_wins"], m["draws"], m["black_wins"]) for m in ms)


def stratified_cases(games: list[list[str]], depth: int, per_stratum: int = 8) -> dict[str, list[list[str]]]:
    from collections import Counter
    prefix_count: Counter = Counter()
    for g in games:
        for n in range(1, min(len(g), depth) + 1):
            prefix_count[tuple(g[:n])] += 1
    items = sorted(prefix_count.items(), key=lambda kv: (-kv[1], kv[0]))
    common = [list(k) for k, v in items if 2 <= len(k) <= 6][:per_stratum]
    mid = [list(k) for k, v in items if 8 <= len(k) <= 14 and 3 <= v <= 30][:per_stratum]
    rare_pool = [list(k) for k, v in items if v == 1 and len(k) >= max(12, depth - 6)]
    step = max(1, len(rare_pool) // max(per_stratum, 1))
    rare = rare_pool[::step][:per_stratum]
    beyond = []
    for g in games:
        if len(g) > depth + 4:
            beyond.append(g[: depth + 4])
        if len(beyond) >= 3:
            break
    absent = [["a3", "a6", "h3", "h6", "a4", "a5", "h4", "h5", "Ra3", "Ra7"],
              ["h4", "h5", "a4", "a5", "Rh3", "Rh6", "Ra3", "Ra6", "Rb3", "Rb6"]]
    return {"common": common, "mid": mid, "rare": rare, "beyond_depth": beyond, "absent_candidates": absent}


TRANSPOSITIONS = [
    (["d4", "Nf6", "c4", "e6"], ["c4", "e6", "d4", "Nf6"]),
    (["e4", "e5", "Nf3", "Nc6"], ["Nf3", "Nc6", "e4", "e5"]),
    (["d4", "d5", "c4", "e6", "Nc3", "Nf6"], ["d4", "Nf6", "c4", "e6", "Nc3", "d5"]),
]


def run_http_checks(codb: Path, roots: dict[str, Path], host: str, port: int, serve_log: Path,
                    depth_by_col: dict[str, int], requests_per_col: int = 400,
                    sample_games: int = 3000, extra_serve_args: list[str] | None = None,
                    per_stratum: int = 8) -> dict[str, Any]:
    checks = Checks()
    report: dict[str, Any] = {"host": host, "port": port, "collections": {}}
    cmd = serve_command(codb, roots["masters-otb"], roots["elite-online"], host, port,
                        DEFAULT_ORIGINS, extra_serve_args)
    t_start = time.monotonic()
    with open(serve_log, "wb") as lf:
        proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT)
    try:
        http_c = Http(host, port)
        ready_at = None
        for _ in range(600):
            try:
                st, _ = http_c.json("/ready")
                if st == 200:
                    ready_at = time.monotonic() - t_start
                    break
            except OSError:
                pass
            if proc.poll() is not None:
                raise CorpusError(f"codb serve exited early (see {serve_log})")
            time.sleep(0.05)
        if ready_at is None:
            raise CorpusError("server did not become ready")
        report["startup_seconds"] = round(ready_at, 3)

        st, health = http_c.json("/health")
        checks.check("GET /health", st == 200 and health.get("status") == "ok", f"{st} {health}")
        st, ready = http_c.json("/ready")
        checks.check("GET /ready", st == 200 and sorted(ready.get("collections_loaded", [])) == sorted(COLLECTIONS),
                     f"{st} {ready}")
        st, colls = http_c.json("/v1/collections")
        meta = {c["collection"]: c for c in colls.get("collections", [])} if st == 200 else {}
        checks.check("GET /v1/collections lists both", sorted(meta) == sorted(COLLECTIONS), f"{st} {sorted(meta)}")

        manifests = {c: json.loads((current_version_dir(roots[c]) / "manifest.json").read_text()) for c in COLLECTIONS}
        ids_by_col = {c: read_game_ids(current_version_dir(roots[c])) for c in COLLECTIONS}
        for c in COLLECTIONS:
            m = manifests[c]
            mc = meta.get(c, {})
            checks.check(f"{c}: snapshot metadata", mc.get("dataset_version") == m["dataset_build_id"]
                         and mc.get("total_games") == m["total_games"] and mc.get("cpi_available") is True
                         and mc.get("cpi_depth_plies") == depth_by_col[c], str(mc))

        for c in COLLECTIONS:
            root, depth = roots[c], depth_by_col[c]
            cr: dict[str, Any] = {"cases": 0, "mismatches": 0, "strata": {}}
            report["collections"][c] = cr
            version = manifests[c]["dataset_build_id"]

            def compare(label: str, moves: list[str]) -> dict[str, Any] | None:
                st, body = http_c.json(pos_url(c, moves) if moves else pos_url(c, fen=START_FEN))
                cli = cli_query(codb, root, moves)
                cr["cases"] += 1
                if st != 200:
                    cr["mismatches"] += 1
                    checks.check(f"{c} {label} {moves}: HTTP 200", False, f"{st} {body}")
                    return None
                ok = (body["collection"] == c and body["dataset_version"] == version
                      and body["white_wins"] + body["draws"] + body["black_wins"] == body["total_games"]
                      and body["total_games"] == cli["total_games"]
                      and (body["white_wins"], body["draws"], body["black_wins"])
                      == (cli["white_wins"], cli["draws"], cli["black_wins"])
                      and normalize_moves(body["moves"]) == normalize_moves(cli["moves"]))
                for mv in body["moves"]:
                    ok = ok and mv["white_wins"] + mv["draws"] + mv["black_wins"] == mv["total_games"]
                ok = ok and sum(mv["total_games"] for mv in body["moves"]) <= body["total_games"]
                if not ok:
                    cr["mismatches"] += 1
                checks.check(f"{c} {label} {'/'.join(moves) or 'initial'}: semantics + HTTP==CLI", ok,
                             json.dumps(body)[:300] if not ok else "")
                return body

            init = compare("initial", [])
            if init:
                checks.check(f"{c}: initial position nonzero", init["total_games"] > 0)
                checks.check(f"{c}: initial total equals accepted games minus unknown results",
                             init["total_games"] <= manifests[c]["total_games"], str(init["total_games"]))
                # candidate legality: every candidate move must be accepted by the API as a legal move
                legal_ok = True
                for mv in init["moves"][:10]:
                    st, child = http_c.json(pos_url(c, [mv["san"]]))
                    legal_ok = legal_ok and st == 200 and child["total_games"] >= mv["total_games"]
                checks.check(f"{c}: initial candidate moves are legal and child totals >= candidate totals", legal_ok)

            games = sample_move_sequences(codb, root, sample_games)
            strata = stratified_cases(games, depth, per_stratum)
            absent = None
            for cand in strata["absent_candidates"]:
                if cli_query(codb, root, cand)["total_games"] == 0:
                    absent = cand
                    break
            for label in ("common", "mid", "rare", "beyond_depth"):
                cr["strata"][label] = len(strata[label])
                for mvs in strata[label]:
                    body = compare(label, mvs)
                    if label == "common" and body:
                        checks.check(f"{c} common {'/'.join(mvs)}: nonzero", body["total_games"] > 0)
                    if label == "beyond_depth" and body:
                        checks.check(f"{c} beyond depth {depth}: absent from index", body["total_games"] == 0,
                                     str(body["total_games"]))
            if absent:
                body = compare("absent", absent)
                cr["strata"]["absent"] = 1
                if body:
                    checks.check(f"{c}: absent position has zero stats", body["total_games"] == 0)
            for a, b in TRANSPOSITIONS:
                ba, bb = compare("transposition-A", a), compare("transposition-B", b)
                if ba and bb:
                    same = (ba["fen"].split()[:4] == bb["fen"].split()[:4]
                            and ba["total_games"] == bb["total_games"])
                    checks.check(f"{c}: transposition {'/'.join(a)} == {'/'.join(b)}", same,
                                 f"{ba['total_games']} vs {bb['total_games']}")
            cr["strata"]["transposition_pairs"] = len(TRANSPOSITIONS)

            # error behaviour
            for label, path, want in [
                ("invalid FEN", pos_url(c, fen="not a fen"), 400),
                ("illegal FEN (no kings)", pos_url(c, fen="8/8/8/8/8/8/8/8 w - - 0 1"), 400),
                ("illegal move", pos_url(c, ["e5"]), 400),
                ("fen and moves", f"/v1/explorer/position?collection={c}&fen=x&moves=e4", 400),
                ("missing position", f"/v1/explorer/position?collection={c}", 400),
                ("unsupported since_year", f"/v1/explorer/position?collection={c}&moves=e4&since_year=2020", 400),
                ("unsupported min_rating", f"/v1/explorer/position?collection={c}&moves=e4&min_rating=2000", 400),
            ]:
                st, _ = http_c.json(path)
                checks.check(f"{c}: {label} -> {want}", st == want, f"got {st}")

            # game retrieval: real stored games
            ids = ids_by_col[c]
            sample_ids = [ids[0], ids[len(ids) // 2], ids[-1]]
            other = "elite-online" if c == "masters-otb" else "masters-otb"
            for gid in sample_ids:
                st, hdr, body = http_c.get(f"/v1/collections/{c}/games/{gid}")
                cli_pgn = subprocess.run([str(codb), "get-game", "--dataset", str(root), "--id", str(gid)],
                                         capture_output=True, text=True).stdout
                checks.check(f"{c}: game {gid} PGN retrieved and equals CLI",
                             st == 200 and body.decode().strip() == cli_pgn.strip() and "[White " in body.decode(),
                             f"status {st}")
                st2, _, _ = http_c.get(f"/v1/collections/{other}/games/{gid}")
                checks.check(f"{c}: game {gid} not visible in {other} (isolation)", st2 == 404, f"got {st2}")
                st3, h3, _ = http_c.get(f"/v1/collections/{c}/games/{gid}", {"If-None-Match": hdr.get("etag", "")})
                checks.check(f"{c}: game {gid} conditional 304", st3 == 304, f"got {st3}")
            st, _, _ = http_c.get(f"/v1/collections/{c}/games/18446744073709551615")
            checks.check(f"{c}: unknown game id -> 404", st == 404, f"got {st}")
            st, _, _ = http_c.get(f"/v1/collections/{c}/games/abc")
            checks.check(f"{c}: malformed game id -> 400", st == 400, f"got {st}")

            # latency (sequential, keep-alive) over a mixed workload
            workload = [m for lbl in ("common", "mid", "rare") for m in strata[lbl]] or [["e4"]]
            lat = []
            for i in range(requests_per_col):
                path = pos_url(c, workload[i % len(workload)])
                t = time.perf_counter()
                st, _, _ = http_c.get(path)
                lat.append(time.perf_counter() - t)
                if st != 200:
                    cr["mismatches"] += 1
            cr["position_latency"] = lat_summary(lat)
            glat = []
            for i in range(min(200, len(ids))):
                t = time.perf_counter()
                st, _, _ = http_c.get(f"/v1/collections/{c}/games/{ids[(i * 7919) % len(ids)]}")
                glat.append(time.perf_counter() - t)
            cr["game_retrieval_latency"] = lat_summary(glat)

            # concurrency: one OS process per client (no shared GIL), keep-alive connections
            conc = {}
            paths = [pos_url(c, m) for m in workload]
            per_worker = max(50, requests_per_col // 4)
            for workers in (1, 4, 8, 16, 32):
                res = run_load(host, port, paths, workers, per_worker)
                conc[str(workers)] = res
                checks.check(f"{c}: concurrency x{workers} without errors", res["errors"] == 0, str(res["errors"]))
            cr["concurrency"] = conc

        # collection isolation at the query level
        st, body = http_c.json(f"/v1/explorer/position?collection=bogus&moves=e4")
        checks.check("unknown collection -> 400", st == 400, f"{st}")
        # server RSS after load
        try:
            for line in Path(f"/proc/{proc.pid}/status").read_text().splitlines():
                if line.startswith("VmHWM:"):
                    report["server_peak_rss_bytes"] = int(line.split()[1]) * 1024
        except OSError:
            pass
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
    report["checks_total"] = len(checks.results)
    report["checks_failed"] = len(checks.failed)
    report["failed"] = checks.failed
    report["passed"] = not checks.failed
    return report


def cmd_http_check(args: argparse.Namespace) -> int:
    data_dir = data_dir_from(args)
    codb = find_codb(args.codb)
    cat = load_catalog(Path(args.catalog))
    roots = {c: release_root(data_dir, c) for c in COLLECTIONS}
    depth = {c: cat["collections"][c]["default_depth_plies"] for c in COLLECTIONS}
    out = data_dir / "reports"
    out.mkdir(parents=True, exist_ok=True)
    rep = run_http_checks(codb, roots, args.host, args.port or free_port(), out / "serve.log", depth,
                          requests_per_col=args.requests, sample_games=args.sample_games,
                          per_stratum=args.per_stratum)
    (out / "http_check.json").write_text(json.dumps(rep, indent=2) + "\n")
    log(f"[http-check] {rep['checks_total'] - rep['checks_failed']}/{rep['checks_total']} checks passed; "
        f"report {out / 'http_check.json'}")
    if not rep["passed"]:
        raise CorpusError("HTTP integration checks failed")
    return 0


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--catalog", default=str(DEFAULT_CATALOG), help="pinned source catalog")
    p.add_argument("--data-dir", help="data directory (default $CODB_DATA_DIR or ./data)")
    p.add_argument("--codb", help="path to the codb binary (default target/release/codb)")
    sub = p.add_subparsers(dest="cmd", required=True)

    def with_col(sp, default="all"):
        sp.add_argument("--collection", default=default, choices=("all", *COLLECTIONS))
        sp.add_argument("--profile", help="catalog profile (default: baseline)")
        return sp

    sp = with_col(sub.add_parser("sources", help="list pinned sources and local status"))
    sp.add_argument("--json", action="store_true")
    sp.set_defaults(fn=cmd_sources)

    sp = with_col(sub.add_parser("plan", help="print resource estimates before acquisition"))
    sp.set_defaults(fn=cmd_plan)

    sp = with_col(sub.add_parser("fetch", help="download and verify archives"))
    sp.add_argument("--source", action="append",
                    help="restrict to this source id (repeatable); allows parallel fetches of different archives")
    sp.add_argument("--offline", action="store_true", help="never touch the network; verify only")
    sp.add_argument("--rehash", action="store_true", help="re-hash existing archives")
    sp.add_argument("--retries", type=int, default=4)
    sp.add_argument("--backoff", type=float, default=2.0, help="base retry delay in seconds")
    sp.add_argument("--max-download-bytes", type=int)
    sp.add_argument("--min-free-bytes", type=int, default=2 * 1024**3)
    sp.add_argument("--update-catalog", action="store_true",
                    help="record verified_sha256/acquired_at in the catalog")
    sp.set_defaults(fn=cmd_fetch)

    sp = with_col(sub.add_parser("verify-sources", help="offline re-hash of local archives"))
    sp.set_defaults(fn=cmd_verify_sources)

    sp = sub.add_parser(
        "extract-local-7z",
        help="safely extract one user-supplied 7z member for local evaluation only",
    )
    sp.add_argument("--archive", required=True, help="local .7z archive path")
    sp.add_argument("--member", required=True, help="exact regular member file name")
    sp.add_argument("--output", required=True, help="new output path under a git-ignored data directory")
    sp.add_argument("--record", help="local provenance JSON path (default: OUTPUT.local-source.json)")
    sp.add_argument(
        "--max-expanded-bytes",
        type=int,
        default=64 * 1024**3,
        help="maximum declared and extracted member size (default: 64 GiB)",
    )
    sp.add_argument(
        "--max-expansion-ratio",
        type=int,
        default=1000,
        help="maximum uncompressed-to-compressed ratio (default: 1000:1)",
    )
    sp.add_argument("--expected-local-archive-sha256", help="optional locally computed archive hash pin")
    sp.add_argument("--expected-member-sha256", help="optional locally computed member hash pin")
    sp.set_defaults(fn=cmd_extract_local_7z)

    sp = sub.add_parser("pin", help="add catalog entries from the publisher checksum list")
    sp.add_argument("--family", choices=tuple(FAMILIES), required=True)
    sp.add_argument("--periods", required=True, help="comma separated YYYY-MM")
    sp.add_argument("--profile")
    sp.add_argument("--confidence", choices=CONFIDENCE_LEVELS, default="high")
    sp.add_argument("--eligibility", default="")
    sp.add_argument("--notes", default="")
    sp.set_defaults(fn=cmd_pin)

    sp = sub.add_parser("build", help="build, index, verify, and publish a release")
    sp.add_argument("--collection", required=True, choices=COLLECTIONS)
    sp.add_argument("--profile")
    sp.add_argument("--depth-plies", type=int)
    sp.add_argument("--memory-limit-mib", type=int, default=128)
    sp.add_argument("--masters-event-audit", type=Path,
                    help="event-level Masters policy JSON (required by the current production policy)")
    sp.set_defaults(fn=cmd_build)

    sp = with_col(sub.add_parser("verify", help="deep-verify published releases"))
    sp.set_defaults(fn=cmd_verify)

    sp = with_col(sub.add_parser("report", help="write quality reports for published releases"))
    sp.set_defaults(fn=cmd_report)

    sp = sub.add_parser("package", help="package a release into a verifiable archive")
    sp.add_argument("--collection", required=True, choices=COLLECTIONS)
    sp.add_argument("--output-dir")
    sp.set_defaults(fn=cmd_package)

    sp = sub.add_parser("verify-package", help="verify checksum, extract, and re-open a package")
    sp.add_argument("--archive", required=True)
    sp.set_defaults(fn=cmd_verify_package)

    sp = sub.add_parser("bootstrap", help="provision real release datasets before Docker startup")
    source = sp.add_mutually_exclusive_group()
    source.add_argument("--rebuild", action="store_true",
                        help="download pinned sources and rebuild both collections (about 8.66 GB)")
    source.add_argument("--masters-package", help="verified Masters OTB release tar archive")
    sp.add_argument("--elite-package", help="verified Elite Online release tar archive")
    sp.add_argument("--memory-limit-mib", type=int, default=128)
    sp.add_argument("--max-download-bytes", type=int,
                    help="optional cap for the explicit --rebuild source download")
    sp.add_argument("--profile", default="baseline")
    sp.set_defaults(fn=cmd_bootstrap)

    sp = sub.add_parser("serve", help="serve both collections via codb serve")
    sp.add_argument("--host", default="127.0.0.1")
    sp.add_argument("--port", type=int, default=8080)
    sp.add_argument("--allowed-origins", default=DEFAULT_ORIGINS)
    sp.set_defaults(fn=cmd_serve)

    sp = sub.add_parser("http-check", help="start the server and run semantic HTTP/CLI checks")
    sp.add_argument("--host", default="127.0.0.1")
    sp.add_argument("--port", type=int)
    sp.add_argument("--requests", type=int, default=400)
    sp.add_argument("--per-stratum", type=int, default=8,
                    help="positions compared per stratum (common, mid, rare)")
    sp.add_argument("--sample-games", type=int, default=3000,
                    help="stored games exported to derive stratified positions")
    sp.set_defaults(fn=cmd_http_check)
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.fn(args)
    except CorpusError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
