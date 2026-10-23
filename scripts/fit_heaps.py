import json
import math
import sys

runs = ["masters_1k", "masters_2.5k", "masters_5k", "masters_10k", "masters_20k"]
data = []
for r in runs:
    with open(f"benchmarks/results/{r}.json") as f:
        d = json.load(f)
        n = d["accepted_unique"]
        d20_pos = d["depth_20"]["unique_positions"]
        d40_pos = d["depth_40"]["unique_positions"]
        d60_pos = d["depth_60"]["unique_positions"]
        d20_bytes = d["depth_20"]["immutable_index_size_bytes"]
        d40_bytes = d["depth_40"]["immutable_index_size_bytes"]
        d60_bytes = d["depth_60"]["immutable_index_size_bytes"]
        codb_bytes = d["codb_block_compressed_bytes"]
        data.append((n, d20_pos, d40_pos, d60_pos, d20_bytes, d40_bytes, d60_bytes, codb_bytes))

header = f"{'N':>6} | {'Depth 20 Pos':>13} | {'Depth 40 Pos':>13} | {'Depth 60 Pos':>13} | {'Bytes/Game':>11}"
print(header)
print("-" * len(header))
for row in data:
    n, p20, p40, p60, b20, b40, b60, cg = row
    print(f"{n:>6} | {p20:>13} | {p40:>13} | {p60:>13} | {cg/n:>11.2f}")

def fit_heaps(points):
    # ln(U) = ln(k) + alpha * ln(N)
    xs = [math.log(p[0]) for p in points]
    ys = [math.log(p[1]) for p in points]
    n = len(xs)
    mean_x = sum(xs) / n
    mean_y = sum(ys) / n
    ss_xx = sum((x - mean_x)**2 for x in xs)
    ss_xy = sum((x - mean_x)*(y - mean_y) for x, y in zip(xs, ys))
    ss_yy = sum((y - mean_y)**2 for y in ys)
    alpha = ss_xy / ss_xx
    ln_k = mean_y - alpha * mean_x
    k = math.exp(ln_k)
    r2 = (ss_xy ** 2) / (ss_xx * ss_yy)
    s_err = math.sqrt(max(0, (ss_yy - alpha * ss_xy) / (n - 2))) / math.sqrt(ss_xx)
    return k, alpha, r2, s_err

p_d20 = [(r[0], r[1]) for r in data]
p_d40 = [(r[0], r[2]) for r in data]
p_d60 = [(r[0], r[3]) for r in data]

print("\n=== Heaps' Law Fits: U(N) = k * N^alpha ===")
results = {}
for name, pts in [("Depth 20", p_d20), ("Depth 40", p_d40), ("Depth 60", p_d60)]:
    k, alpha, r2, s_err = fit_heaps(pts)
    results[name] = (k, alpha, r2, s_err)
    print(f"\n{name}: k = {k:.2f}, alpha = {alpha:.4f} (+/- {1.96*s_err:.4f} 95% CI), R^2 = {r2:.6f}")
    for proj_n in [100_000, 500_000, 1_000_000, 2_000_000, 5_000_000, 10_000_000]:
        proj_u = k * (proj_n ** alpha)
        # CI for projection
        low = math.exp(math.log(k) + (alpha - 1.96*s_err)*math.log(proj_n))
        high = math.exp(math.log(k) + (alpha + 1.96*s_err)*math.log(proj_n))
        print(f"   N={proj_n:>10,}: U={proj_u:>12,.0f} [95% CI: {low:>12,.0f} - {high:>12,.0f}]")

# Storage ratio per unique position
print("\n=== Storage Density (Bytes per Unique Position in CPI) ===")
for row in data:
    n, p20, p40, p60, b20, b40, b60, cg = row
    print(f"N={n:>6}: Depth 20: {b20/p20:.2f} B/pos | Depth 40: {b40/p40:.2f} B/pos | Depth 60: {b60/p60:.2f} B/pos")
