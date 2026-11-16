#!/usr/bin/env python3
"""Validate current real release, provenance, HTTP, and Docker evidence offline."""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EVIDENCE = ROOT / "benchmarks" / "corpus"
CATALOG = ROOT / "corpus" / "catalog.json"
SHA = re.compile(r"^[0-9a-f]{64}$")
COLLECTIONS = ("masters-otb", "elite-online")
CLASSES = ("verified_otb", "verified_online", "unresolved")
COUNTERS = (
    "accepted_unique", "confirmed_duplicates", "superseded_prior", "rejected_by_policy",
    "rejected_variant", "rejected_illegal", "rejected_malformed", "quarantined",
)


def fail(message: str) -> None:
    print(f"  [FAIL] {message}")
    raise SystemExit(1)


def load(path: Path):
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"{path.relative_to(ROOT)}: {error}")


def canonical_sha256(value: object) -> str:
    raw = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(raw).hexdigest()


def validate_release(collection: str, catalog: dict) -> dict:
    directory = EVIDENCE / collection
    if not directory.is_dir():
        fail(f"missing evidence directory {directory.relative_to(ROOT)}")
    release = load(directory / "RELEASE.json")
    build = load(directory / "build_report.json")
    quality = load(directory / "quality_report.json")
    metrics = load(directory / "metrics.json")

    if (release.get("collection") != collection or build.get("collection") != collection
            or quality.get("collection") != collection):
        fail(f"{collection}: collection identity differs across evidence")
    if release.get("release_schema_version") != 1:
        fail(f"{collection}: unsupported release schema")
    accounting = release.get("accounting", {})
    if accounting.get("total_encountered") != sum(accounting.get(key, 0) for key in COUNTERS):
        fail(f"{collection}: accounting identity does not reconcile")
    if accounting != {key: build.get("accounting", {}).get(key) for key in accounting}:
        fail(f"{collection}: RELEASE and build report accounting differ")
    if (release.get("accepted_games") != accounting.get("accepted_unique")
            or quality.get("total_games") != accounting.get("accepted_unique")):
        fail(f"{collection}: accepted-game counts differ")
    if not (build.get("is_reconciled") and build.get("retained_matches_accepted")):
        fail(f"{collection}: build report is not reconciled")
    if sum(build.get("policy_rejection_reasons", {}).values()) != accounting.get("rejected_by_policy"):
        fail(f"{collection}: policy rejection reasons do not reconcile")

    artifacts = release.get("artifacts", {})
    if not isinstance(artifacts, dict) or not artifacts:
        fail(f"{collection}: release artifact inventory is missing")
    for name, metadata in artifacts.items():
        if (not SHA.fullmatch(metadata.get("sha256", ""))
                or not isinstance(metadata.get("size_bytes"), int)
                or metadata["size_bytes"] <= 0):
            fail(f"{collection}: malformed release artifact record {name}")
    index = release.get("index", {})
    if not SHA.fullmatch(index.get("cpi_sha256", "")) or index.get("depth_plies", 0) <= 0:
        fail(f"{collection}: invalid CPI provenance")
    if not release.get("inputs"):
        fail(f"{collection}: no pinned source inputs")
    sources = {source["id"]: source for source in catalog["sources"]}
    for item in release["inputs"]:
        source = sources.get(item.get("id"))
        if (source is None or source.get("collection") != collection
                or source.get("publisher_sha256") != item.get("sha256")
                or source.get("verified_sha256") != item.get("sha256")
                or source.get("license") != item.get("license")):
            fail(f"{collection}: source {item.get('id')} differs from current pinned catalog")
    license_record = release.get("license", {})
    expected_license = "CC-BY-SA-4.0" if collection == "masters-otb" else "CC0-1.0"
    if (license_record.get("id") != expected_license
            or license_record.get("share_alike") != (collection == "masters-otb")):
        fail(f"{collection}: license record is inconsistent")
    if set(metrics.get("steps", {})) != {"build", "build-index", "verify", "report"}:
        fail(f"{collection}: release metrics are incomplete")
    if any(step.get("returncode") != 0 for step in metrics["steps"].values()):
        fail(f"{collection}: a release build step failed")

    lookup_path = directory / "lookup_benchmark.json"
    if lookup_path.exists():
        lookup = load(lookup_path)
        if (lookup.get("collection") != collection
                or lookup.get("dataset_build_id") != release.get("dataset_version")
                or lookup.get("unique_positions") != index.get("unique_positions")):
            fail(f"{collection}: lookup benchmark is stale for the current release")

    print(
        f"  [PASS] {collection}: {release['accepted_games']} games, "
        f"{len(release['inputs'])} verified source archives, accounting reconciled"
    )
    return release


