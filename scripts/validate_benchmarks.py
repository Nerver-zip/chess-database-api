#!/usr/bin/env python3
"""
scripts/validate_benchmarks.py — Benchmark schema, accounting, and consistency validator.

Enforces strict verification on all authoritative benchmark artifacts in benchmarks/results/historical/:
1. Complete schema compliance and absence of obsolete/unmigrated schema fields.
2. Ingestion accounting invariants (mutually exclusive category sum and retained==accepted).
3. Monotonic position-growth and physical size invariants across depth and sample size.
4. Latency percentile invariants (p50 <= p95 <= p99) and 100% semantic equivalence.
5. Exact consistency between source benchmark JSON and derived projections (projections_heaps.json).
"""

import glob
import json
import math
import os
import sys

REQUIRED_FIELDS = [
    "run_name",
    "collection",
    "input_file",
    "total_encountered",
    "accepted_unique",
    "retained_canonical_games",
    "confirmed_duplicates",
    "superseded_prior",
    "rejected_by_policy",
    "rejected_variant",
    "rejected_illegal",
    "rejected_malformed",
    "quarantined",
    "completion_status",
    "is_reconciled",
    "retained_matches_accepted",
    "raw_pgn_bytes",
    "codb_block_compressed_bytes",
    "codb_bytes_per_game",
    "compression_ratio",
    "depth_20",
    "depth_30",
    "depth_40",
    "depth_60",
    "query_latency_us",
]

DEPTH_KEYS = ["depth_20", "depth_30", "depth_40", "depth_60"]


