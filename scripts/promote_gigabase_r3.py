#!/usr/bin/env python3
"""Verify and atomically publish the existing local Gigabase R3 snapshot."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CANDIDATE = ROOT / "data/work/gigabase-eval/docker-r3-smoke/releases/masters-otb/versions/codb-masters-otb-1794774904566910568-6701-5698b67c3952ca6b-cpi-d40"
BUILD_ID = "codb-masters-otb-1794774904566910568-6701-5698b67c3952ca6b-cpi-d40"
RELEASE_MANIFEST_SHA256 = "5b5208e2575cdb1e885fef7326fd38429f74736a158033d349ff9ef4eb9fd58b"
EXPECTED_ARTIFACTS = 18


class PromotionError(RuntimeError):
    """A fail-closed promotion error."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_and_verify_release(candidate: Path) -> dict:
    candidate = candidate.resolve(strict=True)
    release_path = candidate / "RELEASE" / "RELEASE.json"
    try:
        release = json.loads(release_path.read_text(encoding="utf-8"))
        manifest = json.loads((candidate / "manifest.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PromotionError(f"candidate release metadata is unavailable or invalid: {error}") from error

    if sha256_file(release_path) != RELEASE_MANIFEST_SHA256:
        raise PromotionError("candidate RELEASE.json is not the verified R3 release manifest")

    if candidate.name != BUILD_ID or release.get("dataset_version") != BUILD_ID:
        raise PromotionError("candidate is not the pinned Gigabase R3 release")
    if (manifest.get("dataset_build_id") != BUILD_ID or manifest.get("collection") != "masters-otb"
            or manifest.get("total_games") != 3_284_429):
        raise PromotionError("Opening Explorer manifest identity or collection does not match Gigabase R3 Masters")
    if release.get("collection") != "masters-otb" or release.get("accepted_games") != 3_284_429:
        raise PromotionError("release metadata does not record 3,284,429 Masters games")
    if release.get("index", {}).get("depth_plies") != 40:
        raise PromotionError("release metadata does not record CPI depth 40")
    if release.get("index", {}).get("unique_positions") != 51_909_604:
        raise PromotionError("release metadata does not record 51,909,604 indexed positions")
    accounting = release.get("accounting", {})
    if (accounting.get("accepted_unique") != 3_284_429
            or accounting.get("total_encountered") != 13_450_696
            or accounting.get("accepted_unique") + accounting.get("confirmed_duplicates", -1)
            + accounting.get("superseded_prior", -1) + accounting.get("rejected_by_policy", -1)
            + accounting.get("rejected_variant", -1) + accounting.get("rejected_illegal", -1)
            + accounting.get("rejected_malformed", -1) + accounting.get("quarantined", -1)
            != accounting.get("total_encountered")):
        raise PromotionError("R3 source accounting is incomplete or inconsistent")
    source = release.get("source", {})
    if (not isinstance(source, dict)
            or source.get("publisher") != "Lumbra's Gigabase"
            or source.get("source_kind") != "local_evaluation_only"
            or source.get("release_eligible") is not False
            or release.get("public_release_eligible") is not False
            or release.get("license", {}).get("publisher_checksum") is not None
            or release.get("license", {}).get("source_level_rights") != "unresolved"):
        raise PromotionError("local-only source and rights restrictions are not intact")

    artifacts = release.get("artifacts")
    if not isinstance(artifacts, dict) or len(artifacts) != EXPECTED_ARTIFACTS:
        raise PromotionError(f"expected {EXPECTED_ARTIFACTS} release artifacts")
    base = candidate.resolve()
    for relative, metadata in artifacts.items():
        relpath = PurePosixPath(relative)
        if relpath.is_absolute() or not relpath.parts or any(part in ("", ".", "..") for part in relpath.parts):
            raise PromotionError(f"unsafe artifact path in release manifest: {relative}")
        artifact = candidate.joinpath(*relpath.parts)
        if artifact.is_symlink() or not artifact.is_file() or not artifact.resolve().is_relative_to(base):
            raise PromotionError(f"artifact is missing or not a regular in-release file: {relative}")
        if artifact.stat().st_size != metadata.get("size_bytes"):
            raise PromotionError(f"artifact size mismatch: {relative}")
        actual_hash = sha256_file(artifact)
        if actual_hash != metadata.get("sha256"):
            raise PromotionError(f"artifact SHA-256 mismatch: {relative}")
        print(f"[verify] {relative} {actual_hash}")
    return release


def run_opening_explorer(opening_explorer: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    result = subprocess.run([str(opening_explorer), *arguments], capture_output=True, text=True)
    if result.returncode:
        raise PromotionError(f"opening-explorer {' '.join(arguments)} failed:\n{result.stdout}{result.stderr}")
    return result


def current_version(target: Path) -> str | None:
    pointer = target / "current"
    if not os.path.lexists(pointer):
        return None
    if not pointer.is_symlink():
        raise PromotionError(f"refusing target with non-symlink current pointer: {pointer}")
    resolved = (target / os.readlink(pointer)).resolve(strict=True)
    if not resolved.is_relative_to((target / "versions").resolve(strict=True)):
        raise PromotionError("current pointer resolves outside the target versions directory")
    return resolved.name


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", type=Path, default=DEFAULT_CANDIDATE)
    parser.add_argument("--target", type=Path, default=ROOT / "data/releases/masters-otb")
    parser.add_argument("--opening-explorer", type=Path, default=ROOT / "target/release/chess-database")
    parser.add_argument("--publish", action="store_true", help="publish after checksum and deep verification")
    args = parser.parse_args()

    candidate = args.candidate.resolve(strict=True)
    target = args.target.absolute()
    opening_explorer = args.opening_explorer.resolve(strict=True)
    release = load_and_verify_release(candidate)
    run_opening_explorer(opening_explorer, "verify", "--dataset", str(candidate), "--deep")
    previous = current_version(target)
    print(f"[verified] {release['dataset_version']} games={release['accepted_games']} "
          f"depth={release['index']['depth_plies']} previous={previous or 'none'}")
    if not args.publish:
        print("[check-only] pass --publish to invoke the atomic Opening Explorer publisher")
        return 0
    if previous == BUILD_ID:
        print("[published] Gigabase R3 is already active")
        return 0

    target.parent.mkdir(parents=True, exist_ok=True)
    staging_root = target.parent / f".opening-explorer-gigabase-promotion-{os.getpid()}-{time.time_ns()}"
    staging_root.mkdir(parents=True, exist_ok=False)
    staged = staging_root / BUILD_ID
    shutil.copytree(candidate, staged)
    # Recheck the staged copy so publication never trusts a partial copy.
    load_and_verify_release(staged)
    run_opening_explorer(opening_explorer, "verify", "--dataset", str(staged), "--deep")
    run_opening_explorer(opening_explorer, "publish", "--candidate", str(staged), "--target", str(target),
             "--collection", "masters-otb")
    active = current_version(target)
    if active != BUILD_ID:
        raise PromotionError(f"publication returned without activating {BUILD_ID}; current is {active}")
    run_opening_explorer(opening_explorer, "verify", "--dataset", str(target), "--deep")
    load_and_verify_release(target / "current")
    if previous and not (target / "versions" / previous).is_dir():
        raise PromotionError(f"previous version {previous} is missing after publication")
    print(f"[published] {target}/current -> versions/{BUILD_ID}; previous={previous or 'none'}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, PromotionError) as error:
        print(f"promotion failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