def validate_masters_audit(release: dict, build: dict) -> None:
    qualification = release.get("masters_event_qualification")
    if (not isinstance(qualification, dict)
            or qualification.get("status") != "verified_otb_subset"
            or qualification.get("audit_file") != "RELEASE/MASTERS_EVENT_QUALIFICATION.json"):
        fail("Masters release does not identify itself as an audited OTB subset")

    audit_path = EVIDENCE / "masters-otb" / "masters_event_qualification.json"
    audit = load(audit_path)
    if audit.get("schema_version") != 1:
        fail("Masters event audit schema is unsupported")
    if qualification.get("audit_sha256") != hashlib.sha256(audit_path.read_bytes()).hexdigest():
        fail("Masters event audit hash does not match release provenance")
    source = audit.get("input", {})
    if (not source.get("source_dataset_version") or not SHA.fullmatch(source.get("sha256", ""))
            or not isinstance(source.get("games"), int)):
        fail("Masters event audit input release provenance is incomplete")
    method = audit.get("method", {})
    if (method.get("metadata_source") != "Lichess Broadcast API tournament metadata plus source PGN event, site, and broadcast URL tags"
            or not method.get("external_sources")):
        fail("Masters event audit does not identify its evidence sources")

    totals = audit.get("totals", {})
    events = audit.get("events")
    if set(totals) != set(CLASSES) or not isinstance(events, list):
        fail("Masters event audit must report verified OTB, verified online, and unresolved events")
    computed = {name: {"games": 0, "events": 0} for name in CLASSES}
    for event in events:
        classification = event.get("classification")
        count = event.get("game_count")
        if classification not in computed or not isinstance(count, int) or count < 0:
            fail("Masters event audit contains an invalid class or game count")
        computed[classification]["games"] += count
        computed[classification]["events"] += 1
        if classification in ("verified_otb", "verified_online"):
            metadata = event.get("metadata")
            metadata_hash = event.get("metadata_sha256")
            if (not isinstance(metadata, dict) or not SHA.fullmatch(metadata_hash or "")
                    or canonical_sha256(metadata) != metadata_hash
                    or not str(event.get("metadata_url", "")).startswith(
                        "https://lichess.org/api/broadcast/"
                    )
                    or not event.get("evidence")):
                fail("Classified Masters event lacks hashed official metadata evidence")
        if classification == "verified_otb":
            info = event.get("metadata", {}).get("tour", {}).get("info", {})
            linked = event.get("linked_event_evidence")
            external = event.get("external_event_evidence")
            if (not isinstance(info, dict) or not str(info.get("location", "")).strip()
                    or not isinstance(external, dict) or external.get("verified") is not True
                    or not SHA.fullmatch(external.get("sha256", ""))
                    or not isinstance(linked, list)
                    or not any(
                        isinstance(page, dict) and page.get("verified") is True
                        and page.get("sha256") == external.get("sha256")
                        for page in linked
                    )):
                fail("Verified OTB event lacks corroborated venue and linked-source evidence")
    if computed != totals or sum(value["games"] for value in computed.values()) != source["games"]:
        fail("Masters event audit totals do not reconcile to its input PGN")
    if audit.get("release_policy", {}).get("require_verified_otb_event") is not True:
        fail("Masters release policy does not require verified OTB events")

    policy = audit["release_policy"]
    expected_online = sorted(
        event["tournament_slug"] for event in events
        if event["classification"] == "verified_online" and event.get("tournament_slug")
    )
    expected_otb = sorted(
        event["tournament_slug"] for event in events
        if event["classification"] == "verified_otb" and event.get("tournament_slug")
    )
    if (sorted(policy.get("verified_online_tournament_slugs", [])) != expected_online
            or sorted(policy.get("verified_otb_tournament_slugs", [])) != expected_otb
            or not expected_otb or set(expected_online) & set(expected_otb)):
        fail("Masters event audit allowlists do not match its event classifications")
    if qualification.get("baseline_audit_game_counts") != totals:
        fail("Masters release event totals differ from the attached audit")
    reasons = build.get("policy_rejection_reasons", {})
    expected_rejections = {
        "verified_online_event": reasons.get("online_event_in_masters_source", 0),
        "unresolved_masters_provenance": reasons.get("unresolved_masters_provenance", 0),
    }
    if qualification.get("release_rejections") != expected_rejections:
        fail("Masters release rejection totals differ from the build report")
    if release.get("accepted_games", 0) > totals["verified_otb"]["games"]:
        fail("Masters release contains more games than its verified OTB source subset")

    print("  [PASS] Masters event audit: "
          + "; ".join(f"{name}={totals[name]['games']} games/{totals[name]['events']} events"
                      for name in CLASSES))