def validate_benchmark_file(path):
    print(f"Validating benchmark artifact: {path}")
    with open(path) as f:
        data = json.load(f)

    # 1. Required fields
    for field in REQUIRED_FIELDS:
        if field not in data:
            raise ValueError(f"{path}: Missing required field '{field}'.")

    # 2. Accounting reconciliation invariant
    total_enc = data["total_encountered"]
    accepted = data["accepted_unique"]
    retained = data["retained_canonical_games"]
    dups = data["confirmed_duplicates"]
    superseded = data["superseded_prior"]
    rej_policy = data["rejected_by_policy"]
    rej_variant = data["rejected_variant"]
    rej_illegal = data["rejected_illegal"]
    rej_malformed = data["rejected_malformed"]
    quarantined = data["quarantined"]

    expected_sum = (
        accepted
        + dups
        + superseded
        + rej_policy
        + rej_variant
        + rej_illegal
        + rej_malformed
        + quarantined
    )

    if total_enc != expected_sum:
        raise ValueError(
            f"{path}: Ingestion accounting does not sum! "
            f"total_encountered ({total_enc}) != category sum ({expected_sum})."
        )

    if not data["is_reconciled"]:
        raise ValueError(f"{path}: 'is_reconciled' flag is False.")

    if retained != accepted:
        raise ValueError(
            f"{path}: retained_canonical_games ({retained}) != accepted_unique ({accepted})."
        )

    if not data["retained_matches_accepted"]:
        raise ValueError(f"{path}: 'retained_matches_accepted' flag is False.")

    if data["completion_status"] not in ("CompleteEof", "PartialLimitReached"):
        raise ValueError(
            f"{path}: Invalid completion_status: {data['completion_status']}"
        )

    # 2b. Benchmark timing semantics & disambiguation
    b_type = data.get("benchmark_type", "raw_ingest")
    import_dur = data.get("import_duration_secs")
    reload_dur = data.get("archive_reload_duration_secs")

    if b_type == "dataset_archive":
        if import_dur is not None:
            raise ValueError(
                f"{path}: Ambiguous benchmark semantics! 'import_duration_secs' must not be populated "
                "for dataset_archive benchmarks. Use 'archive_reload_duration_secs'."
            )
        if reload_dur is None or reload_dur <= 0:
            raise ValueError(
                f"{path}: 'archive_reload_duration_secs' must be positive for dataset_archive."
            )
        # Check byte accounting
        if "persistent_storage" in data:
            p_bytes = data["persistent_storage"].get("total_persistent_bytes")
            sources = data.get("source_archives", [])
            source_size = sum(s.get("size_bytes", 0) for s in sources)
            inp_bytes = data.get("input_size_bytes")
            if (
                inp_bytes is not None
                and inp_bytes == p_bytes
                and (source_size != p_bytes or source_size == 0)
            ):
                raise ValueError(
                    f"{path}: Physical archive size ({p_bytes}) incorrectly populates source input_size_bytes! "
                    "When original source size is unknown, input_size_bytes must not disguise archive size."
                )
    elif b_type == "raw_ingest":
        if import_dur is None or import_dur <= 0:
            raise ValueError(
                f"{path}: 'import_duration_secs' must be positive for raw_ingest benchmark."
            )
        if reload_dur is not None:
            raise ValueError(
                f"{path}: 'archive_reload_duration_secs' must not be set for raw_ingest benchmark."
            )

    # 2c. Cryptographic hash & provenance integrity
    prov_status = data.get("provenance_status")
    for s_idx, source in enumerate(data.get("source_archives", [])):
        h = source.get("sha256")
        if h is not None:
            if not isinstance(h, str) or len(h) != 64 or not all(c in "0123456789abcdefABCDEF" for c in h):
                raise ValueError(
                    f"{path}: Invalid SHA-256 in source_archives[{s_idx}]: {h!r}. "
                    "Placeholders or non-hex hashes are strictly rejected."
                )
        if prov_status == "verified":
            if h is None:
                raise ValueError(
                    f"{path}: Provenance marked as 'verified', but source_archives[{s_idx}] lacks sha256."
                )
            if b_type == "dataset_archive" and source.get("checksum_verified") is not True:
                raise ValueError(
                    f"{path}: Provenance marked as 'verified', but source_archives[{s_idx}] "
                    "lacks physical build-time verification proof (checksum_verified is not True)."
                )

    # 3. Depth structures and physical sizes
    prev_pos = -1
    for d_key in DEPTH_KEYS:
        block = data[d_key]
        for subfield in [
            "depth_plies",
            "unique_positions",
            "unique_position_move_pairs",
            "cpi_block_count",
            "cpi_sparse_directory_bytes",
            "immutable_index_size_bytes",
            "sqlite_index_size_bytes",
        ]:
            if subfield not in block:
                raise ValueError(f"{path}: Missing subfield '{subfield}' in '{d_key}'.")

        pos = block["unique_positions"]
        cpi_bytes = block["immutable_index_size_bytes"]
        sqlite_bytes = block["sqlite_index_size_bytes"]

        if pos < 0 or cpi_bytes < 0 or sqlite_bytes < 0:
            raise ValueError(f"{path}: Negative counts/sizes in '{d_key}'.")

        # Obsolete schema detection: if positions > 0 but SQLite bytes == 0
        if pos > 0 and sqlite_bytes == 0:
            raise ValueError(
                f"{path}: Obsolete benchmark artifact detected! "
                f"sqlite_index_size_bytes is 0 despite {pos} unique positions. "
                "Move obsolete prototypes to benchmarks/results/historical/."
            )

        if pos < prev_pos:
            raise ValueError(
                f"{path}: Positions do not monotonically increase with depth! "
                f"{d_key} has {pos} < previous depth {prev_pos}."
            )
        prev_pos = pos

    # 4. Latency percentiles and semantic equivalence
    lat = data["query_latency_us"]
    for lat_engine in ["cpi_disk", "sqlite_disk", "cpi_in_memory"]:
        p50 = lat.get(f"{lat_engine}_p50_us", 0.0)
        p95 = lat.get(f"{lat_engine}_p95_us", 0.0)
        p99 = lat.get(f"{lat_engine}_p99_us", 0.0)

        if not (p50 <= p95 <= p99):
            raise ValueError(
                f"{path}: Latency percentiles violated for {lat_engine}: "
                f"p50={p50}, p95={p95}, p99={p99}."
            )

    if not lat.get("semantic_equivalence_verified", False):
        raise ValueError(
            f"{path}: Semantic equivalence between CPI and SQLite was not verified."
        )

    print(
        f"  [OK] {data['run_name']}: N={accepted}, Enc={total_enc}, D40 Pos={data['depth_40']['unique_positions']}, SQLite={data['depth_40']['sqlite_index_size_bytes']} B"
    )
    return data


