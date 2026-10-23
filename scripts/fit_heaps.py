import json
import math

runs = ["masters_1k", "masters_2.5k", "masters_5k", "masters_10k", "masters_20k"]
data = []
for r in runs:
    with open(f"benchmarks/results/{r}.json") as f:
        d = json.load(f)
        n = d["accepted_unique"]
        d20_pos = d["depth_20"]["unique_positions"]
        d30_pos = d["depth_30"]["unique_positions"]
        d40_pos = d["depth_40"]["unique_positions"]
        d60_pos = d["depth_60"]["unique_positions"]
        d20_bytes = d["depth_20"]["immutable_index_size_bytes"]
        d30_bytes = d["depth_30"]["immutable_index_size_bytes"]
        d40_bytes = d["depth_40"]["immutable_index_size_bytes"]
        d60_bytes = d["depth_60"]["immutable_index_size_bytes"]
        codb_bytes = d["codb_block_compressed_bytes"]
        codb_dir_bytes = d["codb_block_directory_bytes"]
        data.append(
            (
                n,
                d20_pos,
                d30_pos,
                d40_pos,
                d60_pos,
                d20_bytes,
                d30_bytes,
                d40_bytes,
                d60_bytes,
                codb_bytes,
                codb_dir_bytes,
            )
        )

header = (
    f"{'N':>6} | {'Depth 20 Pos':>12} | {'Depth 30 Pos':>12} | "
    f"{'Depth 40 Pos':>12} | {'Depth 60 Pos':>12} | {'Bytes/Game':>10}"
)
print(header)
print("-" * len(header))
for row in data:
    n, p20, p30, p40, p60, b20, b30, b40, b60, cg, cdir = row
    print(f"{n:>6} | {p20:>12} | {p30:>12} | {p40:>12} | {p60:>12} | {cg/n:>10.2f}")


def fit_heaps(points):
    # OLS fit of ln(U) = ln(k) + alpha * ln(N) across nested prefix points.
    # NOTE: Because points are nested cumulative prefixes (N=1k..20k, df=3),
    # s_err is an in-sample log-slope residual error, NOT a formal statistical
    # confidence interval for 250x-500x out-of-sample extrapolation.
    xs = [math.log(p[0]) for p in points]
    ys = [math.log(p[1]) for p in points]
    n = len(xs)
    mean_x = sum(xs) / n
    mean_y = sum(ys) / n
    ss_xx = sum((x - mean_x) ** 2 for x in xs)
    ss_xy = sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys))
    ss_yy = sum((y - mean_y) ** 2 for y in ys)
    alpha = ss_xy / ss_xx
    ln_k = mean_y - alpha * mean_x
    k = math.exp(ln_k)
    r2 = (ss_xy**2) / (ss_xx * ss_yy)
    s_err = math.sqrt(max(0.0, (ss_yy - alpha * ss_xy) / (n - 2))) / math.sqrt(ss_xx)
    return k, alpha, r2, s_err


p_d20 = [(r[0], r[1]) for r in data]
p_d30 = [(r[0], r[2]) for r in data]
p_d40 = [(r[0], r[3]) for r in data]
p_d60 = [(r[0], r[4]) for r in data]

# Measured CPI bytes/position at N=20,000
n20k = data[-1]
b_per_pos = {
    "depth_20": round(n20k[5] / n20k[1], 2),
    "depth_30": round(n20k[6] / n20k[2], 2),
    "depth_40": round(n20k[7] / n20k[3], 2),
    "depth_60": round(n20k[8] / n20k[4], 2),
}