def validate_docker(report: dict, releases: dict[str, dict]) -> None:
    if (report.get("schema_version") != 1 or report.get("mode") != "real"
            or report.get("service") != "codb" or report.get("api_address") != "http://codb:8080"):
        fail("Docker evidence is not a real-corpus Compose run")
    if "@sha256:" not in report.get("independent_client_image", ""):
        fail("Docker evidence does not identify a digest-pinned independent client image")
    if not report.get("network"):
        fail("Docker evidence is missing its container network")

    if report.get("http_checks") != {
        "health_status": 200,
        "ready_status": 200,
        "collections_status": 200,
        "invalid_position_status": 400,
        "unknown_game_status": 404,
        "admin_reload_status": 403,
    }:
        fail("Docker evidence is missing expected API status checks")

    queries = report.get("collections", {})
    if set(queries) != set(COLLECTIONS):
        fail("Docker evidence does not include both real collections")
    for collection in COLLECTIONS:
        query = queries[collection]
        release = releases[collection]
        if (query.get("dataset_version") != release.get("dataset_version")
                or query.get("dataset_total_games") != release.get("accepted_games")
                or query.get("cli_match") is not True):
            fail(f"Docker {collection} response does not match its release or CLI")
        if (not isinstance(query.get("expected_total_games_at_e4_e5_nf3"), int)
                or query.get("total_games_at_e4_e5_nf3")
                != query.get("expected_total_games_at_e4_e5_nf3")):
            fail(f"Docker {collection} opening-position count differs from its recorded expectation")
        if (query.get("stored_game_http_status") != 200
                or query.get("stored_game_bytes", 0) <= 0
                or not SHA.fullmatch(query.get("stored_game_sha256", ""))
                or query.get("stored_game_is_pgn") is not True):
            fail(f"Docker {collection} stored-PGN retrieval evidence is invalid")

    concurrent = report.get("concurrent_requests", {})
    if (concurrent.get("count") != 96 or concurrent.get("successful") != 96
            or concurrent.get("concurrent_workers") != 12):
        fail("Docker concurrent position and PGN requests did not all succeed")
    rss = report.get("runtime_rss", {})
    if (rss.get("sampled_peak_bytes", 0) <= 0
            or rss.get("sampled_peak_bytes") != rss.get("sampled_peak_kib", 0) * 1024
            or rss.get("sampled_peak_bytes") >= 512 * 1024 * 1024):
        fail("Docker real-corpus RSS evidence is missing or exceeds the 512 MiB runtime cap")
    settings = report.get("container_settings", {})
    if (settings.get("root_filesystem_read_only") is not True
            or settings.get("configured_user") != "10001:10001"
            or settings.get("dataset_mount_mode") not in {
                "read_only_host_bind", "read_only_named_volume_copy"
            }
            or settings.get("masters_mount_read_only") is not True
            or settings.get("elite_mount_read_only") is not True
            or settings.get("no_host_ports_published") is not True
            or settings.get("image_size_bytes", 0) <= 0):
        fail("Docker security, mount, image, or host-exposure checks failed")
    if report.get("bad_dataset_starts_rejected") != [
        "missing collection mount", "wrong collection path", "incomplete dataset", "corrupted CPI"
    ]:
        fail("Docker failure checks did not reject all invalid dataset setups")
    restart = report.get("container_restart", {})
    if (restart.get("healthy_after_restart") is not True
            or restart.get("api_results_match_before_restart") is not True
            or restart.get("deep_dataset_verification_after_restart") is not True):
        fail("Docker restart and mounted-dataset verification evidence is incomplete")
    if report.get("graceful_sigterm_shutdown") is not True:
        fail("Docker SIGTERM shutdown did not complete gracefully")
    print(
        f"  [PASS] real Docker integration: {concurrent['successful']}/{concurrent['count']} "
        f"concurrent requests; sampled RSS {rss['sampled_peak_kib']} KiB"
    )