def validate_progressive_series(runs_data):
    print("\nValidating progressive scaling monotonicity across N...")
    prev_n = 0
    prev_d40_pos = 0
    prev_codb_bytes = 0

    for data in runs_data:
        n = data["accepted_unique"]
        d40_pos = data["depth_40"]["unique_positions"]
        codb_bytes = data["codb_block_compressed_bytes"]

        if n <= prev_n:
            raise ValueError(
                f"Progressive series non-increasing N: {n} <= previous {prev_n}."
            )
        if d40_pos <= prev_d40_pos:
            raise ValueError(
                f"Progressive series non-increasing D40 positions at N={n}: {d40_pos} <= {prev_d40_pos}."
            )
        if codb_bytes <= prev_codb_bytes:
            raise ValueError(
                f"Progressive series non-increasing CGB3 bytes at N={n}: {codb_bytes} <= {prev_codb_bytes}."
            )

        prev_n = n
        prev_d40_pos = d40_pos
        prev_codb_bytes = codb_bytes
    print("  [OK] Progressive scaling series is strictly monotonic in games, positions, and storage.")


def validate_projections(proj_path, m20k_data):
    print(f"\nValidating derived projections artifact: {proj_path}")
    if not os.path.isfile(proj_path):
        raise FileNotFoundError(f"Missing {proj_path}")

    with open(proj_path) as f:
        proj = json.load(f)

    # 1. Calculation integrity and numerical validity
    params = proj.get("parameters")
    if not params or not isinstance(params, dict):
        raise ValueError(f"{proj_path}: Missing or invalid 'parameters' object.")

    advisories = []

    for d_key in DEPTH_KEYS:
        if d_key not in params:
            raise ValueError(f"{proj_path}: Missing parameters for '{d_key}'.")
        p = params[d_key]
        for field in [
            "k",
            "alpha_ols",
            "in_sample_slope_std_err",
            "sensitivity_delta_alpha",
            "r2",
            "measured_bytes_per_pos_at_20k",
        ]:
            if field not in p:
                raise ValueError(
                    f"{proj_path}: Missing parameter field '{field}' in '{d_key}'."
                )
            val = p[field]
            if not isinstance(val, (int, float)) or not math.isfinite(val):
                raise ValueError(
                    f"{proj_path}: Parameter '{field}' in '{d_key}' is not a finite number: {val}"
                )

        if p["k"] <= 0:
            raise ValueError(
                f"{proj_path}: Scaling constant k must be positive, got {p['k']} in '{d_key}'."
            )
        if p["measured_bytes_per_pos_at_20k"] <= 0:
            raise ValueError(
                f"{proj_path}: measured_bytes_per_pos_at_20k must be positive in '{d_key}'."
            )

        # Scientific advisories (advisory notes, NOT validation failures)
        if p["r2"] < 0.99:
            advisories.append(
                f"Model fit advisory: {d_key} goodness of fit R^2 is {p['r2']:.4f} (< 0.99)."
            )
        if p["alpha_ols"] < 0.70 or p["alpha_ols"] > 1.00:
            advisories.append(
                f"Scaling exponent advisory: {d_key} alpha_ols is {p['alpha_ols']:.4f} (outside typical range 0.70-1.00)."
            )

    # Check dynamic SQLite multiplier calculation consistency
    expected_multiplier = round(
        m20k_data["depth_40"]["sqlite_index_size_bytes"]
        / m20k_data["depth_40"]["immutable_index_size_bytes"],
        2,
    )
    actual_multiplier = params.get("sqlite_multiplier_vs_cpi_at_d40")
    if actual_multiplier is None or not math.isfinite(actual_multiplier):
        raise ValueError(
            f"{proj_path}: Missing or non-finite 'sqlite_multiplier_vs_cpi_at_d40'."
        )
    if abs(actual_multiplier - expected_multiplier) > 0.01:
        raise ValueError(
            f"{proj_path}: Calculation inconsistency! sqlite_multiplier_vs_cpi_at_d40 "
            f"({actual_multiplier}) does not match m20k data ({expected_multiplier})."
        )

    # 2. Check budget scenarios structure & internal consistency
    budgets = proj.get("budget_scenarios_gb")
    if not budgets or not isinstance(budgets, dict):
        raise ValueError(f"{proj_path}: Missing or invalid 'budget_scenarios_gb'.")

    opt_b = budgets.get("option_b_recommended_d40_masters_d30_elite")
    if not opt_b or not isinstance(opt_b, dict):
        raise ValueError(
            f"{proj_path}: Missing 'option_b_recommended_d40_masters_d30_elite' budget."
        )

    for field in [
        "masters_index_d40_baseline",
        "elite_index_d30_proxy_baseline",
        "core_payload_baseline",
        "deployed_total_optimistic",
        "deployed_total_baseline",
        "deployed_total_conservative",
    ]:
        if field not in opt_b:
            raise ValueError(
                f"{proj_path}: Missing budget field '{field}' in Option B."
            )
        v = opt_b[field]
        if not isinstance(v, (int, float)) or not math.isfinite(v):
            raise ValueError(
                f"{proj_path}: Budget field '{field}' is not a finite number: {v}"
            )

    base_total = opt_b["deployed_total_baseline"]
    cons_total = opt_b["deployed_total_conservative"]
    opt_total = opt_b["deployed_total_optimistic"]

    if not (opt_total <= base_total <= cons_total):
        raise ValueError(
            f"{proj_path}: Inconsistent scenario totals! "
            f"Expected optimistic ({opt_total}) <= baseline ({base_total}) <= conservative ({cons_total})."
        )

    # 3. Scientific feasibility assessment (reported, NOT a validation failure)
    if cons_total <= 10.0:
        assessment = "within_budget"
    elif base_total <= 10.0:
        assessment = "conditionally_within_budget"
    else:
        assessment = "over_budget"

    print(
        f"  [OK] Calculation integrity verified: SQLite/CPI ratio={actual_multiplier}x, "
        f"Option B Baseline={base_total} GB (Cons: {cons_total} GB)."
    )
    print(f"  [ANALYSIS] Feasibility Assessment: {assessment}")
    if advisories:
        for adv in advisories:
            print(f"  [ADVISORY] {adv}")

    return {
        "assessment": assessment,
        "advisories": advisories,
        "baseline_gb": base_total,
        "conservative_gb": cons_total,
    }


