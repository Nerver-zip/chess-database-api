#!/usr/bin/env python3
"""Prepare, verify, reconstruct, and install Opening Explorer dataset release assets.

Two products are distributed for one curated corpus (see docs/DATASET_RELEASES.md):

* ``<release>-indexed.tar``: the indexed dataset package (CGB3 shards, game lookup,
  CPI v2 position index, collection-specific metadata, and a source-attribution sidecar
  only when the snapshot contains one). Its member layout is the corpus release-package
  layout verified by ``scripts/corpus.py``.
* ``<release>-filtered.pgn.zst``: the same canonical games as Zstandard-compressed PGN,
  written by ``opening-explorer export-pgn``.

Commands:

  prepare      build both products from an active snapshot into data/releases-dist/
  verify       re-check every part and whole-product SHA-256 in a dist directory
  reconstruct  concatenate parts into whole products, verifying before and after
  install      extract an indexed package, deep-verify it, and atomically publish it
               with ``opening-explorer publish`` into a non-live dataset root
  check-manifest  validate the committed manifest and collection-specific license gate
  verify-remote   compare a draft GitHub Release's asset list with the manifest
  upload       print (default) or, with --execute, create a DRAFT GitHub Release

Fail-closed rules: incomplete or extra files, size or SHA-256 mismatches, a manifest
that disagrees with the expected dataset facts, a tag that is not reachable from main,
or a collection-specific distribution policy failure all stop the command with a nonzero exit.
Upload is dry-run unless ``--execute`` is given, and it never runs from CI.

The package builder deliberately reuses ``corpus.py``'s verification and safe-extraction
functions. The Elite release is checked against its pinned Lichess catalog sources;
Masters OTB remains independently validated because its Gigabase source is not a catalog source.
"""

from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any, Iterable

SCRIPTS_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS_DIR.parent
sys.path.insert(0, str(SCRIPTS_DIR))

import corpus  # noqa: E402  (reuses verified release-file and safe-extraction logic)

MANIFEST_SCHEMA_VERSION = 1
CLEARANCE_SCHEMA_VERSION = 1
CHUNK = 8 * 1024 * 1024
# GitHub rejects release assets of 2 GiB or more; parts stay well below that bound.
PART_LIMIT_BYTES = 1900 * 1024 * 1024
GITHUB_ASSET_LIMIT_BYTES = 2 * 1024 * 1024 * 1024 - 1
# `export-pgn`'s default zstd level is independent of the CGB3 shard level in build configuration.
ELITE_PGN_ZSTD_LEVEL = 19
DIST_DIR = REPO_ROOT / "data" / "releases-dist"
RELEASES_DIR = REPO_ROOT / "releases"
CLEARANCE_PATH = RELEASES_DIR / "distribution_clearance.json"
NOTICE_TEMPLATE = REPO_ROOT / "dataset" / "ATTRIBUTION.md"
LIVE_ROOT = (REPO_ROOT / "data" / "releases" / "masters-otb").resolve()
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")

# Collection policy is deliberately declarative: release identifiers, dataset facts,
# licensing, notice sources, and publication gates live together. Masters R3 remains
# pinned to its existing facts and clearance record.
COLLECTION_POLICIES: dict[str, dict[str, Any]] = {
    "masters-otb": {
        "depth_plies": 40,
        "build_id_re": re.compile(
            r"^codb-masters-otb-[0-9]+-[0-9]+-[0-9a-f]{16}-cpi-d40$"
        ),
        "distribution_gate": "clearance",
        "license_id": None,
    },
    "elite-online": {
        "depth_plies": 30,
        "build_id_re": re.compile(
            r"^codb-elite-online-[0-9]+-[0-9]+-[0-9a-f]{16}-cpi-d30$"
        ),
        "distribution_gate": "license",
        "license_id": "CC0-1.0",
        "license_name": "Creative Commons CC0 1.0 Universal",
        "license_url": "https://creativecommons.org/publicdomain/zero/1.0/",
        "publisher": "Lichess.org",
        "source_url": "https://database.lichess.org/standard/",
        "source_periods": ["2015-09", "2015-12", "2016-03", "2016-06",
                            "2016-09", "2016-12", "2017-03"],
    },
}

EXPECTED_RELEASES: dict[str, dict[str, Any]] = {
    "masters-otb-r3": {
        "collection": "masters-otb",
        "game_count": 3_284_429,
        "depth_plies": 40,
        "unique_positions": 51_909_604,
        "unique_position_move_pairs": 55_864_091,
        "accepted_unique": 3_284_429,
        "total_encountered": 13_450_696,
    },
    "elite-online-r1": {
        "collection": "elite-online",
        "game_count": 429_150,
        "depth_plies": 30,
        "unique_positions": 6_136_307,
        "accepted_unique": 429_150,
    },
}
PRODUCT_KINDS = {"indexed-package": "indexed", "filtered-pgn": "filtered"}


def policy_for_release(release_id: str) -> dict[str, Any]:
    expected = EXPECTED_RELEASES.get(release_id)
    if expected is None:
        raise ReleaseError(f"unknown release id {release_id!r}")
    return COLLECTION_POLICIES[expected["collection"]]


def live_root_for(collection: str) -> Path:
    return (REPO_ROOT / "data" / "releases" / collection).resolve()


class ReleaseError(RuntimeError):
    """A fail-closed release error. Exit code 2."""


def log(message: str) -> None:
    print(message, flush=True)


def tag_for(release_id: str) -> str:
    return f"dataset-{release_id}"


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(CHUNK), b""):
            digest.update(block)
    return digest.hexdigest()


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise ReleaseError(f"file not found: {path}") from error
    except json.JSONDecodeError as error:
        raise ReleaseError(f"invalid JSON in {path}: {error}") from error


def asset_names(release_id: str) -> dict[str, str]:
    return {
        "indexed": f"{release_id}-indexed.tar",
        "filtered": f"{release_id}-filtered.pgn.zst",
    }


def notice_name(release_id: str) -> str:
    return f"{release_id}-NOTICE.md"


def manifest_name(release_id: str) -> str:
    return f"{release_id}-RELEASE_MANIFEST.json"


def sums_name(release_id: str) -> str:
    return f"{release_id}-SHA256SUMS.txt"