print("\n=== Empirical Heaps' Law Fits (N in 1k..20k) & Sensitivity Bounds ===")
fits = {}
for key, label, pts in [
    ("depth_20", "Depth 20", p_d20),
    ("depth_30", "Depth 30", p_d30),
    ("depth_40", "Depth 40", p_d40),
    ("depth_60", "Depth 60", p_d60),
]:
    k, alpha, r2, s_err = fit_heaps(pts)
    # Bounded sensitivity delta: max(2 * in-sample s_err, 0.015)
    delta_alpha = max(round(2.0 * s_err, 4), 0.015)
    fits[key] = {
        "k": round(k, 2),
        "alpha_ols": round(alpha, 4),
        "in_sample_slope_std_err": round(s_err, 4),
        "sensitivity_delta_alpha": delta_alpha,
        "r2": round(r2, 6),
        "measured_bytes_per_pos_at_20k": b_per_pos[key],
    }
    print(
        f"\n{label}: k = {k:.2f}, alpha_ols = {alpha:.4f} (in-sample s_err = {s_err:.4f}, "
        f"sensitivity +/- {delta_alpha:.4f}), R^2 = {r2:.6f}, CPI density = {b_per_pos[key]:.2f} B/pos"
    )
    for proj_n in [100_000, 500_000, 1_000_000, 2_000_000, 5_000_000, 10_000_000]:
        u_base = k * (proj_n**alpha)
        u_opt = k * (proj_n ** (alpha - delta_alpha))
        u_cons = k * (proj_n ** (alpha + delta_alpha))
        print(
            f"   N={proj_n:>10,}: Baseline U={u_base:>12,.0f} "
            f"[Optimistic: {u_opt:>12,.0f} | Conservative: {u_cons:>12,.0f}]"
        )

mean_codb_bpg = round(sum(r[9] / r[0] for r in data) / len(data), 2)
n20k_codb_bpg = round(n20k[9] / n20k[0], 2)


def project_index_gb(depth_key, n_games, mode="baseline"):
    f = fits[depth_key]
    k = f["k"]
    alpha = f["alpha_ols"]
    delta = f["sensitivity_delta_alpha"]
    bpp = f["measured_bytes_per_pos_at_20k"]
    if mode == "optimistic":
        u = k * (n_games ** (alpha - delta))
        return (u * (bpp * 0.95)) / 1e9
    elif mode == "conservative":
        u = k * (n_games ** (alpha + delta))
        return (u * (bpp * 1.15)) / 1e9
    else:
        u = k * (n_games**alpha)
        return (u * bpp) / 1e9


# Deployed overhead estimate for 15M games (5M Masters + 10M Elite):
# 1. CGB block directory framing: 64B + 28B * ceil(15M / 512) = ~0.82 MB
# 2. Global GameID -> (block_id, offset) lookup index (12 B/game) = 180 MB (0.180 GB)
# 3. Provenance & SHA-256 manifests = 5 MB (0.005 GB)
# 4. Filesystem alignment & operational safety reserve = 0.250 GB
deployed_ancillary_overhead_gb = 0.44

games_5m_base = (5_000_000 * n20k_codb_bpg) / 1e9
games_10m_base = (10_000_000 * n20k_codb_bpg) / 1e9
games_15m_base = games_5m_base + games_10m_base
games_15m_cons = (15_000_000 * (n20k_codb_bpg * 1.10)) / 1e9

