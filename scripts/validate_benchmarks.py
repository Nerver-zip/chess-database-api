#!/usr/bin/env python3
"""
scripts/validate_benchmarks.py — Benchmark schema, accounting, and consistency validator.

Enforces strict verification on all authoritative benchmark artifacts in benchmarks/results/:
1. Complete schema compliance and absence of obsolete/unmigrated schema fields.
2. Ingestion accounting invariants (mutually exclusive category sum and retained==accepted).
3. Monotonic position-growth and physical size invariants across depth and sample size.
4. Latency percentile invariants (p50 <= p95 <= p99) and 100% semantic equivalence.
5. Exact consistency between source benchmark JSON and derived projections (projections_heaps.json).
"""

import glob
import json
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

    # Check parameters
    params = proj["parameters"]
    for d_key in DEPTH_KEYS:
        p = params[d_key]
        if p["r2"] < 0.999:
            raise ValueError(
                f"{proj_path}: {d_key} goodness of fit R^2 is below 0.999: {p['r2']}"
            )
        if not (0.75 <= p["alpha_ols"] <= 0.98):
            raise ValueError(
                f"{proj_path}: {d_key} alpha_ols {p['alpha_ols']} out of plausible range [0.75, 0.98]."
            )

    # Check dynamic SQLite multiplier
    expected_multiplier = round(
        m20k_data["depth_40"]["sqlite_index_size_bytes"]
        / m20k_data["depth_40"]["immutable_index_size_bytes"],
        2,
    )
    actual_multiplier = params["sqlite_multiplier_vs_cpi_at_d40"]
    if abs(actual_multiplier - expected_multiplier) > 0.01:
        raise ValueError(
            f"{proj_path}: sqlite_multiplier_vs_cpi_at_d40 mismatch! "
            f"Expected {expected_multiplier} from masters_20k.json, found {actual_multiplier}."
        )

    # Check budget scenarios
    opt_b = proj["budget_scenarios_gb"]["option_b_recommended_d40_masters_d30_elite"]
    base_total = opt_b["deployed_total_baseline"]
    cons_total = opt_b["deployed_total_conservative"]

    if base_total > 10.0:
        raise ValueError(
            f"{proj_path}: Option B baseline total ({base_total} GB) exceeds 10 GB budget!"
        )
    if cons_total > 10.0:
        raise ValueError(
            f"{proj_path}: Option B conservative total ({cons_total} GB) exceeds 10 GB budget!"
        )

    print(
        f"  [OK] Projections verified: SQLite/CPI ratio={actual_multiplier}x, Option B Total={base_total} GB (Cons: {cons_total} GB)."
    )


def main():
    benchmarks_dir = "benchmarks/results"
    all_files = sorted(glob.glob(os.path.join(benchmarks_dir, "*.json")))
    if not all_files:
        print(f"No benchmark files found in {benchmarks_dir}!")
        sys.exit(1)

    # Separate projections file from benchmark outputs
    benchmark_files = [f for f in all_files if not f.endswith("projections_heaps.json")]

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