# --------------------------------------------------------------------------- #
# Splitting and reconstruction
# --------------------------------------------------------------------------- #
def split_product(source: Path, dest_dir: Path, name: str, limit: int) -> dict[str, Any]:
    """Copy ``source`` into ``dest_dir`` as one file or as numbered parts.

    Files at or below ``limit`` keep their final name. Larger files become
    ``<name>.part-001``, ``.part-002``, and so on, cut at fixed multiples of ``limit``,
    so the split is deterministic. Returns the whole-product and per-part digests.
    """
    if not 0 < limit <= GITHUB_ASSET_LIMIT_BYTES:
        raise ReleaseError(f"part limit {limit} must be between 1 and the GitHub asset limit")
    size = source.stat().st_size
    whole = hashlib.sha256()
    parts: list[dict[str, Any]] = []
    if size <= limit:
        target = dest_dir / name
        part_digest = hashlib.sha256()
        with source.open("rb") as src, target.open("xb") as dst:
            for block in iter(lambda: src.read(CHUNK), b""):
                whole.update(block)
                part_digest.update(block)
                dst.write(block)
        parts.append({"name": name, "size_bytes": size, "sha256": part_digest.hexdigest()})
    else:
        count = -(-size // limit)
        with source.open("rb") as src:
            for index in range(1, count + 1):
                part_name = f"{name}.part-{index:03d}"
                remaining = min(limit, size - (index - 1) * limit)
                part_digest = hashlib.sha256()
                written = 0
                with (dest_dir / part_name).open("xb") as dst:
                    while written < remaining:
                        block = src.read(min(CHUNK, remaining - written))
                        if not block:
                            raise ReleaseError(f"{source} shrank while splitting")
                        whole.update(block)
                        part_digest.update(block)
                        dst.write(block)
                        written += len(block)
                parts.append(
                    {"name": part_name, "size_bytes": written, "sha256": part_digest.hexdigest()}
                )
    return {
        "name": name,
        "size_bytes": size,
        "sha256": whole.hexdigest(),
        "parts": parts,
    }


def product_part_names(asset: dict[str, Any]) -> list[str]:
    return [part["name"] for part in asset["parts"]]


def dist_expected_files(manifest: dict[str, Any]) -> set[str]:
    names: set[str] = set()
    for asset in manifest["assets"]:
        names.update(product_part_names(asset))
    release_id = manifest["release_id"]
    names.update({notice_name(release_id), manifest_name(release_id), sums_name(release_id)})
    return names


def verify_dist(manifest: dict[str, Any], dist_dir: Path) -> None:
    """Fail closed unless the directory holds exactly the expected, intact files."""
    if not dist_dir.is_dir():
        raise ReleaseError(f"dist directory does not exist: {dist_dir}")
    expected = dist_expected_files(manifest)
    actual = {
        entry.name
        for entry in dist_dir.iterdir()
        if not entry.name.startswith(".")
    }
    missing = sorted(expected - actual)
    if missing:
        raise ReleaseError(f"release assets are incomplete; missing: {', '.join(missing)}")
    unexpected = sorted(actual - expected)
    if unexpected:
        raise ReleaseError(f"unexpected files in dist directory: {', '.join(unexpected)}")
    for asset in manifest["assets"]:
        whole = hashlib.sha256()
        total = 0
        for part in asset["parts"]:
            path = dist_dir / part["name"]
            if path.is_symlink() or not path.is_file():
                raise ReleaseError(f"release part is not a regular file: {part['name']}")
            actual_size = path.stat().st_size
            if actual_size != part["size_bytes"]:
                raise ReleaseError(
                    f"size mismatch for {part['name']}: {actual_size} != {part['size_bytes']}"
                )
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                for block in iter(lambda: stream.read(CHUNK), b""):
                    digest.update(block)
                    whole.update(block)
                    total += len(block)
            if digest.hexdigest() != part["sha256"]:
                raise ReleaseError(f"SHA-256 mismatch for {part['name']}")
        if total != asset["size_bytes"]:
            raise ReleaseError(f"reconstructed size of {asset['name']} differs from the manifest")
        if whole.hexdigest() != asset["sha256"]:
            raise ReleaseError(f"reconstructed SHA-256 of {asset['name']} differs from the manifest")
        log(f"[verify] {asset['name']}: {len(asset['parts'])} part(s), {total} bytes, sha256 ok")


def reconstruct(manifest: dict[str, Any], dist_dir: Path, output_dir: Path) -> dict[str, Path]:
    verify_dist(manifest, dist_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    produced: dict[str, Path] = {}
    for asset in manifest["assets"]:
        target = output_dir / asset["name"]
        if target.exists() or target.is_symlink():
            raise ReleaseError(f"refusing to overwrite {target}")
        partial = output_dir / f".{asset['name']}.partial"
        whole = hashlib.sha256()
        try:
            with partial.open("xb") as out:
                for part in asset["parts"]:
                    with (dist_dir / part["name"]).open("rb") as src:
                        for block in iter(lambda: src.read(CHUNK), b""):
                            whole.update(block)
                            out.write(block)
            if whole.hexdigest() != asset["sha256"]:
                raise ReleaseError(f"reconstructed {asset['name']} failed its SHA-256 check")
            os.replace(partial, target)
        except BaseException:
            with contextlib.suppress(FileNotFoundError):
                partial.unlink()
            raise
        produced[asset["name"]] = target
        log(f"[reconstruct] {target} ({asset['size_bytes']} bytes) sha256 {asset['sha256']}")
        write_sha_sidecar(target)
    return produced


def write_sha_sidecar(archive: Path) -> None:
    """Write the ``<archive>.sha256`` sidecar that corpus.py's extraction requires."""
    digest = sha256_file(archive)
    (archive.with_name(archive.name + ".sha256")).write_text(f"{digest}  {archive.name}\n")


# --------------------------------------------------------------------------- #
# Indexed package (deterministic tar with corpus.py member rules)
# --------------------------------------------------------------------------- #
def build_indexed_tar(
    live_root: Path, archive: Path, notice: Path | None, collection: str | None = None
) -> None:
    """Write the release package for the active version of ``live_root``.

    Member rules match ``corpus.py cmd_package``: the immutable version directory
    (without lock or dot files), its RELEASE copy at the collection root, and the
    root symlinks that make the package a valid dataset root. Owner, group, and time
    fields are normalized so the same snapshot yields the same bytes. ``notice`` is
    added under the version's RELEASE directory when given.
    """
    collection = collection or live_root.name
    if collection not in COLLECTION_POLICIES:
        raise ReleaseError(f"unsupported dataset collection: {collection}")
    version_dir = corpus.current_version_dir(live_root)
    members: list[tuple[Path, str]] = []
    for path in sorted(version_dir.rglob("*")):
        if path.name.startswith(".") or path.name.endswith(".lock"):
            continue
        members.append((path, f"{collection}/versions/{version_dir.name}/{path.relative_to(version_dir)}"))
    for path in sorted((version_dir / "RELEASE").rglob("*")):
        members.append((path, f"{collection}/RELEASE/{path.relative_to(version_dir / 'RELEASE')}"))
    for link in ("current", "games.idx", "manifest.json", "positions.cpi", "shards"):
        path = live_root / link
        if path.is_symlink():
            members.append((path, f"{collection}/{link}"))
    if notice is not None:
        members.append((notice, f"{collection}/versions/{version_dir.name}/RELEASE/NOTICE.md"))

    partial = archive.with_name(archive.name + ".partial")
    with tarfile.open(partial, "w", format=tarfile.PAX_FORMAT) as tf:
        for source, arcname in sorted(members, key=lambda item: item[1]):
            info = tf.gettarinfo(str(source), arcname=arcname)
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 0
            info.mode = 0o755 if (info.isdir() or info.issym()) else 0o644
            if info.isreg():
                with source.open("rb") as stream:
                    tf.addfile(info, stream)
            else:
                tf.addfile(info)
    os.replace(partial, archive)


def verify_indexed_package(archive: Path, codb: Path, expected: dict[str, Any]) -> dict[str, Any]:
    """Extract the package into a fresh directory and run the same checks as corpus.py."""
    with tempfile.TemporaryDirectory(prefix="opening-explorer-pkg-") as scratch:
        root = corpus.extract_release_archive(archive, Path(scratch))
        problem = corpus.verify_release_files(root)
        if problem:
            raise ReleaseError(f"extracted package failed artifact verification: {problem}")
        version_dir = corpus.current_version_dir(root)
        release = load_json(version_dir / "RELEASE" / "RELEASE.json")
        check_release_identity(release, expected)
        run_codb_verify(codb, version_dir)
        return release


def run_codb_verify(codb: Path, dataset: Path) -> None:
    result = subprocess.run(
        [str(codb), "verify", "--dataset", str(dataset), "--deep"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0 or "Result: SUCCESS" not in result.stdout:
        raise ReleaseError(f"codb verify --deep failed for {dataset}:\n{result.stdout}{result.stderr}")
    log(f"[codb] verify --deep passed: {dataset.name}")


def check_release_identity(release: dict[str, Any], expected: dict[str, Any]) -> None:
    index = release.get("index", {})
    accounting = release.get("accounting", {})
    checks = {
        "collection": (release.get("collection"), expected["collection"]),
        "accepted_games": (release.get("accepted_games"), expected["game_count"]),
        "index depth": (index.get("depth_plies"), expected["depth_plies"]),
        "unique positions": (index.get("unique_positions"), expected["unique_positions"]),
        "accepted_unique": (accounting.get("accepted_unique"), expected["accepted_unique"]),
    }
    for optional_key in ("unique_position_move_pairs", "total_encountered"):
        if optional_key in expected:
            source = index if optional_key == "unique_position_move_pairs" else accounting
            label = "position/move pairs" if optional_key == "unique_position_move_pairs" else optional_key
            actual = source.get(optional_key)
            if optional_key == "unique_position_move_pairs" and actual is None:
                actual = index.get("total_move_records")
            checks[label] = (actual, expected[optional_key])
    for label, (actual, wanted) in checks.items():
        if actual != wanted:
            raise ReleaseError(f"release {label} is {actual!r}, expected {wanted!r}")
    build_id = release.get("dataset_version")
    policy = COLLECTION_POLICIES[expected["collection"]]
    if not isinstance(build_id, str) or not policy["build_id_re"].fullmatch(build_id):
        raise ReleaseError(f"release dataset_version is not a {expected['collection']} build identifier")


# --------------------------------------------------------------------------- #
# Manifest, distribution gate, and notice
# --------------------------------------------------------------------------- #
def load_clearance(path: Path = CLEARANCE_PATH) -> dict[str, Any]:
    clearance = load_json(path)
    if not isinstance(clearance, dict) or clearance.get("schema_version") != CLEARANCE_SCHEMA_VERSION:
        raise ReleaseError("distribution clearance file has an unsupported schema")
    if clearance.get("status") not in ("not_cleared", "cleared"):
        raise ReleaseError("distribution clearance status must be not_cleared or cleared")
    return clearance


def clearance_blockers(clearance: dict[str, Any], release_id: str) -> list[str]:
    blockers: list[str] = []
    if clearance.get("status") != "cleared":
        blockers.append(f"distribution is not cleared (status: {clearance.get('status')})")
    reference = clearance.get("clearance_reference")
    if not isinstance(reference, str) or not reference.strip():
        blockers.append("no written clearance reference is recorded")
    if clearance.get("release_id") != release_id:
        blockers.append("distribution clearance does not name this release")
    return blockers


def distribution_blockers(manifest: dict[str, Any], clearance_path: Path = CLEARANCE_PATH) -> list[str]:
    """Return blockers required by this collection's recorded licensing policy."""
    policy = policy_for_release(manifest["release_id"])
    if policy["distribution_gate"] == "clearance":
        return clearance_blockers(load_clearance(clearance_path), manifest["release_id"])
    if policy["distribution_gate"] == "license":
        return []
    raise ReleaseError(f"unknown distribution gate for {manifest['release_id']}")


def elite_source_archives() -> list[dict[str, Any]]:
    """Return the exact pinned Elite baseline archives recorded in the source catalog."""
    policy = COLLECTION_POLICIES["elite-online"]
    catalog = corpus.load_catalog(corpus.DEFAULT_CATALOG)
    sources = corpus.select_sources(catalog, "elite-online", "baseline")
    periods = [source["period"] for source in sources]
    if periods != policy["source_periods"]:
        raise ReleaseError(f"Elite baseline periods differ from release policy: {periods}")
    return [
        {
            "id": source["id"],
            "filename": source["filename"],
            "period": source["period"],
            "url": source["url"],
            "license": source["license"],
            "size_bytes": source["expected_size_bytes"],
            "publisher_sha256": source["publisher_sha256"],
            "publisher_checksum_url": source["publisher_checksum_url"],
            "provenance_confidence": source["provenance_confidence"],
        }
        for source in sources
    ]


def elite_catalog_identity() -> dict[str, str]:
    catalog = corpus.load_catalog(corpus.DEFAULT_CATALOG)
    catalog_bytes = json.dumps(catalog, sort_keys=True).encode()
    return {
        "catalog_id": catalog["catalog_id"],
        "catalog_content_sha256": hashlib.sha256(catalog_bytes).hexdigest(),
    }


def validate_elite_sources(sources: Any) -> None:
    if not isinstance(sources, dict):
        raise ReleaseError("Elite source provenance must be an object")
    policy = COLLECTION_POLICIES["elite-online"]
    expected_archives = elite_source_archives()
    expected_catalog = elite_catalog_identity()
    if (sources.get("publisher") != policy["publisher"]
            or sources.get("profile") != "baseline"
            or sources.get("archive_index_url") != policy["source_url"]
            or sources.get("periods") != policy["source_periods"]
            or sources.get("source_catalog") != expected_catalog
            or sources.get("archives") != expected_archives):
        raise ReleaseError("Elite source archives or provenance differ from the pinned baseline catalog")


def validate_elite_build_metadata(manifest: dict[str, Any]) -> None:
    """Validate build, curation, format, and export provenance for Elite R1."""
    policy = COLLECTION_POLICIES["elite-online"]
    sources = manifest["sources"]
    archives = sources["archives"]
    build = manifest.get("build")
    if not isinstance(build, dict):
        raise ReleaseError("Elite manifest must record build configuration")
    if (build.get("profile") != "baseline"
            or build.get("index_depth_plies") != policy["depth_plies"]
            or type(build.get("memory_limit_mib")) is not int or build["memory_limit_mib"] < 1
            or build.get("dedupe_mode") != "disk"
            or type(build.get("block_capacity")) is not int or build["block_capacity"] < 1
            or type(build.get("zstd_level")) is not int
            or build.get("source_order") != [archive["id"] for archive in archives]):
        raise ReleaseError("Elite build configuration does not match the pinned baseline")
    curation = manifest.get("policies")
    catalog = corpus.load_catalog(corpus.DEFAULT_CATALOG)
    expected_policy = catalog["collections"]["elite-online"]["curation_policy_version"]
    if (not isinstance(curation, dict)
            or curation.get("curation_policy_version") != expected_policy
            or not isinstance(curation.get("curation_policy"), dict)
            or not isinstance(curation.get("deduplication_policy_version"), str)
            or not curation["deduplication_policy_version"].strip()):
        raise ReleaseError("Elite curation or deduplication policy provenance is missing or inconsistent")
    tool = manifest.get("tool")
    if (not isinstance(tool, dict) or not isinstance(tool.get("codb_version"), str)
            or not tool["codb_version"].strip()):
        raise ReleaseError("Elite manifest must record the build tool version")
    formats = manifest.get("formats")
    if (not isinstance(formats, dict) or formats.get("shard") != "CGB3/v1"
            or formats.get("position_index") != "CPI/v2"
            or formats.get("game_lookup") != "games.idx"
            or manifest["index"].get("format") != "CPI/v2"):
        raise ReleaseError("Elite manifest must record CGB3, CPI v2, and games.idx formats")
    export = manifest.get("export")
    report = export.get("report") if isinstance(export, dict) else None
    if not isinstance(report, dict) or export.get("tool") != "opening-explorer export-pgn":
        raise ReleaseError("Elite manifest must include the opening-explorer export-pgn report")
    if (report.get("schema_version") != 1
            or report.get("dataset_build_id") != manifest["dataset_version"]
            or report.get("collection") != "elite-online"
            or report.get("expected_games") != manifest["game_count"]
            or report.get("games_written") != manifest["game_count"]
            or report.get("output_file") != asset_names(manifest["release_id"])["filtered"]
            or type(report.get("zstd_level")) is not int
            or report["zstd_level"] != ELITE_PGN_ZSTD_LEVEL
            or type(report.get("plain_bytes")) is not int or report["plain_bytes"] <= 0
            or type(report.get("setup_games")) is not int or not 0 <= report["setup_games"] <= manifest["game_count"]
            or type(report.get("missing_source_tag")) is not int or report["missing_source_tag"] != 0
            or not isinstance(report.get("omitted_conflicting_tags"), dict)
            or "source_attribution" in report):
        raise ReleaseError("Elite PGN export report does not match the dataset or filtered PGN product")


def validate_manifest(manifest: Any) -> dict[str, Any]:
    if not isinstance(manifest, dict):
        raise ReleaseError("release manifest must be a JSON object")
    if manifest.get("schema_version") != MANIFEST_SCHEMA_VERSION:
        raise ReleaseError("unsupported release manifest schema")
    release_id = manifest.get("release_id")
    if not isinstance(release_id, str) or release_id not in EXPECTED_RELEASES:
        raise ReleaseError(f"unknown release id: {release_id!r}")
    expected = EXPECTED_RELEASES[release_id]
    policy = COLLECTION_POLICIES[expected["collection"]]
    if manifest.get("tag") != tag_for(release_id):
        raise ReleaseError("manifest tag does not match its release id")
    if manifest.get("collection") != expected["collection"]:
        raise ReleaseError("manifest collection does not match its release id")
    if manifest.get("game_count") != expected["game_count"]:
        raise ReleaseError("manifest game count does not match the expected release")
    index = manifest.get("index")
    if not isinstance(index, dict):
        raise ReleaseError("manifest index metadata must be an object")
    if (index.get("depth_plies") != expected["depth_plies"]
            or index.get("unique_positions") != expected["unique_positions"]):
        raise ReleaseError("manifest index facts do not match the expected release")
    pairs = index.get("unique_position_move_pairs")
    if "unique_position_move_pairs" in expected:
        if pairs != expected["unique_position_move_pairs"]:
            raise ReleaseError("manifest index facts do not match the expected release")
    elif not isinstance(pairs, int) or pairs <= 0:
        raise ReleaseError("manifest must record a positive unique position/move pair count")
    build_id = manifest.get("dataset_version")
    if not isinstance(build_id, str) or not policy["build_id_re"].fullmatch(build_id):
        raise ReleaseError(f"manifest dataset_version is not a {expected['collection']} build identifier")
    accounting = manifest.get("accounting")
    counter_names = (
        "accepted_unique", "confirmed_duplicates", "superseded_prior", "rejected_by_policy",
        "rejected_variant", "rejected_illegal", "rejected_malformed", "quarantined",
    )
    if (not isinstance(accounting, dict)
            or any(type(accounting.get(name)) is not int or accounting[name] < 0 for name in counter_names)
            or type(accounting.get("total_encountered")) is not int
            or accounting.get("total_encountered", -1) < 0
            or accounting.get("accepted_unique") != expected["accepted_unique"]
            or accounting.get("accepted_unique") != expected["game_count"]
            or accounting.get("total_encountered") != sum(accounting[name] for name in counter_names)):
        raise ReleaseError("manifest accounting is missing, inconsistent, or does not reconcile")
    if "total_encountered" in expected and accounting["total_encountered"] != expected["total_encountered"]:
        raise ReleaseError("manifest total encountered does not match the expected release")
    assets = manifest.get("assets")
    if not isinstance(assets, list) or len(assets) != 2:
        raise ReleaseError("manifest must list exactly the two distributed products")
    names = asset_names(release_id)
    seen_kinds: set[str] = set()
    for asset in assets:
        if not isinstance(asset, dict):
            raise ReleaseError("manifest assets must be objects")
        kind = PRODUCT_KINDS.get(asset.get("kind"))
        if kind is None or kind in seen_kinds:
            raise ReleaseError(f"unknown or duplicate asset kind: {asset.get('kind')!r}")
        seen_kinds.add(kind)
        if asset.get("name") != names[kind]:
            raise ReleaseError(f"asset name {asset.get('name')!r} does not follow the naming rule")
        if type(asset.get("size_bytes")) is not int or asset["size_bytes"] <= 0:
            raise ReleaseError(f"asset {asset['name']} has an invalid size")
        if not SHA256_RE.fullmatch(str(asset.get("sha256", ""))):
            raise ReleaseError(f"asset {asset['name']} has a malformed SHA-256")
        parts = asset.get("parts")
        if not isinstance(parts, list) or not parts:
            raise ReleaseError(f"asset {asset['name']} lists no parts")
        total = 0
        for index_number, part in enumerate(parts, start=1):
            if (not isinstance(part, dict) or type(part.get("size_bytes")) is not int
                    or part["size_bytes"] <= 0):
                raise ReleaseError("manifest contains a part with an invalid size")
            if part["size_bytes"] > GITHUB_ASSET_LIMIT_BYTES:
                raise ReleaseError(f"part {part['name']} exceeds the GitHub asset limit")
            if not SHA256_RE.fullmatch(str(part.get("sha256", ""))):
                raise ReleaseError(f"part {part.get('name')} has a malformed SHA-256")
            expected_name = (asset["name"] if len(parts) == 1
                             else f"{asset['name']}.part-{index_number:03d}")
            if part.get("name") != expected_name:
                raise ReleaseError(f"part name {part.get('name')!r} should be {expected_name!r}")
            total += part["size_bytes"]
        if total != asset["size_bytes"]:
            raise ReleaseError(f"parts of {asset['name']} do not sum to its size")
    license_block = manifest.get("license")
    if not isinstance(license_block, dict):
        raise ReleaseError("manifest license metadata must be an object")
    if license_block.get("notice") != notice_name(release_id):
        raise ReleaseError("manifest does not name the dataset notice")
    distribution = manifest.get("distribution")
    if not isinstance(distribution, dict):
        raise ReleaseError("manifest distribution metadata must be an object")
    if policy["distribution_gate"] == "clearance":
        if distribution.get("clearance_file") != "releases/distribution_clearance.json":
            raise ReleaseError("Masters manifest does not reference the distribution clearance file")
    else:
        if distribution.get("clearance_file") is not None:
            raise ReleaseError("Elite CC0 manifest must not require the Masters clearance file")
        if distribution.get("basis") != policy["license_id"]:
            raise ReleaseError("Elite distribution basis must be CC0-1.0")
        if (license_block.get("id") != policy["license_id"]
                or license_block.get("name") != policy["license_name"]
                or license_block.get("url") != policy["license_url"]
                or license_block.get("dataset") != policy["license_name"]
                or license_block.get("publisher") != policy["publisher"]
                or license_block.get("software") !=
                "MIT OR Apache-2.0 (software only; does not apply to dataset contents)"):
            raise ReleaseError("Elite dataset license must be CC0 1.0 Universal")
        validate_elite_sources(manifest.get("sources"))
        validate_elite_build_metadata(manifest)
    return manifest


def render_notice(template: str, values: dict[str, str]) -> str:
    rendered = template
    for key, value in values.items():
        rendered = rendered.replace("{{" + key + "}}", value)
    leftover = re.findall(r"\{\{[A-Z_]+\}\}", rendered)
    if leftover:
        raise ReleaseError(f"notice template has unresolved placeholders: {sorted(set(leftover))}")
    return rendered


def build_manifest(
    release_id: str,
    assets: list[dict[str, Any]],
    release: dict[str, Any],
    export_report: dict[str, Any] | None = None,
) -> dict[str, Any]:
    expected = EXPECTED_RELEASES[release_id]
    policy = COLLECTION_POLICIES[expected["collection"]]
    format_versions = release.get("format_versions", {})
    source = release.get("source", {})
    qualification = release.get("qualification", {})
    formats = {
        "shard": format_versions.get("shard", "CGB3/v1"),
        "game_lookup": "games.idx",
        "pgn": "PGN, one game per canonical game, Zstandard-compressed in the .zst product",
    }
    curation = release.get("curation_policy", {})
    build = release.get("build_configuration", {})
    tool = release.get("tool", {})
    if expected["collection"] == "masters-otb":
        formats["source_attribution"] = "source-attribution.jsonl"
        sources = {
            "publisher": source.get("publisher"),
            "product": source.get("product"),
            "publisher_version": source.get("publisher_version"),
            "publisher_download_url": source.get("publisher_download_url"),
            "publisher_terms_url": source.get("publisher_terms_url"),
            "publisher_checksum": source.get("publisher_checksum"),
            "source_tag_counts": qualification.get("source_tag_counts", {}),
            "mode_provenance_counts": qualification.get("mode_provenance_counts", {}),
            "otb_provenance_tier": qualification.get("otb_provenance_tier"),
        }
        license_record = {
            "dataset": (
                "Lumbra's Gigabase OTB Complete, publisher claim CC BY-NC 4.0; "
                "upstream rights unresolved"
            ),
            "publisher_claim": release.get("license", {}).get("publisher_claim"),
            "source_level_rights": release.get("license", {}).get("source_level_rights"),
            "software": "MIT OR Apache-2.0 (software only; does not apply to dataset contents)",
            "notice": notice_name(release_id),
            "elite_online": "separate collection under CC0 1.0; not part of this release",
        }
        distribution = {
            "clearance_file": "releases/distribution_clearance.json",
            "part_size_limit_bytes": PART_LIMIT_BYTES,
        }
    else:
        catalog_archives = elite_source_archives()
        catalog_identity = elite_catalog_identity()
        inputs_by_id = {item.get("id"): item for item in release.get("inputs", [])}
        if set(inputs_by_id) != {item["id"] for item in catalog_archives}:
            raise ReleaseError("Elite release inputs do not match the seven pinned baseline archives")
        for archive in catalog_archives:
            item = inputs_by_id[archive["id"]]
            if any((item.get("filename") != archive["filename"],
                    item.get("period") != archive["period"],
                    item.get("url") != archive["url"],
                    item.get("license") != archive["license"],
                    item.get("sha256") != archive["publisher_sha256"],
                    item.get("size_bytes") != archive["size_bytes"])):
                raise ReleaseError(f"Elite source input {archive['id']} differs from the pinned catalog")
        sources = {
            "publisher": policy["publisher"],
            "profile": "baseline",
            "archive_index_url": policy["source_url"],
            "periods": policy["source_periods"],
            "source_catalog": release.get("source_catalog", catalog_identity),
            "archives": catalog_archives,
        }
        license_record = {
            "dataset": policy["license_name"],
            "id": policy["license_id"],
            "name": policy["license_name"],
            "url": policy["license_url"],
            "publisher": policy["publisher"],
            "software": "MIT OR Apache-2.0 (software only; does not apply to dataset contents)",
            "notice": notice_name(release_id),
        }
        distribution = {"basis": policy["license_id"], "part_size_limit_bytes": PART_LIMIT_BYTES}
        formats["position_index"] = format_versions.get("position_index", "CPI/v2")
    manifest = {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "release_id": release_id,
        "tag": tag_for(release_id),
        "dataset_version": release["dataset_version"],
        "collection": expected["collection"],
        "built_at": release.get("built_at"),
        "game_count": release["accepted_games"],
        "accounting": release["accounting"],
        "index": {
            "format": format_versions.get("position_index", "CPI/v2"),
            "depth_plies": release["index"]["depth_plies"],
            "unique_positions": release["index"]["unique_positions"],
            "unique_position_move_pairs": release["index"].get(
                "unique_position_move_pairs", release["index"].get("total_move_records")
            ),
        },
        "formats": formats,
        "sources": sources,
        "license": license_record,
        "distribution": distribution,
        "assets": assets,
        "metadata_assets": [manifest_name(release_id), sums_name(release_id)],
        "reconstruction": {
            "shell": "cat <name>.part-* > <name>  (order: part-001, part-002, ...)",
            "tool": "python3 scripts/release_assets.py reconstruct",
        },
    }
    if expected["collection"] == "elite-online":
        manifest["build"] = build
        manifest["policies"] = {
            "curation_policy_version": curation.get("version"),
            "curation_policy": curation.get("metadata"),
            "deduplication_policy_version": curation.get("deduplication_policy_version"),
        }
        manifest["tool"] = tool
        manifest["export"] = {
            "tool": "opening-explorer export-pgn",
            "report": export_report,
        }
    return manifest


def build_elite_notice(release_id: str, release: dict[str, Any]) -> str:
    policy = COLLECTION_POLICIES["elite-online"]
    periods = ", ".join(policy["source_periods"])
    curating = release.get("curation_policy", {})
    curation_version = curating.get("version", "elite-online baseline")
    dedupe_version = curating.get("deduplication_policy_version", "the release-recorded policy")
    index = release["index"]
    return f"""# Dataset notice: {release_id}

## Dataset and license

Elite Online R1 contains {release['accepted_games']:,} canonical games curated from Lichess standard rated archives. The archives are offered under [CC0 1.0 Universal]({policy['license_url']}); the curated dataset may be redistributed, including commercially, under CC0. The Opening Explorer API software is separately licensed under MIT OR Apache-2.0; those software licenses do not apply to the dataset.

Source: [Lichess database exports]({policy['source_url']}). This project is not endorsed by Lichess.

## Coverage and curation

This release covers selected monthly archives from 2015 through 2017: {periods}. It is historical coverage, not contemporary Elite Online play.

The source records were filtered under `{curation_version}` and deduplicated under `{dedupe_version}`. The resulting games were normalized into the Opening Explorer storage format and indexed to {index['depth_plies']} plies ({index['unique_positions']:,} positions). The filtered PGN re-serializes the canonical stored games in legal SAN; it is not a byte copy of the source PGN.

Each PGN preserves available names, ratings, date, event, site, round, result, ECO, time control, source URL, and a nonstandard starting FEN where present. Original comments, annotations, variations, clock data, and other source PGN annotations are unavailable after ingestion.

## Release identity

- Release: `{release_id}`
- Dataset build: `{release['dataset_version']}`
- Canonical games: {release['accepted_games']:,}
- CPI depth: {index['depth_plies']} plies
- Unique positions: {index['unique_positions']:,}

The archive checksums and pinned publisher checksums for all seven source archives are listed in the release manifest.
"""


def render_sums(manifest: dict[str, Any]) -> str:
    lines = []
    for asset in manifest["assets"]:
        for part in asset["parts"]:
            lines.append(f"{part['sha256']}  {part['name']}")
    lines.append(f"{manifest['assets'][0]['sha256']}  {manifest['assets'][0]['name']}  (reconstructed)")
    lines.append(f"{manifest['assets'][1]['sha256']}  {manifest['assets'][1]['name']}  (reconstructed)")
    return "\n".join(lines) + "\n"


def write_text_atomic(path: Path, text: str) -> None:
    partial = path.with_name(path.name + ".partial")
    partial.write_text(text, encoding="utf-8")
    os.replace(partial, path)


# --------------------------------------------------------------------------- #
# Commands
# --------------------------------------------------------------------------- #
def resolve_codb(explicit: Path | None) -> Path:
    if explicit is not None:
        candidate = explicit
    else:
        candidate = None
        for name in ("opening-explorer", "codb"):
            path = REPO_ROOT / "target" / "release" / name
            if path.is_file():
                candidate = path
                break
        if candidate is None:
            raise ReleaseError("no binary found; pass --codb")
    if not candidate.is_file() or not os.access(candidate, os.X_OK):
        raise ReleaseError(f"not an executable binary: {candidate}")
    return candidate.resolve()


def cmd_prepare(args: argparse.Namespace) -> int:
    release_id = args.release_id
    if release_id not in EXPECTED_RELEASES:
        raise ReleaseError(f"unknown release id {release_id!r}")
    expected = EXPECTED_RELEASES[release_id]
    collection = expected["collection"]
    policy = COLLECTION_POLICIES[collection]
    live_root = Path(args.live_root).resolve() if args.live_root else live_root_for(collection)
    codb = resolve_codb(args.codb)
    problem = corpus.verify_release_files(live_root)
    if problem:
        raise ReleaseError(f"active snapshot failed artifact verification: {problem}")
    version_dir = corpus.current_version_dir(live_root)
    release = load_json(version_dir / "RELEASE" / "RELEASE.json")
    check_release_identity(release, expected)

    dist = Path(args.dist_dir).resolve() if args.dist_dir else (DIST_DIR / release_id).resolve()
    default_work = REPO_ROOT / "data" / "work" / "release-dist" / release_id
    work = Path(args.work_dir).resolve() if args.work_dir else default_work
    dist.mkdir(parents=True, exist_ok=True)
    RELEASES_DIR.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    names = asset_names(release_id)

    version_manifest = load_json(version_dir / "manifest.json")
    if policy["distribution_gate"] == "clearance":
        clearance = load_clearance()
        notice_values = {
            "RELEASE_ID": release_id,
            "POLICY_ID": str(release.get("qualification", {}).get("policy_id")),
            "DEDUP_POLICY": str(version_manifest.get("deduplication_policy_version")),
            "TOTAL_ENCOUNTERED": f"{release['accounting']['total_encountered']:,}",
            "TAG": tag_for(release_id),
            "DATASET_VERSION": release["dataset_version"],
            "GAME_COUNT": f"{release['accepted_games']:,}",
            "DEPTH": str(release["index"]["depth_plies"]),
            "POSITIONS": f"{release['index']['unique_positions']:,}",
            "POSITION_MOVE_PAIRS": f"{release['index'].get('unique_position_move_pairs', release['index'].get('total_move_records')):,}",
            "PUBLISHER_VERSION": str(release.get("source", {}).get("publisher_version")),
            "BUILT_AT": str(release.get("built_at")),
            "DISTRIBUTION_STATUS": str(clearance.get("status")),
            "SOURCE_TAG_TABLE": source_tag_table(release),
        }
        notice_text = render_notice(NOTICE_TEMPLATE.read_text(encoding="utf-8"), notice_values)
    else:
        notice_text = build_elite_notice(release_id, release)
    notice_work = work / notice_name(release_id)
    write_text_atomic(notice_work, notice_text)

    log("[prepare] building indexed package (deterministic tar)")
    tar_work = work / names["indexed"]
    build_indexed_tar(live_root, tar_work, notice_work, collection)
    write_sha_sidecar(tar_work)
    if not args.skip_package_verify:
        verify_indexed_package(tar_work, codb, expected)
        log(f"[prepare] indexed package extraction and checks passed: {tar_work.name}")

    log("[prepare] exporting filtered PGN with opening-explorer export-pgn")
    pgn_work = work / names["filtered"]
    report_work = work / f"{release_id}-export-pgn-report.json"
    if pgn_work.exists():
        raise ReleaseError(f"refusing to overwrite {pgn_work}")
    subprocess.run(
        [str(codb), "export-pgn", "--dataset", str(version_dir),
         "--output", str(pgn_work), "--report", str(report_work)],
        check=True,
    )
    export_report = load_json(report_work)

    log("[prepare] splitting products into release parts")
    assets = []
    for kind, source in (("indexed", tar_work), ("filtered", pgn_work)):
        entry = split_product(source, dist, names[kind], args.part_limit)
        entry["kind"] = {"indexed": "indexed-package", "filtered": "filtered-pgn"}[kind]
        assets.append(entry)
        log(f"[prepare] {names[kind]}: {entry['size_bytes']} bytes, {len(entry['parts'])} part(s)")

    notice_dest = dist / notice_name(release_id)
    shutil.copyfile(notice_work, notice_dest)
    manifest = build_manifest(release_id, assets, release, export_report)
    validate_manifest(manifest)
    manifest_text = json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    write_text_atomic(dist / manifest_name(release_id), manifest_text)
    write_text_atomic(RELEASES_DIR / f"{release_id}.json", manifest_text)
    write_text_atomic(dist / sums_name(release_id), render_sums(manifest))
    verify_dist(manifest, dist)
    log(f"[prepare] wrote {dist} and {RELEASES_DIR / (release_id + '.json')}")
    return 0


def source_tag_table(release: dict[str, Any]) -> str:
    counts = release.get("qualification", {}).get("source_tag_counts", {})
    rows = ["| Source tag | Source records |", "| --- | ---: |"]
    for tag in sorted(counts):
        rows.append(f"| {tag} | {counts[tag]:,} |")
    return "\n".join(rows)


def cmd_check_manifest(args: argparse.Namespace) -> int:
    manifest_path = Path(args.manifest)
    manifest = validate_manifest(load_json(manifest_path))
    blockers = distribution_blockers(manifest, Path(args.clearance))
    log(f"[check-manifest] {manifest['tag']}: schema, facts, and asset naming valid")
    if blockers:
        for blocker in blockers:
            log(f"[check-manifest] BLOCKED: {blocker}")
        return 3 if args.require_clearance or args.require_policy else 0
    if policy_for_release(manifest["release_id"])["distribution_gate"] == "clearance":
        log("[check-manifest] Masters distribution clearance gate cleared")
    else:
        log("[check-manifest] CC0 license and source provenance validated; no clearance file required")
    return 0


def cmd_verify(args: argparse.Namespace) -> int:
    manifest_path = Path(args.manifest)
    manifest = validate_manifest(load_json(manifest_path))
    dist = Path(args.dist_dir) if args.dist_dir else DIST_DIR / manifest["release_id"]
    verify_dist(manifest, dist)
    uploaded = (dist / manifest_name(manifest["release_id"])).read_bytes()
    if uploaded != manifest_path.read_bytes():
        raise ReleaseError("the dist manifest asset differs from the manifest being checked")
    if args.committed:
        committed = RELEASES_DIR / f"{manifest['release_id']}.json"
        if committed.read_bytes() != manifest_path.read_bytes():
            raise ReleaseError("manifest differs from the committed releases/ copy")
        log("[verify] manifest matches the committed releases/ copy")
    log("[verify] manifest asset matches; all parts and whole-product checksums passed")
    return 0


def cmd_reconstruct(args: argparse.Namespace) -> int:
    manifest = validate_manifest(load_json(Path(args.manifest)))
    dist = Path(args.dist_dir) if args.dist_dir else DIST_DIR / manifest["release_id"]
    reconstruct(manifest, dist, Path(args.output_dir))
    return 0


def cmd_install(args: argparse.Namespace) -> int:
    target = Path(args.target).resolve()
    for live_collection in COLLECTION_POLICIES:
        live_root = live_root_for(live_collection)
        if target == live_root or target.is_relative_to(live_root):
            live_label = "Masters" if live_collection == "masters-otb" else "Elite Online"
            raise ReleaseError(f"refusing to install into the live {live_label} root; choose a scratch root")
    manifest = validate_manifest(load_json(Path(args.manifest)))
    collection = manifest["collection"]
    codb = resolve_codb(args.codb)
    package = Path(args.package).resolve()
    indexed = next(asset for asset in manifest["assets"] if asset["kind"] == "indexed-package")
    if package.name != indexed["name"]:
        raise ReleaseError(f"package must be {indexed['name']}")
    sidecar = package.with_name(package.name + ".sha256")
    if not sidecar.is_file():
        write_sha_sidecar(package)
    if sha256_file(package) != indexed["sha256"]:
        raise ReleaseError("indexed package SHA-256 differs from the manifest")
    if package.stat().st_size != indexed["size_bytes"]:
        raise ReleaseError("indexed package size differs from the manifest")

    target.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".opening-explorer-install-", dir=target.parent))
    try:
        root = corpus.extract_release_archive(package, staging / "extract")
        problem = corpus.verify_release_files(root)
        if problem:
            raise ReleaseError(f"extracted package failed artifact verification: {problem}")
        version_dir = corpus.current_version_dir(root)
        if version_dir.name != manifest["dataset_version"]:
            raise ReleaseError("extracted version does not match the manifest")
        release = load_json(version_dir / "RELEASE" / "RELEASE.json")
        check_release_identity(release, EXPECTED_RELEASES[manifest["release_id"]])
        run_codb_verify(codb, version_dir)
        publish = subprocess.run(
            [str(codb), "publish", "--candidate", str(version_dir),
             "--target", str(target), "--collection", collection],
            capture_output=True, text=True,
        )
        if publish.returncode != 0:
            raise ReleaseError(f"codb publish failed:\n{publish.stdout}{publish.stderr}")
        log(f"[install] {publish.stdout.strip()}")
        run_codb_verify(codb, target)
        active = corpus.current_version_dir(target).name
        if active != manifest["dataset_version"]:
            raise ReleaseError(f"installed current is {active}, expected {manifest['dataset_version']}")
        log(f"[install] {target}/current -> {active}")
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return 0


def cmd_upload(args: argparse.Namespace) -> int:
    manifest = validate_manifest(load_json(Path(args.manifest)))
    release_id = manifest["release_id"]
    tag = manifest["tag"]
    if args.tag and args.tag != tag:
        raise ReleaseError(f"--tag {args.tag} does not match the manifest tag {tag}")
    dist = Path(args.dist_dir) if args.dist_dir else DIST_DIR / release_id
    verify_dist(manifest, dist)
    blockers = distribution_blockers(manifest)

    commit = git_output(["rev-parse", "--verify", "--quiet", f"refs/tags/{tag}^{{commit}}"], check=False)
    if not commit:
        blockers.append(f"tag {tag} does not exist locally; create it on the reviewed merged commit")
    else:
        main_ref = next((ref for ref in ("origin/main", "main")
                         if git_output(["rev-parse", "--verify", "--quiet", ref], check=False)), None)
        if main_ref is None:
            blockers.append("neither origin/main nor main is available to check reachability")
        elif subprocess.run(["git", "-C", str(REPO_ROOT), "merge-base", "--is-ancestor", commit, main_ref],
                            capture_output=True).returncode != 0:
            blockers.append(f"tag {tag} ({commit[:12]}) is not reachable from {main_ref}")
        tagged = git_bytes(["show", f"{commit}:releases/{release_id}.json"])
        if tagged is None or tagged != Path(args.manifest).read_bytes():
            blockers.append(f"the tagged commit does not contain this manifest at releases/{release_id}.json")
    exists = gh_release_exists(tag)
    if exists is None:
        blockers.append("the gh CLI is unavailable, so release existence cannot be checked")
    elif exists:
        blockers.append(f"a GitHub release for {tag} already exists; tags and releases are immutable")

    files = [dist / name for name in sorted(product_part_names_all(manifest))]
    files += [dist / notice_name(release_id), dist / manifest_name(release_id), dist / sums_name(release_id)]
    command = ["gh", "release", "create", tag, "--draft", "--verify-tag",
               "--title", release_title(manifest),
               "--notes-file", str(dist / notice_name(release_id)),
               *[str(path) for path in files]]
    log(f"[upload] {len(files)} files, {sum(p.stat().st_size for p in files)} bytes")
    log("[upload] command: " + " ".join(command[:9]) + " <files>")
    if not args.execute:
        log("[upload] DRY RUN: nothing was uploaded.")
        for blocker in blockers:
            log(f"[upload] would refuse --execute: {blocker}")
        return 0
    if blockers:
        for blocker in blockers:
            log(f"[upload] refused: {blocker}")
        return 3
    subprocess.run(command, check=True)
    log(f"[upload] created DRAFT release {tag}; publish only through the publish-dataset workflow")
    return 0


def product_part_names_all(manifest: dict[str, Any]) -> list[str]:
    names: list[str] = []
    for asset in manifest["assets"]:
        names.extend(product_part_names(asset))
    return names


def release_title(manifest: dict[str, Any]) -> str:
    if manifest["collection"] == "masters-otb":
        return f"Masters OTB dataset {manifest['release_id']}"
    if manifest["collection"] == "elite-online":
        return "Elite Online R1 — Curated Lichess Games"
    raise ReleaseError(f"unsupported collection {manifest['collection']}")


def git_output(arguments: list[str], check: bool = True) -> str | None:
    result = subprocess.run(["git", "-C", str(REPO_ROOT), *arguments], capture_output=True, text=True)
    if result.returncode != 0:
        if check:
            raise ReleaseError(f"git {' '.join(arguments)} failed: {result.stderr.strip()}")
        return None
    return result.stdout.strip() or None


def git_bytes(arguments: list[str]) -> bytes | None:
    result = subprocess.run(["git", "-C", str(REPO_ROOT), *arguments], capture_output=True)
    return result.stdout if result.returncode == 0 else None


def gh_release_exists(tag: str) -> bool | None:
    """True or False when GitHub answers; None when the gh CLI is unavailable."""
    try:
        result = subprocess.run(["gh", "release", "view", tag], capture_output=True, text=True)
    except FileNotFoundError:
        return None
    return result.returncode == 0


def cmd_verify_remote(args: argparse.Namespace) -> int:
    """Check a draft release's asset list (JSON from `gh api`) against the manifest."""
    manifest = validate_manifest(load_json(Path(args.manifest)))
    remote = load_json(Path(args.assets_json))
    if remote.get("tag_name") != manifest["tag"]:
        raise ReleaseError("draft release tag does not match the manifest")
    if remote.get("draft") is not True:
        raise ReleaseError("release is not a draft; refusing to act on a published release")
    listed = {item["name"]: item.get("size") for item in remote.get("assets", [])}
    expected = {name: None for name in dist_expected_files(manifest)}
    missing = sorted(set(expected) - set(listed))
    if missing:
        raise ReleaseError(f"draft release is missing assets: {', '.join(missing)}")
    unexpected = sorted(set(listed) - set(expected))
    if unexpected:
        raise ReleaseError(f"draft release has unexpected assets: {', '.join(unexpected)}")
    sizes = {part["name"]: part["size_bytes"] for asset in manifest["assets"] for part in asset["parts"]}
    for name, size in sizes.items():
        if listed[name] != size:
            raise ReleaseError(f"draft asset {name} has size {listed[name]}, expected {size}")
    log(f"[verify-remote] {len(listed)} draft assets match the manifest by name and size")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    sp = sub.add_parser("prepare", help="build both products from the active snapshot")
    sp.add_argument("--release-id", default="masters-otb-r3")
    sp.add_argument("--live-root", default=None)
    sp.add_argument("--dist-dir", default=None)
    sp.add_argument("--work-dir", default=None)
    sp.add_argument("--codb", type=Path, default=None)
    sp.add_argument("--part-limit", type=int, default=PART_LIMIT_BYTES)
    sp.add_argument("--skip-package-verify", action="store_true")
    sp.set_defaults(fn=cmd_prepare)

    sp = sub.add_parser("check-manifest", help="validate the committed manifest and collection license gate")
    sp.add_argument("--manifest", required=True)
    sp.add_argument("--clearance", default=str(CLEARANCE_PATH))
    sp.add_argument("--require-clearance", action="store_true")
    sp.add_argument("--require-policy", action="store_true",
                    help="require the configured collection-specific distribution policy to pass")
    sp.set_defaults(fn=cmd_check_manifest)

    sp = sub.add_parser("verify", help="verify a dist directory against a manifest")
    sp.add_argument("--manifest", required=True)
    sp.add_argument("--dist-dir", default=None)
    sp.add_argument("--committed", action="store_true", help="also require equality with releases/")
    sp.set_defaults(fn=cmd_verify)

    sp = sub.add_parser("reconstruct", help="rebuild whole products from parts")
    sp.add_argument("--manifest", required=True)
    sp.add_argument("--dist-dir", default=None)
    sp.add_argument("--output-dir", required=True)
    sp.set_defaults(fn=cmd_reconstruct)

    sp = sub.add_parser("install", help="install the indexed package into a scratch dataset root")
    sp.add_argument("--manifest", required=True)
    sp.add_argument("--package", required=True, help="reconstructed indexed .tar")
    sp.add_argument("--target", required=True, help="collection root, e.g. a scratch masters-otb directory")
    sp.add_argument("--codb", type=Path, default=None)
    sp.set_defaults(fn=cmd_install)

    sp = sub.add_parser("upload", help="dry-run by default; --execute creates a DRAFT release")
    sp.add_argument("--manifest", required=True)
    sp.add_argument("--dist-dir", default=None)
    sp.add_argument("--tag", default=None)
    sp.add_argument("--execute", action="store_true")
    sp.set_defaults(fn=cmd_upload)

    sp = sub.add_parser("verify-remote", help="check a draft release listing (gh api JSON)")
    sp.add_argument("--manifest", required=True)
    sp.add_argument("--assets-json", required=True)
    sp.set_defaults(fn=cmd_verify_remote)
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = build_parser().parse_args(list(argv) if argv is not None else None)
    return args.fn(args)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ReleaseError, corpus.CorpusError, subprocess.CalledProcessError) as error:
        print(f"release failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
