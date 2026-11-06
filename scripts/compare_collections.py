import json
import os

print(f"{'Scale':>8} | {'Collection':>14} | {'D20 Pos':>12} | {'D30 Pos':>12} | {'D40 Pos':>12} | {'Bytes/Game':>10}")
print("-" * 80)

points = [
    ("1k", "benchmarks/results/historical/masters_1k.json", "benchmarks/results/historical/pr02_scale_elite_1k.json"),
    ("5k", "benchmarks/results/historical/masters_5k.json", "benchmarks/results/historical/pr02_scale_elite_5k.json"),
    ("10k", "benchmarks/results/historical/masters_10k.json", "benchmarks/results/historical/pr02_scale_elite_10k.json"),
    ("20k", "benchmarks/results/historical/masters_20k.json", None),
    ("50k", None, "benchmarks/results/historical/pr02_scale_elite_50k.json"),
    ("100k", None, "benchmarks/results/historical/pr02_scale_elite_100k.json"),
]

for n, otb_f, elite_f in points:
    if otb_f and os.path.exists(otb_f):
        with open(otb_f) as f:
            d_otb = json.load(f)
        print(f"{n:>8} | {'Masters OTB':>14} | {d_otb['depth_20']['unique_positions']:>12,} | {d_otb['depth_30']['unique_positions']:>12,} | {d_otb['depth_40']['unique_positions']:>12,} | {d_otb['codb_bytes_per_game']:>10.2f}")
    if elite_f and os.path.exists(elite_f):
        with open(elite_f) as f:
            d_elite = json.load(f)
        print(f"{n:>8} | {'Elite Online':>14} | {d_elite['depth_20']['unique_positions']:>12,} | {d_elite['depth_30']['unique_positions']:>12,} | {d_elite['depth_40']['unique_positions']:>12,} | {d_elite['codb_bytes_per_game']:>10.2f}")

