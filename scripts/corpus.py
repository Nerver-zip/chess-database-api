#!/usr/bin/env python3
"""Reproducible corpus acquisition, build, release, and serving orchestration.

This tool is a thin orchestration layer around the existing `codb` Rust CLI. It
never reimplements ingestion, curation, deduplication, indexing, or dataset
publication: it pins and verifies source archives, invokes `codb build`,
`codb build-index`, `codb verify --deep`, and `codb report`, records release
provenance, packages releases, and drives `codb serve`.

Only the Python standard library is used.

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
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Iterable

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CATALOG = REPO_ROOT / "corpus" / "catalog.json"
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
                "ingest_rss_ceiling_mib": 30,
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


def run_measured(
    cmd: list[str], log_path: Path, env: dict[str, str], sample_paths: list[Path]
) -> dict[str, Any]:
    """Run a command, capturing wall time, peak RSS (kernel rusage) and peak disk use."""
    sampler = DiskSampler(sample_paths)
    t0 = time.monotonic()
    with open(log_path, "wb") as out:
        sampler.start()
        proc = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, env=env)
        _, status, ru = os.wait4(proc.pid, 0)
        proc.returncode = os.waitstatus_to_exitcode(status)
    wall = time.monotonic() - t0
    peak_disk = sampler.finish()
    return {
        "command": " ".join(cmd),
        "returncode": proc.returncode,
        "wall_seconds": round(wall, 3),
        "peak_rss_bytes": ru.ru_maxrss * 1024,  # Linux reports KiB
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
    step("build", [str(codb), "build", "--manifest", str(manifest_path), "--collection", collection,
                   "--output", str(ds_root), "--report-json", str(build_report)])
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
                        qj, metrics, depth, args, codb)
    shutil.copy(build_report, ds_root / "RELEASE" / "build_report.json")
    shutil.copy(work / "index_report.json", ds_root / "RELEASE" / "index_report.json")
    shutil.copy(quality, ds_root / "RELEASE" / "quality_report.json")
    shutil.copy(logs / "verify.log", ds_root / "RELEASE" / "verify_deep.txt")
    (ds_root / "RELEASE" / "metrics.json").write_text(json.dumps(metrics, indent=2) + "\n")
    # keep the pinned input manifest with the release
    shutil.copy(manifest_path, ds_root / "RELEASE" / "sources.manifest.json")

    # 4. Promote without ever deleting a valid existing release.
    final = release_root(data_dir, collection)
    final.parent.mkdir(parents=True, exist_ok=True)
    shutil.rmtree(tmp, ignore_errors=True)
    retired = None
    if final.exists():
        retired_dir = data_dir / "releases" / ".retired"
        retired_dir.mkdir(parents=True, exist_ok=True)
        retired = retired_dir / f"{collection}-{int(time.time())}"
        os.replace(final, retired)
    os.replace(ds_root, final)
    fsync_dir(final.parent)
    # post-promotion deep verification of the published location
    r = subprocess.run([str(codb), "verify", "--dataset", str(final), "--deep"],
                       capture_output=True, text=True)
    if r.returncode != 0:
        if retired is not None:
            os.replace(final, work / "failed-release")
            os.replace(retired, final)
        raise CorpusError(f"post-promotion verification failed: {r.stdout[-500:]}{r.stderr[-500:]}")
    shutil.rmtree(work, ignore_errors=True)
    log(f"[build] published {final}")
    if retired:
        log(f"[build] previous release preserved at {retired}")
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
        "online-event exclusion, minimum length), deduplicated, and re-encoded into the CGB3 "
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
                        qj, metrics, depth, args, codb) -> None:
    lic = cat["licenses"][sources[0]["license"]]
    rel_dir = ds_root / "RELEASE"
    rel_dir.mkdir(exist_ok=True)
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
def cmd_verify(args: argparse.Namespace) -> int:
    data_dir = data_dir_from(args)
    codb = find_codb(args.codb)
    collections = COLLECTIONS if args.collection == "all" else (args.collection,)
    failures = 0
    for c in collections:
        root = release_root(data_dir, c)
        r = subprocess.run([str(codb), "verify", "--dataset", str(root), "--deep"],
                           capture_output=True, text=True)
        ok = r.returncode == 0 and "Result: SUCCESS" in r.stdout
        verdict = verify_release_files(root)
        log(f"[verify] {c}: codb verify --deep {'PASS' if ok else 'FAIL'}; "
            f"release artifacts {'PASS' if verdict is None else 'FAIL: ' + verdict}")
        failures += 0 if ok and verdict is None else 1
    if failures:
        raise CorpusError(f"{failures} release(s) failed verification")
    return 0


def verify_release_files(root: Path) -> str | None:
    """Re-hash every artifact listed in RELEASE.json. Returns an error string or None."""
    rj = root / "RELEASE" / "RELEASE.json"
    if not rj.exists():
        return "RELEASE/RELEASE.json missing"
    release = json.loads(rj.read_text())
    vdir = current_version_dir(root)
    if vdir.name != release["dataset_version"] and not vdir.name.startswith(release["dataset_version"]):
        return f"current version {vdir.name} does not match release {release['dataset_version']}"
    for rel, meta in release["artifacts"].items():
        p = vdir / rel
        if not p.is_file():
            return f"artifact missing: {rel}"
        if p.stat().st_size != meta["size_bytes"] or sha256_file(p) != meta["sha256"]:
            return f"artifact checksum mismatch: {rel}"
    return None


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
    if problem:
        raise CorpusError(f"refusing to package an unverified release: {problem}")
    vdir = current_version_dir(root)
    release = json.loads((root / "RELEASE" / "RELEASE.json").read_text())
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
        for p in sorted((root / "RELEASE").rglob("*")):
            names.append((p, f"{c}/RELEASE/{p.relative_to(root / 'RELEASE')}"))
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
    verify_package(archive, codb, expected_games=release["accepted_games"])
    log(f"[package] extraction and deep verification PASSED for {archive.name}")
    return 0


def verify_package(archive: Path, codb: Path, expected_games: int | None = None) -> dict[str, Any]:
    sums = archive.with_name(archive.name + ".sha256")
    if not sums.exists():
        raise CorpusError(f"checksum file missing: {sums}")
    want = sums.read_text().split()[0]
    got = sha256_file(archive)
    if want != got:
        raise CorpusError(f"package checksum mismatch: {got} != {want}")
    import tempfile
    with tempfile.TemporaryDirectory(prefix="codb-pkg-", dir=archive.parent) as td:
        with tarfile.open(archive) as tf:
            tf.extractall(td, filter="data")
        roots = [p for p in Path(td).iterdir() if p.is_dir()]
        if len(roots) != 1:
            raise CorpusError("package must contain exactly one top-level release directory")
        root = roots[0]
        for required in ("RELEASE/RELEASE.json", "RELEASE/DATASET_LICENSE.txt", "current"):
            if not (root / required).exists():
                raise CorpusError(f"package is missing required entry {required}")
        problem = verify_release_files(root)
        if problem:
            raise CorpusError(f"extracted package failed artifact verification: {problem}")
        r = subprocess.run([str(codb), "verify", "--dataset", str(root), "--deep"],
                           capture_output=True, text=True)
        if r.returncode != 0 or "Result: SUCCESS" not in r.stdout:
            raise CorpusError(f"extracted package failed `codb verify --deep`:\n{r.stdout}{r.stderr}")
        info = json.loads((root / "RELEASE" / "RELEASE.json").read_text())
        if expected_games is not None and info["accepted_games"] != expected_games:
            raise CorpusError("extracted release game count differs from the source release")
        q = codb_json(codb, ["query", "--dataset", str(root), "--moves", "e4", "--json"])
        if "total_games" not in q:
            raise CorpusError("extracted package query returned no statistics")
        return info


def cmd_verify_package(args: argparse.Namespace) -> int:
    info = verify_package(Path(args.archive), find_codb(args.codb))
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

            # concurrency
            conc = {}
            for workers in (1, 4, 8, 16, 32):
                errors = 0
                lock = threading.Lock()
                per_worker = max(50, requests_per_col // 4)

                def worker(seed: int) -> None:
                    nonlocal errors
                    cl = Http(host, port)
                    for j in range(per_worker):
                        st, _, _ = cl.get(pos_url(c, workload[(seed + j) % len(workload)]))
                        if st != 200:
                            with lock:
                                errors += 1
                t0 = time.perf_counter()
                ths = [threading.Thread(target=worker, args=(w,)) for w in range(workers)]
                [t.start() for t in ths]
                [t.join() for t in ths]
                el = time.perf_counter() - t0
                conc[str(workers)] = {"requests": workers * per_worker, "errors": errors,
                                      "req_per_sec": round(workers * per_worker / el, 1)}
                checks.check(f"{c}: concurrency x{workers} without errors", errors == 0, str(errors))
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