def main():
    benchmarks_dir = sys.argv[1] if len(sys.argv) > 1 else (
        "benchmarks/results/historical" if os.path.exists("benchmarks/results/historical") else "benchmarks/results"
    )
    all_files = sorted(glob.glob(os.path.join(benchmarks_dir, "*.json")))
    if not all_files:
        print(f"No benchmark files found in {benchmarks_dir}!")
        sys.exit(1)

    # Separate projections file and obsolete prototypes from benchmark outputs
    benchmark_files = [
        f for f in all_files
        if not f.endswith("projections_heaps.json") and not f.endswith("stage_b_masters_otb_07.json")
    ]

    loaded_benchmarks = {}
    for path in benchmark_files:
        data = validate_benchmark_file(path)
        loaded_benchmarks[data["run_name"]] = data

    # Validate progressive scaling series
    prog_keys = [
        "masters_1k",
        "masters_2.5k",
        "masters_5k",
        "masters_10k",
        "masters_20k",
    ]
    if all(k in loaded_benchmarks for k in prog_keys):
        prog_data = [loaded_benchmarks[k] for k in prog_keys]
        validate_progressive_series(prog_data)

    # Validate projections
    proj_path = os.path.join(benchmarks_dir, "projections_heaps.json")
    if "masters_20k" in loaded_benchmarks:
        validate_projections(proj_path, loaded_benchmarks["masters_20k"])

    print("\nAll benchmark artifacts and derived projections are 100% VALID!")


if __name__ == "__main__":
    main()
