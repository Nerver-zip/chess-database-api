#!/usr/bin/env python3
"""Build and operate a restricted local Gigabase evaluation package.

This workflow is deliberately separate from corpus.py's approved source catalog. It
pins locally computed hashes, keeps the publisher checksum absent, writes an
evaluation-only notice, and installs only below data/work/gigabase-eval/.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import corpus


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_WORK_ROOT = REPO_ROOT / "data" / "work" / "gigabase-eval"
POLICY_PATH = REPO_ROOT / "corpus" / "gigabase_source_qualification.json"
PUBLISHER_DOWNLOAD_URL = "https://lumbrasgigabase.com/en/download-area-en/"
PUBLISHER_TERMS_URL = "https://lumbrasgigabase.com/en/commercial-licenses-en/"
PUBLISHER_SOURCES_URL = "https://lumbrasgigabase.com/en/sources/"
PUBLISHER_VERSION = "2026-11-11"
PUBLISHER_LICENSE_CLAIM = "CC BY-NC 4.0"
COLLECTION = "masters-otb"


class EvaluationError(RuntimeError):
    """Raised when an evaluation artifact fails a fail-closed check."""


def _regular_file(path: Path, label: str) -> Path:
    path = Path(path)
    try:
        metadata = path.lstat()
    except OSError as error:
        raise EvaluationError(f"{label} is unavailable: {path}: {error}") from error
    if not path.is_file() or path.is_symlink():
        raise EvaluationError(f"{label} must be a regular non-symlink file: {path}")
    if metadata.st_size <= 0:
        raise EvaluationError(f"{label} is empty: {path}")
    return path.resolve()


def _work_path(path: Path, label: str, *, must_exist: bool = False) -> Path:
    path = Path(path).absolute()
    parent = path.parent.resolve()
    resolved = parent / path.name
    work_root = DEFAULT_WORK_ROOT.resolve()
    if not resolved.is_relative_to(work_root):
        raise EvaluationError(
            f"{label} must stay under the ignored local evaluation workspace {work_root}: {path}"
        )
    if path.is_symlink():
        raise EvaluationError(f"{label} must not be a symlink: {path}")
    if must_exist and not path.exists():
        raise EvaluationError(f"{label} does not exist: {path}")
    return resolved


def _write_json_new(path: Path, value: dict[str, Any]) -> None:
    path = _work_path(path, "output file")
    path.parent.mkdir(parents=True, exist_ok=True)
    if os.path.lexists(path):
        raise EvaluationError(f"refusing to overwrite {path}")
    temporary = path.with_name(path.name + f".tmp-{os.getpid()}")
    try:
        with temporary.open("x", encoding="utf-8") as output:
            json.dump(value, output, indent=2, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.link(temporary, path)
        temporary.unlink()
    except Exception:
        temporary.unlink(missing_ok=True)
        raise


def _sha256(path: Path) -> str:
    return corpus.sha256_file(path)


def register_source(archive: Path, pgn: Path, local_record: Path, output: Path) -> dict[str, Any]:
    archive = _regular_file(archive, "source archive")
    pgn = _regular_file(pgn, "extracted PGN member")
    local_record = _regular_file(local_record, "local extraction record")
    try:
        record = json.loads(local_record.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise EvaluationError(f"invalid local extraction record {local_record}: {error}") from error

    if isinstance(record.get("archive"), dict) and isinstance(record.get("members"), list):
        archive_record = record["archive"]
        publisher_record = record.get("publisher", {})
        local_evaluation_record = record.get("local_evaluation", {})
        matching_members = [member for member in record["members"]
                            if member.get("filename") == pgn.name]
        if len(matching_members) != 1:
            raise EvaluationError("archive inventory must identify exactly one matching PGN member")
        member_record = matching_members[0]
        archive_sha256 = archive_record.get("sha256")
        archive_size = archive_record.get("size_bytes")
        member_sha256 = member_record.get("sha256")
        member_size = member_record.get("size_bytes")
        extraction_tool = record.get("archive_verification_tool")
        decompression_check = archive_record.get("full_decompression_check")
        crc_metadata_available = archive_record.get("packed_stream_crc_metadata_available")
        expected_archive_path = (REPO_ROOT / archive_record.get("relative_path", "")).resolve()
        expected_member_path = (REPO_ROOT / member_record.get("relative_path", "")).resolve()
        if expected_archive_path != archive or expected_member_path != pgn:
            raise EvaluationError("source paths differ from the archive inventory")
        if decompression_check != "passed":
            raise EvaluationError("archive inventory does not record a full decompression pass")
        if member_record.get("type") != "regular_file":
            raise EvaluationError("archive inventory member is not a regular file")
        if archive_record.get("publisher_sha256_available") is not False:
            raise EvaluationError("archive inventory must state that no publisher checksum exists")
        if publisher_record.get("publisher_checksum") is not None:
            raise EvaluationError("publisher checksum must remain absent")
        if publisher_record.get("version") != PUBLISHER_VERSION:
            raise EvaluationError("archive inventory publisher version is unexpected")
        if publisher_record.get("license") != PUBLISHER_LICENSE_CLAIM:
            raise EvaluationError("archive inventory publisher license claim is unexpected")
        if local_evaluation_record.get("production_release_eligible") is not False:
            raise EvaluationError("archive inventory must keep the source release-ineligible")
    elif record.get("source_kind") == "user_supplied_local_archive":
        if (record.get("publisher_checksum") is not None
                or record.get("publisher_checksum_verified") is not False
                or record.get("release_eligible") is not False):
            raise EvaluationError("local hashes must remain unauthenticated and release-ineligible")
        archive_sha256 = record.get("archive_sha256_local")
        archive_size = record.get("archive_size_bytes")
        member_sha256 = record.get("member_sha256_local")
        member_size = record.get("member_size_bytes")
        extraction_tool = record.get("extraction_tool")
        decompression_check = "passed"
        crc_metadata_available = None
    else:
        raise EvaluationError("source evidence is neither a local extraction record nor archive inventory")

    if archive_sha256 != _sha256(archive):
        raise EvaluationError("archive bytes do not match the recorded local SHA-256")
    if member_sha256 != _sha256(pgn):
        raise EvaluationError("PGN member bytes do not match the recorded local SHA-256")
    if archive_size != archive.stat().st_size:
        raise EvaluationError("archive size differs from the local extraction record")
    if member_size != pgn.stat().st_size:
        raise EvaluationError("PGN size differs from the local extraction record")

    registration = {
        "schema_version": 1,
        "source_kind": "local_evaluation_only",
        "publisher": "Lumbra's Gigabase",
        "product": "OTB Complete",
        "publisher_version": PUBLISHER_VERSION,
        "publisher_download_url": PUBLISHER_DOWNLOAD_URL,
        "publisher_terms_url": PUBLISHER_TERMS_URL,
        "publisher_sources_url": PUBLISHER_SOURCES_URL,
        "publisher_license_claim": PUBLISHER_LICENSE_CLAIM,
        "publisher_checksum": None,
        "publisher_checksum_verified": False,
        "archive_path_local": str(archive),
        "member_path_local": str(pgn),
        "local_source_evidence_path": str(local_record),
        "local_source_evidence_sha256": _sha256(local_record),
        "local_extraction_tool": extraction_tool,
        "full_decompression_check": decompression_check,
        "packed_stream_crc_metadata_available": crc_metadata_available,
        "archive_filename": archive.name,
        "archive_size_bytes": archive.stat().st_size,
        "archive_sha256_local": _sha256(archive),
        "member_filename": pgn.name,
        "member_size_bytes": pgn.stat().st_size,
        "member_sha256_local": _sha256(pgn),
        "rights_clearance": "unresolved_upstream_rights",
        "policy_id": json.loads(POLICY_PATH.read_text(encoding="utf-8"))["policy_id"],
        "policy_sha256": _sha256(POLICY_PATH),
        "release_eligible": False,
        "registered_at": corpus.utc_now(),
    }
    _write_json_new(output, registration)
    return registration


def load_registration(path: Path) -> tuple[dict[str, Any], Path]:
    path = _regular_file(path, "source registration")
    try:
        registration = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise EvaluationError(f"invalid source registration {path}: {error}") from error
    if registration.get("schema_version") != 1 or registration.get("release_eligible") is not False:
        raise EvaluationError("unsupported or release-eligible local source registration")
    if (registration.get("publisher_checksum") is not None
            or registration.get("publisher_checksum_verified") is not False):
        raise EvaluationError(
            "publisher checksum must remain absent and unverified for this local source"
        )
    if registration.get("publisher_license_claim") != PUBLISHER_LICENSE_CLAIM:
        raise EvaluationError("publisher license claim differs from the reviewed source record")
    if registration.get("rights_clearance") != "unresolved_upstream_rights":
        raise EvaluationError("local source rights status is missing or unexpectedly changed")
    evidence_path = _regular_file(
        Path(registration["local_source_evidence_path"]), "local source evidence"
    )
    if _sha256(evidence_path) != registration.get("local_source_evidence_sha256"):
        raise EvaluationError("local source evidence hash differs from registration")
    if _sha256(POLICY_PATH) != registration.get("policy_sha256"):
        raise EvaluationError("registered source policy digest differs from the current policy")
    archive = _regular_file(Path(registration["archive_path_local"]), "source archive")
    pgn = _regular_file(Path(registration["member_path_local"]), "extracted PGN member")
    allowed_source_root = (REPO_ROOT / "data" / "sources").resolve()
    if not archive.is_relative_to(allowed_source_root) or not pgn.is_relative_to(allowed_source_root):
        raise EvaluationError("registered local source files must stay under git-ignored data/sources")
    if archive.stat().st_size != registration.get("archive_size_bytes"):
        raise EvaluationError("source archive size differs from registration")
    if _sha256(archive) != registration.get("archive_sha256_local"):
        raise EvaluationError("source archive local hash differs from registration")
    if pgn.stat().st_size != registration.get("member_size_bytes"):
        raise EvaluationError("PGN member size differs from registration")
    if _sha256(pgn) != registration.get("member_sha256_local"):
        raise EvaluationError("PGN member local hash differs from registration")
    return registration, pgn


def find_opening_explorer(value: str | None) -> Path:
    candidate = Path(value) if value else REPO_ROOT / "target" / "release" / "opening-explorer"
    if candidate.is_file() and os.access(candidate, os.X_OK):
        return candidate.resolve()
    located = shutil.which(value or "opening-explorer")
    if located:
        return Path(located).resolve()
    raise EvaluationError("opening-explorer executable not found; build it or pass --opening-explorer")


def run_opening_explorer(opening_explorer: Path, arguments: list[str]) -> subprocess.CompletedProcess[str]:
    result = subprocess.run([str(opening_explorer), *arguments], capture_output=True, text=True)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise EvaluationError(f"opening-explorer {' '.join(arguments)} failed: {detail}")
    return result


def run_measured_opening_explorer(
    opening_explorer: Path,
    arguments: list[str],
    *,
    log_path: Path,
    environment: dict[str, str],
    sample_paths: list[Path],
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    """Run one production opening-explorer stage and retain process and workspace measurements."""
    command = [str(opening_explorer), *arguments]
    metrics = corpus.run_measured(command, log_path, environment, sample_paths)
    output = log_path.read_text(encoding="utf-8", errors="replace")
    result = subprocess.CompletedProcess(command, metrics["returncode"], output, "")
    if result.returncode != 0:
        detail = output.strip()[-4000:] or "no command output"
        raise EvaluationError(f"opening-explorer {' '.join(arguments)} failed: {detail}")
    return result, metrics


def read_json_output(result: subprocess.CompletedProcess[str], label: str) -> dict[str, Any]:
    try:
        parsed = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise EvaluationError(f"{label} did not emit valid JSON: {error}") from error
    if not isinstance(parsed, dict):
        raise EvaluationError(f"{label} JSON output must be an object")
    return parsed


def _source_attribution_counts(snapshot: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    attribution = manifest.get("source_attribution")
    if not attribution:
        raise EvaluationError("Gigabase snapshot is missing its source-attribution sidecar")
    path = snapshot / attribution["filename"]
    if not path.is_file() or path.is_symlink() or _sha256(path) != attribution["sha256"]:
        raise EvaluationError("source-attribution sidecar failed its manifest hash")
    source_counts: dict[str, int] = {}
    mode_counts: dict[str, int] = {}
    with path.open(encoding="utf-8") as source_file:
        for line_number, line in enumerate(source_file, 1):
            try:
                item = json.loads(line)
            except json.JSONDecodeError as error:
                raise EvaluationError(
                    f"invalid attribution record at line {line_number}: {error}"
                ) from error
            records = item.get("source_attributions")
            if not isinstance(records, list) or not records:
                raise EvaluationError(f"attribution line {line_number} has no source records")
            for record in records:
                source = record.get("source_tag") or "missing"
                mode = record.get("mode")
                source_counts.setdefault(source, 0)
                if mode is not None:
                    normalized_mode = "".join(ch.lower() for ch in str(mode) if ch.isalnum())
                    if normalized_mode not in {"otb", "overtheboard"}:
                        raise EvaluationError(
                            f"accepted game attribution has non-OTB Mode: {mode!r}"
                        )
            # Count each source once for this canonical game, even when the deduplicator
            # preserved several upstream attributions for it.
            for source in {record.get("source_tag") or "missing" for record in records}:
                source_counts[source] = source_counts.get(source, 0) + 1
            modes = {record.get("mode") for record in records}
            if modes == {None}:
                mode_class = "missing_mode"
            elif None in modes:
                mode_class = "mixed_missing_and_explicit_otb"
            else:
                mode_class = "explicit_otb"
            mode_counts[mode_class] = mode_counts.get(mode_class, 0) + 1
    return {
        "source_tag_counts": dict(sorted(source_counts.items())),
        "mode_provenance_counts": dict(sorted(mode_counts.items())),
        "otb_provenance_tier": "publisher_asserted_otb",
        "independent_event_audit_games": 0,
    }


def _registration_summary(registration: dict[str, Any]) -> dict[str, Any]:
    return {
        key: registration[key] for key in (
            "schema_version", "source_kind", "publisher", "product", "publisher_version",
            "publisher_download_url", "publisher_terms_url", "publisher_sources_url",
            "publisher_license_claim", "publisher_checksum", "publisher_checksum_verified",
            "archive_filename", "archive_size_bytes", "archive_sha256_local",
            "member_filename", "member_size_bytes", "member_sha256_local",
            "local_extraction_tool", "full_decompression_check",
            "packed_stream_crc_metadata_available", "local_source_evidence_sha256",
            "policy_id", "policy_sha256", "rights_clearance", "release_eligible",
        )
    }


def _source_license_label(registration: dict[str, Any]) -> str:
    return (
        f"Publisher claim: {registration['publisher_license_claim']}; "
        "upstream rights unresolved"
    )


def _local_source_manifest(registration: dict[str, Any], pgn: Path) -> dict[str, Any]:
    """Pin the build to registered local bytes without claiming publisher authentication."""
    if (registration.get("publisher_checksum") is not None
            or registration.get("publisher_checksum_verified") is not False
            or registration.get("release_eligible") is not False):
        raise EvaluationError(
            "local source manifest requires absent publisher checksum and release eligibility"
        )
    return {
        "collection": COLLECTION,
        "sources": [{
            "path": str(pgn),
            "url": None,
            "sha256": registration["member_sha256_local"],
            "license": _source_license_label(registration),
        }],
    }


def _validate_source_archive_metadata(
    source_archives: Any,
    registration: dict[str, Any],
    evidence_label: str,
) -> None:
    """Require build and dataset manifests to match the locally pinned input."""
    if not isinstance(source_archives, list) or len(source_archives) != 1:
        raise EvaluationError(f"{evidence_label} must contain exactly the registered PGN source")
    source = source_archives[0]
    if not isinstance(source, dict):
        raise EvaluationError(f"{evidence_label} source archive metadata must be an object")
    expected = {
        "filename": registration["member_filename"],
        "url": None,
        "sha256": registration["member_sha256_local"],
        "size_bytes": registration["member_size_bytes"],
        "license": _source_license_label(registration),
        # Opening Explorer verified the manifest-pinned local digest; this is not a publisher checksum.
        "checksum_verified": True,
    }
    if any(source.get(key) != value for key, value in expected.items()):
        raise EvaluationError(
            f"{evidence_label} source archive metadata differs from the registered local source"
        )


def _release_artifacts(version_dir: Path) -> dict[str, dict[str, Any]]:
    release_json = version_dir / "RELEASE" / "RELEASE.json"
    artifacts: dict[str, dict[str, Any]] = {}
    for path in sorted(version_dir.rglob("*")):
        if (path.is_file() and not path.is_symlink() and path != release_json
                and not path.name.startswith(".") and not path.name.endswith(".lock")):
            rel = str(path.relative_to(version_dir))
            artifacts[rel] = {"sha256": _sha256(path), "size_bytes": path.stat().st_size}
    if not artifacts:
        raise EvaluationError("evaluation release has no checksum-covered artifacts")
    return artifacts


def _license_notice(registration: dict[str, Any]) -> str:
    return (
        "LOCAL EVALUATION ONLY — NOT A PUBLIC OR COMMERCIAL RELEASE\n\n"
        "Dataset: Lumbra's Gigabase OTB Complete, publisher version 2026-11-11.\n"
        "The publisher states that the main database is licensed under CC BY-NC 4.0. "
        "This is recorded as a publisher claim, not independent rights clearance for every "
        "upstream game. Upstream rights remain unresolved for this heterogeneous collection.\n\n"
        f"Archive: {registration['archive_filename']} ({registration['archive_size_bytes']} bytes)\n"
        f"Local archive SHA-256: {registration['archive_sha256_local']}\n"
        f"PGN member: {registration['member_filename']} ({registration['member_size_bytes']} bytes)\n"
        f"Local member SHA-256: {registration['member_sha256_local']}\n"
        "The publisher supplied no checksum for this archive; these local hashes do not "
        "authenticate it as a publisher-issued artifact.\n\n"
        f"Recorded full-decompression check: {registration['full_decompression_check']}. "
        f"Extraction tool/version recorded: {registration['local_extraction_tool'] or 'no'}. "
        f"Packed-stream CRC metadata available: {registration['packed_stream_crc_metadata_available']}.\n\n"
        f"Publisher download page: {PUBLISHER_DOWNLOAD_URL}\n"
        f"Publisher source notes: {PUBLISHER_SOURCES_URL}\n"
        f"Publisher B2B terms: {PUBLISHER_TERMS_URL}\n\n"
        "This package is restricted to local internal evaluation. Do not redistribute it, "
        "deploy it to a public or commercial service, or provide raw PGN to end users. The "
        "publisher's current B2B page describes internal development and a closed beta of up "
        "to 50 unpaid testers for up to six months, permits internal derived metadata, and "
        "prohibits end-user bulk/raw PGN downloads. Obtain written confirmation before any "
        "user-facing use.\n"
    )


def build_evaluation(args: argparse.Namespace) -> dict[str, Any]:
    work_root = _work_path(Path(args.work_root), "evaluation workspace")
    output_root = _work_path(Path(args.output_root), "candidate dataset root")
    registration, pgn = load_registration(Path(args.registration))
    opening_explorer = find_opening_explorer(args.opening_explorer)
    if output_root.exists():
        raise EvaluationError(f"candidate output already exists: {output_root}")
    output_root.parent.mkdir(parents=True, exist_ok=True)
    work_root.mkdir(parents=True, exist_ok=True)

    build_report_path = work_root / "build_report.json"
    source_manifest_path = work_root / "sources.manifest.json"
    index_report_path = work_root / "index_report.json"
    quality_report_path = work_root / "quality_report.json"
    resource_metrics_path = work_root / "resource_metrics.json"
    scratch_dir = work_root / "scratch"
    scratch_dir.mkdir(exist_ok=False)
    resource_metrics: dict[str, Any] = {
        "schema_version": 1,
        "measurement_method": (
            "Linux process VmHWM sampled from /proc, with wait4 peak-RSS fallback; current "
            "anonymous, file-backed, and shared RSS components sampled every 5 ms; allocated "
            "bytes sampled every 250 ms across the explicit scratch directory and candidate "
            "dataset root. Component samples may understate their true peaks."
        ),
        "scratch_directory": str(scratch_dir),
        "candidate_root": str(output_root),
        "steps": {},
    }
    command_environment = os.environ.copy()
    command_environment["TMPDIR"] = str(scratch_dir)
    _write_json_new(source_manifest_path, _local_source_manifest(registration, pgn))
    _, build_metrics = run_measured_opening_explorer(opening_explorer, [
        "build", "--manifest", str(source_manifest_path), "--collection", COLLECTION,
        "--gigabase-source-policy", str(POLICY_PATH), "--output", str(output_root),
        "--scratch-dir", str(scratch_dir), "--report-json", str(build_report_path),
    ], log_path=work_root / "build.log", environment=command_environment,
       sample_paths=[scratch_dir, output_root])
    resource_metrics["steps"]["build"] = build_metrics
    index_result, index_metrics = run_measured_opening_explorer(opening_explorer, [
        "build-index", "--dataset", str(output_root), "--collection", COLLECTION,
        "--depth-plies", str(args.depth_plies), "--memory-limit-mib", str(args.memory_limit_mib),
        "--json",
    ], log_path=work_root / "build-index.log", environment=command_environment,
       sample_paths=[scratch_dir, output_root])
    resource_metrics["steps"]["build_index"] = index_metrics
    index_report = read_json_output(index_result, "opening-explorer build-index")
    index_report_path.write_text(json.dumps(index_report, indent=2, sort_keys=True) + "\n")
    resource_metrics["index_memory_limit_mib"] = args.memory_limit_mib
    _write_json_new(resource_metrics_path, resource_metrics)
    run_opening_explorer(opening_explorer, ["verify", "--dataset", str(output_root), "--deep"])
    run_opening_explorer(opening_explorer, ["report", "--dataset", str(output_root), "--output", str(quality_report_path)])

    build_report = json.loads(build_report_path.read_text(encoding="utf-8"))
    if not build_report.get("is_reconciled") or not build_report.get("retained_matches_accepted"):
        raise EvaluationError("ingestion accounting did not reconcile")
    _validate_source_archive_metadata(
        build_report.get("source_archives"), registration, "build report"
    )
    accounting = build_report.get("accounting", {})
    if accounting.get("completion_status") != "CompleteEof":
        raise EvaluationError("the input did not reach clean EOF")
    snapshot = corpus.current_version_dir(output_root)
    manifest = json.loads((snapshot / "manifest.json").read_text(encoding="utf-8"))
    if snapshot.name != manifest.get("dataset_build_id"):
        raise EvaluationError("snapshot directory does not match the dataset build ID")
    _validate_source_archive_metadata(
        manifest.get("source_archives"), registration, "dataset manifest"
    )

    release_dir = snapshot / "RELEASE"
    release_dir.mkdir(exist_ok=False)
    for source, destination in (
        (build_report_path, release_dir / "build_report.json"),
        (index_report_path, release_dir / "index_report.json"),
        (quality_report_path, release_dir / "quality_report.json"),
        (resource_metrics_path, release_dir / "resource_metrics.json"),
        (POLICY_PATH, release_dir / "gigabase_source_qualification.json"),
    ):
        shutil.copyfile(source, destination)
    registration_summary = _registration_summary(registration)
    (release_dir / "local_source_registration.json").write_text(
        json.dumps(registration_summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (release_dir / "DATASET_LICENSE.txt").write_text(
        _license_notice(registration), encoding="utf-8"
    )

    source_summary = _source_attribution_counts(snapshot, manifest)
    release = {
        "release_schema_version": 1,
        "evaluation_schema_version": 1,
        "release_status": "local_evaluation_only",
        "release_eligible": False,
        "public_release_eligible": False,
        "collection": COLLECTION,
        "dataset_version": manifest["dataset_build_id"],
        "accepted_games": manifest["total_games"],
        "built_at": manifest["build_timestamp"],
        "format_versions": {"shard": "CGB3/v1", "position_index": "CPI/v2"},
        "index": {
            "depth_plies": manifest.get("position_index", {}).get("indexed_depth_plies"),
            "unique_positions": manifest.get("position_index", {}).get("unique_positions"),
            "unique_position_move_pairs": manifest.get("position_index", {}).get(
                "unique_position_move_pairs"
            ),
            "cpi_sha256": manifest.get("position_index", {}).get("sha256"),
            "cpi_file_size_bytes": manifest.get("position_index", {}).get("file_size_bytes"),
        },
        "source": registration_summary,
        "archive_integrity_evidence": {
            "full_decompression_check": registration["full_decompression_check"],
            "verification_tool_version": registration["local_extraction_tool"],
            "packed_stream_crc_metadata_available": registration[
                "packed_stream_crc_metadata_available"
            ],
            "publisher_checksum": None,
        },
        "qualification": {
            "policy_id": build_report.get("gigabase_source_policy", {}).get("policy_id"),
            "policy_sha256": _sha256(POLICY_PATH),
            "policy_file": "RELEASE/gigabase_source_qualification.json",
            "rights_clearance": "unresolved_upstream_rights",
            **source_summary,
        },
        "accounting": accounting,
        "license": {
            "publisher_claim": PUBLISHER_LICENSE_CLAIM,
            "source_level_rights": "unresolved",
            "notice_file": "RELEASE/DATASET_LICENSE.txt",
            "publisher_checksum": None,
            "local_archive_sha256": registration["archive_sha256_local"],
            "local_member_sha256": registration["member_sha256_local"],
        },
        "artifact_bytes": manifest.get("total_physical_bytes"),
        "software_license": "MIT OR Apache-2.0 (software only)",
    }
    release_path = release_dir / "RELEASE.json"
    release["artifacts"] = _release_artifacts(snapshot)
    release_path.write_text(json.dumps(release, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    problem = corpus.verify_release_files(output_root)
    if problem:
        raise EvaluationError(f"evaluation artifacts failed checksum verification: {problem}")
    run_opening_explorer(opening_explorer, ["verify", "--dataset", str(output_root), "--deep"])
    query = read_json_output(
        run_opening_explorer(opening_explorer, ["query", "--dataset", str(output_root), "--moves", "e4", "--json"]),
        "opening-explorer query",
    )
    if not isinstance(query.get("total_games"), int):
        raise EvaluationError("evaluation snapshot query returned no statistics")
    return release


def verify_evaluation_release(root: Path, opening_explorer: Path) -> dict[str, Any]:
    problem = corpus.verify_release_files(root)
    if problem:
        raise EvaluationError(f"release artifact verification failed: {problem}")
    snapshot = corpus.current_version_dir(root)
    release_path = snapshot / "RELEASE" / "RELEASE.json"
    release = json.loads(release_path.read_text(encoding="utf-8"))
    if release.get("release_schema_version") != 1 or release.get("evaluation_schema_version") != 1:
        raise EvaluationError("package is not a supported local-evaluation package")
    if (release.get("release_status") != "local_evaluation_only"
            or release.get("release_eligible") is not False
            or release.get("public_release_eligible") is not False):
        raise EvaluationError("package local-only release status is missing or was changed")
    if release.get("source", {}).get("publisher_checksum") is not None:
        raise EvaluationError("package improperly claims a publisher checksum")
    if release.get("license", {}).get("source_level_rights") != "unresolved":
        raise EvaluationError("package no longer records unresolved upstream rights")
    policy = snapshot / "RELEASE" / "gigabase_source_qualification.json"
    if _sha256(policy) != release.get("qualification", {}).get("policy_sha256"):
        raise EvaluationError("package qualification policy checksum mismatch")
    if not (snapshot / "RELEASE" / "DATASET_LICENSE.txt").is_file():
        raise EvaluationError("package is missing its local-evaluation license notice")
    run_opening_explorer(opening_explorer, ["verify", "--dataset", str(root), "--deep"])
    query = read_json_output(
        run_opening_explorer(opening_explorer, ["query", "--dataset", str(root), "--moves", "e4", "--json"]),
        "opening-explorer query",
    )
    if not isinstance(query.get("total_games"), int) or query["total_games"] <= 0:
        raise EvaluationError("package offline query returned no opening statistics")
    return release


def _add_tar_member(tf: tarfile.TarFile, path: Path, arcname: str) -> None:
    info = tf.gettarinfo(str(path), arcname=arcname)
    if info.islnk():
        # Python's tarfile turns repeated inode paths into hard links, while the safe
        # release extractor intentionally accepts only regular files and symlinks.
        info.type = tarfile.REGTYPE
        info.linkname = ""
        info.size = path.stat().st_size
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = 0
    info.mode = 0o755 if (info.isdir() or info.issym()) else 0o644
    if info.isreg():
        with path.open("rb") as source:
            tf.addfile(info, source)
    else:
        tf.addfile(info)


def package_evaluation(
    root: Path,
    output_dir: Path,
    opening_explorer: Path,
    registration_path: Path | None = None,
) -> Path:
    root = _work_path(root, "candidate dataset root", must_exist=True)
    output_dir = _work_path(output_dir, "package output directory")
    snapshot = corpus.current_version_dir(root)
    release_path = snapshot / "RELEASE" / "RELEASE.json"
    try:
        release_metadata = json.loads(release_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise EvaluationError(f"evaluation release metadata is invalid: {error}") from error
    if (release_metadata.get("release_status") != "local_evaluation_only"
            or release_metadata.get("release_eligible") is not False
            or release_metadata.get("public_release_eligible") is not False):
        raise EvaluationError("refusing to package a release without local-only status")
    if registration_path is not None:
        registration, _ = load_registration(registration_path)
        registration_summary = _registration_summary(registration)
        release_dir = snapshot / "RELEASE"
        (release_dir / "local_source_registration.json").write_text(
            json.dumps(registration_summary, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        (release_dir / "DATASET_LICENSE.txt").write_text(
            _license_notice(registration), encoding="utf-8"
        )
        release_metadata["source"] = registration_summary
        release_metadata["archive_integrity_evidence"] = {
            "full_decompression_check": registration["full_decompression_check"],
            "verification_tool_version": registration["local_extraction_tool"],
            "packed_stream_crc_metadata_available": registration[
                "packed_stream_crc_metadata_available"
            ],
            "publisher_checksum": None,
        }
        release_metadata["license"].update({
            "publisher_claim": PUBLISHER_LICENSE_CLAIM,
            "source_level_rights": "unresolved",
            "publisher_checksum": None,
            "local_archive_sha256": registration["archive_sha256_local"],
            "local_member_sha256": registration["member_sha256_local"],
        })
    manifest = json.loads((snapshot / "manifest.json").read_text(encoding="utf-8"))
    release_metadata.setdefault("qualification", {}).update(
        _source_attribution_counts(snapshot, manifest)
    )
    # Internal locks are intentionally omitted from packages. Refresh the artifact list
    # after the build lock has been created so it covers only distributable snapshot files.
    release_metadata["artifacts"] = _release_artifacts(snapshot)
    release_path.write_text(
        json.dumps(release_metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    release = verify_evaluation_release(root, opening_explorer)
    output_dir.mkdir(parents=True, exist_ok=True)
    archive = output_dir / f"{COLLECTION}-{release['dataset_version']}-local-evaluation.tar"
    if archive.exists() or archive.is_symlink():
        raise EvaluationError(f"refusing to overwrite package {archive}")
    checksum_path = archive.with_name(archive.name + ".sha256")
    if checksum_path.exists() or checksum_path.is_symlink():
        raise EvaluationError(f"refusing to overwrite package checksum {checksum_path}")
    temporary = archive.with_suffix(archive.suffix + ".part")
    try:
        with tarfile.open(temporary, "w", format=tarfile.PAX_FORMAT) as tf:
            current = root / "current"
            if not current.is_symlink():
                raise EvaluationError(
                    "candidate package root is missing its current snapshot symlink"
                )
            members: list[tuple[Path, str]] = [(current, f"{COLLECTION}/current")]
            if corpus.current_version_dir(root).parent != (root / "versions").resolve():
                raise EvaluationError("current snapshot is outside the immutable versions directory")
            for path in sorted(snapshot.rglob("*")):
                if path.is_symlink():
                    raise EvaluationError(f"snapshot contains an unsupported symlink: {path}")
                if path.name.startswith(".") or path.name.endswith(".lock"):
                    continue
                members.append((path, f"{COLLECTION}/{path.relative_to(root)}"))
            for path, name in sorted(members, key=lambda item: item[1]):
                _add_tar_member(tf, path, name)
        os.replace(temporary, archive)
    except Exception:
        temporary.unlink(missing_ok=True)
        raise
    checksum = _sha256(archive)
    checksum_path.write_text(f"{checksum}  {archive.name}\n", encoding="utf-8")
    try:
        verify_evaluation_package(archive, opening_explorer)
    except Exception:
        archive.unlink(missing_ok=True)
        checksum_path.unlink(missing_ok=True)
        raise
    return archive


def verify_evaluation_package(archive: Path, opening_explorer: Path) -> dict[str, Any]:
    archive = _regular_file(archive, "evaluation package")
    with tempfile.TemporaryDirectory(prefix="gigabase-package-verify-", dir=archive.parent) as name:
        destination = Path(name)
        root = corpus.extract_release_archive(archive, destination, COLLECTION)
        release = verify_evaluation_release(root, opening_explorer)
        with tarfile.open(archive, "r") as tf:
            for member in tf.getmembers():
                lower = member.name.lower()
                if lower.endswith((".pgn", ".pgn.zst", ".7z")):
                    raise EvaluationError(f"package includes raw source data: {member.name}")
        if release.get("dataset_version") != corpus.current_version_dir(root).name:
            raise EvaluationError("package version does not match its current snapshot")
        return release


def _publish(opening_explorer: Path, candidate: Path, target: Path) -> str:
    result = run_opening_explorer(opening_explorer, [
        "publish", "--candidate", str(candidate), "--target", str(target),
        "--collection", COLLECTION,
    ])
    del result
    current = corpus.current_version_dir(target)
    run_opening_explorer(opening_explorer, ["verify", "--dataset", str(target), "--deep"])
    return current.name


def bootstrap_package(
    archive: Path, target: Path, opening_explorer: Path, previous_root: Path | None = None
) -> dict[str, Any]:
    target = _work_path(target, "deployment root")
    archive = _regular_file(archive, "evaluation package")
    if target.exists() and target.is_symlink():
        raise EvaluationError("deployment root must not be a symlink")
    target.parent.mkdir(parents=True, exist_ok=True)
    staging_parent = Path(tempfile.mkdtemp(prefix="bootstrap-", dir=DEFAULT_WORK_ROOT))
    try:
        if previous_root is not None and not (target / "current").exists():
            previous_root = Path(previous_root).resolve()
            previous_snapshot = corpus.current_version_dir(previous_root)
            seed = staging_parent / previous_snapshot.name
            shutil.copytree(previous_snapshot, seed, symlinks=True)
            _publish(opening_explorer, seed, target)

        extracted = staging_parent / "package"
        extracted.mkdir()
        root = corpus.extract_release_archive(archive, extracted, COLLECTION)
        release = verify_evaluation_release(root, opening_explorer)
        active = _publish(opening_explorer, root, target)
        if active != release["dataset_version"]:
            raise EvaluationError("bootstrap published an unexpected dataset version")
        query = read_json_output(
            run_opening_explorer(opening_explorer, ["query", "--dataset", str(target), "--moves", "e4", "--json"]),
            "offline bootstrap query",
        )
        return {
            "target": str(target),
            "active_version": active,
            "accepted_games": release["accepted_games"],
            "query_total_games": query["total_games"],
            "previous_versions": sorted(
                path.name for path in (target / "versions").iterdir() if path.is_dir()
            ),
        }
    finally:
        shutil.rmtree(staging_parent, ignore_errors=True)


def rollback(target: Path, version: str, opening_explorer: Path) -> dict[str, Any]:
    target = _work_path(target, "deployment root", must_exist=True)
    if not version or version.startswith(".") or Path(version).name != version:
        raise EvaluationError("rollback version must be a safe immutable version directory name")
    snapshot = target / "versions" / version
    if not snapshot.is_dir() or snapshot.is_symlink():
        raise EvaluationError(f"rollback version does not exist: {snapshot}")
    expected = json.loads((snapshot / "manifest.json").read_text(encoding="utf-8"))
    if expected.get("dataset_build_id") != version:
        raise EvaluationError("rollback snapshot directory does not match its manifest ID")
    staging = Path(tempfile.mkdtemp(prefix="rollback-", dir=DEFAULT_WORK_ROOT))
    try:
        candidate = staging / version
        shutil.copytree(snapshot, candidate, symlinks=True)
        active = _publish(opening_explorer, candidate, target)
        if active != version:
            raise EvaluationError("rollback did not activate the requested immutable version")
        return {
            "target": str(target),
            "active_version": active,
            "accepted_games": expected["total_games"],
        }
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def _add_shared_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--opening-explorer", help="opening-explorer executable (default: target/release/opening-explorer)")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    register = subparsers.add_parser("register", help="verify and register local source hashes")
    register.add_argument("--archive", required=True, type=Path)
    register.add_argument("--pgn", required=True, type=Path)
    register.add_argument("--local-record", required=True, type=Path)
    register.add_argument("--output", type=Path, default=DEFAULT_WORK_ROOT / "source-registration.json")
    register.set_defaults(function=lambda args: register_source(
        args.archive, args.pgn, args.local_record, args.output
    ))

    build = subparsers.add_parser("build", help="build an isolated local evaluation snapshot")
    build.add_argument("--registration", type=Path, default=DEFAULT_WORK_ROOT / "source-registration.json")
    build.add_argument("--work-root", type=Path, default=DEFAULT_WORK_ROOT)
    build.add_argument("--output-root", type=Path, default=DEFAULT_WORK_ROOT / "candidate")
    build.add_argument("--depth-plies", type=int, default=40)
    build.add_argument("--memory-limit-mib", type=int, default=128)
    _add_shared_args(build)
    build.set_defaults(function=lambda args: build_evaluation(args))

    package = subparsers.add_parser("package", help="create a local-only verified package")
    package.add_argument("--root", type=Path, default=DEFAULT_WORK_ROOT / "candidate")
    package.add_argument("--output-dir", type=Path, default=DEFAULT_WORK_ROOT / "packages")
    package.add_argument("--registration", type=Path,
                         help="refresh source evidence and license notice from a registration")
    _add_shared_args(package)
    package.set_defaults(function=lambda args: package_evaluation(
        args.root, args.output_dir, find_opening_explorer(args.opening_explorer), args.registration
    ))

    verify = subparsers.add_parser("verify-package", help="extract, verify, and query a package")
    verify.add_argument("--package", required=True, type=Path)
    _add_shared_args(verify)
    verify.set_defaults(function=lambda args: verify_evaluation_package(
        args.package, find_opening_explorer(args.opening_explorer)
    ))

    bootstrap = subparsers.add_parser("bootstrap", help="install into an isolated local root")
    bootstrap.add_argument("--package", required=True, type=Path)
    bootstrap.add_argument("--target", type=Path, default=DEFAULT_WORK_ROOT / "deploy" / COLLECTION)
    bootstrap.add_argument("--previous-root", type=Path,
                           help="optional old Masters root to seed for rollback testing")
    _add_shared_args(bootstrap)
    bootstrap.set_defaults(function=lambda args: bootstrap_package(
        args.package, args.target, find_opening_explorer(args.opening_explorer), args.previous_root
    ))

    rollback_cmd = subparsers.add_parser("rollback", help="atomically restore an immutable version")
    rollback_cmd.add_argument("--target", type=Path, default=DEFAULT_WORK_ROOT / "deploy" / COLLECTION)
    rollback_cmd.add_argument("--version", required=True, help="immutable version directory ID")
    _add_shared_args(rollback_cmd)
    rollback_cmd.set_defaults(function=lambda args: rollback(
        args.target, args.version, find_opening_explorer(args.opening_explorer)
    ))
    return parser


def main(argv: list[str] | None = None) -> int:
    try:
        args = build_parser().parse_args(argv)
        result = args.function(args)
        if isinstance(result, Path):
            print(result)
        else:
            print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (EvaluationError, corpus.CorpusError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