def validate_gigabase_policy_impact() -> None:
    directory = ROOT / "benchmarks" / "gigabase"
    inventory = load(directory / "INVENTORY.json")
    impact = load(directory / "POLICY_IMPACT.json")
    input_record = inventory.get("input", {})
    parser = inventory.get("parser", {})
    if (impact.get("schema_version") != 1
            or impact.get("policy_id") != "gigabase-otb/2026-11-r3"
            or impact.get("method") != "full_streaming_record_header_scan_using_the_production_r3_header_policy"):
        fail("Gigabase policy-impact report has an unsupported schema or method")
    if (impact.get("input_size_bytes") != input_record.get("size_bytes")
            or impact.get("input_sha256") != input_record.get("sha256")
            or impact.get("records_encountered") != parser.get("records_encountered")
            or impact.get("structurally_parsed") != parser.get("records_structurally_parsed")
            or impact.get("recoverable_syntax_errors")
            != parser.get("records_with_recoverable_syntax_errors")):
        fail("Gigabase policy-impact input or parser accounting differs from the inventory")
    if (impact.get("header_candidate_accounting_reconciles") is not True
            or impact.get("records_encountered") != impact.get("structurally_parsed")
            or impact.get("recoverable_syntax_errors") != 0):
        fail("Gigabase policy-impact source scan did not complete and reconcile")

    all_strength = impact.get("valid_strength_rule_counts_non_overlapping", {})
    current_strength = impact.get("r3_header_candidates_by_strength_rule", {})
    source_counts = impact.get("r3_header_candidates_by_source_tag", {})
    candidate_count = impact.get("r3_header_candidate_count")
    if (not isinstance(candidate_count, int) or candidate_count <= 0
            or sum(all_strength.values()) != impact.get("strength_eligible_header_records")
            or sum(current_strength.values()) != candidate_count
            or sum(source_counts.values()) != candidate_count
            or candidate_count > impact.get("records_encountered", 0)):
        fail("Gigabase r3 candidate and strength-rule counts do not reconcile")
    if (impact.get("r3_candidates_admitted_by_both_source_and_identity", 0)
            > min(impact.get("r3_candidates_admitted_by_removing_r2_source_allowlist", 0),
                 impact.get("r3_candidates_admitted_by_relaxing_identity_confidence", 0))):
        fail("Gigabase source and identity overlap count exceeds either component")
    print(
        f"  [PASS] Gigabase r3 full-source header analysis: {candidate_count} candidates; "
        "source hash and record counts match the structural inventory"
    )


def main() -> int:
    catalog = load(CATALOG)
    releases = {
        collection: validate_release(collection, catalog)
        for collection in COLLECTIONS
    }

    http = load(EVIDENCE / "http_check.json")
    if (http.get("passed") is not True or http.get("checks_failed") != 0
            or http.get("checks_total", 0) < 50):
        fail("real HTTP/CLI check does not record a passing run")
    for collection in COLLECTIONS:
        if http.get("collections", {}).get(collection, {}).get("mismatches") != 0:
            fail(f"{collection}: real HTTP/CLI comparisons contain mismatches")

    validate_masters_audit(releases["masters-otb"],
                           load(EVIDENCE / "masters-otb" / "build_report.json"))
    validate_docker(load(EVIDENCE / "docker_real_check.json"), releases)
    validate_gigabase_policy_impact()

    reproducibility = load(EVIDENCE / "reproducibility_check.json")
    for collection in COLLECTIONS:
        release = releases[collection]
        check = reproducibility.get("collections", {}).get(collection)
        if (not isinstance(check, dict) or check.get("collection") != collection
                or check.get("reference_release_id") != release.get("dataset_version")
                or not check.get("same_binary_artifacts")
                or not check.get("accounting_reconciled")
                or check.get("accepted_games") != release.get("accepted_games")):
            fail(f"{collection}: missing current independent rebuild evidence")
        reference = check.get("reference_sha256", {})
        repeat = check.get("repeat_sha256", {})
        if not reference or reference != repeat:
            fail(f"{collection}: independent rebuild artifact hashes differ")
        for name, digest in reference.items():
            if (not SHA.fullmatch(digest)
                    or release.get("artifacts", {}).get(name, {}).get("sha256") != digest):
                fail(f"{collection}: reproducibility hash does not match release artifact {name}")
        if any(value <= 0 for value in check.get("wall_seconds", {}).values()):
            fail(f"{collection}: independent rebuild timing is invalid")
        print(f"  [PASS] {collection}: independent build artifacts match")

    return 0


if __name__ == "__main__":
    sys.exit(main())