output_json = {
    "methodology_note": (
        "Parameters are fitted via OLS log-linear regression across 5 nested cumulative prefixes "
        "(N=1,000 to N=20,000 accepted Masters OTB games). Because prefixes are nested and N=5M..10M "
        "is a 250x-500x extrapolation beyond the measured domain, bounds are presented as sensitivity "
        "scenarios (+/- delta_alpha and +15% bytes/pos margin) rather than formal statistical confidence intervals. "
        "Elite Online projections use Masters OTB scaling parameters as an unvalidated proxy because the "
        "2013-01 Lichess sample had 0 accepted games under EliteOnlinePolicy."
    ),
    "parameters": {
        **fits,
        "game_bytes_codb_mean": mean_codb_bpg,
        "game_bytes_codb_at_20k": n20k_codb_bpg,
        "sqlite_multiplier_vs_cpi_at_d40": round(n20k[7] and (78602240 / n20k[7]), 2),
    },
    "deployed_storage_inventory_15m_gb": {
        "measured_extrapolated_game_blocks_5m_masters": round(games_5m_base, 2),
        "proxy_extrapolated_game_blocks_10m_elite": round(games_10m_base, 2),
        "total_core_game_blocks_15m": round(games_15m_base, 2),
        "estimated_cgb_block_directory_framing": 0.001,
        "estimated_game_id_lookup_table_12b_per_game": 0.18,
        "estimated_provenance_manifests": 0.005,
        "estimated_filesystem_alignment_margin": 0.254,
        "total_ancillary_deployment_overhead": deployed_ancillary_overhead_gb,
    },
    "budget_scenarios_gb": {
        "option_a_depth_20_both": {
            "masters_index_d20_baseline": round(project_index_gb("depth_20", 5_000_000, "baseline"), 2),
            "elite_index_d20_proxy_baseline": round(project_index_gb("depth_20", 10_000_000, "baseline"), 2),
            "core_payload_baseline": round(
                games_15m_base
                + project_index_gb("depth_20", 5_000_000, "baseline")
                + project_index_gb("depth_20", 10_000_000, "baseline"),
                2,
            ),
            "deployed_total_baseline": round(
                games_15m_base
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_20", 5_000_000, "baseline")
                + project_index_gb("depth_20", 10_000_000, "baseline"),
                2,
            ),
            "deployed_total_conservative": round(
                games_15m_cons
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_20", 5_000_000, "conservative")
                + project_index_gb("depth_20", 10_000_000, "conservative"),
                2,
            ),
        },
        "option_b_recommended_d40_masters_d30_elite": {
            "masters_index_d40_baseline": round(project_index_gb("depth_40", 5_000_000, "baseline"), 2),
            "elite_index_d30_proxy_baseline": round(project_index_gb("depth_30", 10_000_000, "baseline"), 2),
            "core_payload_baseline": round(
                games_15m_base
                + project_index_gb("depth_40", 5_000_000, "baseline")
                + project_index_gb("depth_30", 10_000_000, "baseline"),
                2,
            ),
            "deployed_total_optimistic": round(
                games_15m_base * 0.95
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_40", 5_000_000, "optimistic")
                + project_index_gb("depth_30", 10_000_000, "optimistic"),
                2,
            ),
            "deployed_total_baseline": round(
                games_15m_base
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_40", 5_000_000, "baseline")
                + project_index_gb("depth_30", 10_000_000, "baseline"),
                2,
            ),
            "deployed_total_conservative": round(
                games_15m_cons
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_40", 5_000_000, "conservative")
                + project_index_gb("depth_30", 10_000_000, "conservative"),
                2,
            ),
        },
        "option_c_depth_40_both": {
            "masters_index_d40_baseline": round(project_index_gb("depth_40", 5_000_000, "baseline"), 2),
            "elite_index_d40_proxy_baseline": round(project_index_gb("depth_40", 10_000_000, "baseline"), 2),
            "core_payload_baseline": round(
                games_15m_base
                + project_index_gb("depth_40", 5_000_000, "baseline")
                + project_index_gb("depth_40", 10_000_000, "baseline"),
                2,
            ),
            "deployed_total_baseline": round(
                games_15m_base
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_40", 5_000_000, "baseline")
                + project_index_gb("depth_40", 10_000_000, "baseline"),
                2,
            ),
            "deployed_total_conservative": round(
                games_15m_cons
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_40", 5_000_000, "conservative")
                + project_index_gb("depth_40", 10_000_000, "conservative"),
                2,
            ),
        },
        "option_d_depth_60_both": {
            "masters_index_d60_baseline": round(project_index_gb("depth_60", 5_000_000, "baseline"), 2),
            "elite_index_d60_proxy_baseline": round(project_index_gb("depth_60", 10_000_000, "baseline"), 2),
            "core_payload_baseline": round(
                games_15m_base
                + project_index_gb("depth_60", 5_000_000, "baseline")
                + project_index_gb("depth_60", 10_000_000, "baseline"),
                2,
            ),
            "deployed_total_baseline": round(
                games_15m_base
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_60", 5_000_000, "baseline")
                + project_index_gb("depth_60", 10_000_000, "baseline"),
                2,
            ),
            "deployed_total_conservative": round(
                games_15m_cons
                + deployed_ancillary_overhead_gb
                + project_index_gb("depth_60", 5_000_000, "conservative")
                + project_index_gb("depth_60", 10_000_000, "conservative"),
                2,
            ),
        },
    },
}

with open("benchmarks/results/projections_heaps.json", "w") as out_f:
    json.dump(output_json, out_f, indent=2)
    out_f.write("\n")

print("\nUpdated benchmarks/results/projections_heaps.json:")
print(json.dumps(output_json, indent=2))

