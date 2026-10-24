#!/usr/bin/env python3
"""Continue Falcon TPS exploration: multi-packer stability + safe combos."""
import json, os, subprocess, time, statistics
from pathlib import Path

ROOT = Path("/opt/falcon-dag/falcon-dag-rs")
HARNESS = ROOT / "scripts/multinode_metrics.sh"
OUT = Path(os.environ.get("OUT_DIR", f"/tmp/tps-explore-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

BASE = dict(
    CRYPTO="falcon-512",
    FLOOD="1",
    NTX="200",
    PACK_SKIP_HOLE_AFTER="0.5",
    MAX_BATCH_TXS="256",
    BATCH_INTERVAL="0.05",
    SEQUENCE_WINDOW="512",
    UNL_THRESHOLD="0.6",
    ADAPTIVE_BATCH="true",
    BATCH_INTERVAL_MIN="0.02",
    BATCH_INTERVAL_MAX="0.4",
    ADAPTIVE_MEMPOOL_LOW="16",
    ADAPTIVE_MEMPOOL_HIGH="256",
    MAX_UNCERTIFIED_PER_CREATOR="4",
    SINGLE_PACKER="true",
)

# name -> overrides
RUNS = []

# Baseline x3
for i in range(1, 4):
    RUNS.append((f"B{i}_baseline", {}))

# Multi-packer x5 stability
for i in range(1, 6):
    RUNS.append((f"M{i}_multipacker", {"SINGLE_PACKER": "false"}))

# Multi-packer + fat 512
for i in range(1, 3):
    RUNS.append((f"MF{i}_mp_batch512", {"SINGLE_PACKER": "false", "MAX_BATCH_TXS": "512"}))

# Multi-packer + uncert 8
for i in range(1, 3):
    RUNS.append((f"MU{i}_mp_uncert8", {"SINGLE_PACKER": "false", "MAX_UNCERTIFIED_PER_CREATOR": "8"}))

# Multi-packer + interval floor 0.01
for i in range(1, 3):
    RUNS.append((f"MI{i}_mp_intmin01", {"SINGLE_PACKER": "false", "BATCH_INTERVAL_MIN": "0.01"}))

# Multi-packer + uncert8 + intmin + batch512 (combo)
for i in range(1, 3):
    RUNS.append((f"MC{i}_mp_combo", {
        "SINGLE_PACKER": "false",
        "MAX_BATCH_TXS": "512",
        "MAX_UNCERTIFIED_PER_CREATOR": "8",
        "BATCH_INTERVAL_MIN": "0.01",
        "BATCH_INTERVAL": "0.04",
    }))

# Single packer + aggressive but require longer (same as EXP3) x2
for i in range(1, 3):
    RUNS.append((f"A{i}_aggressive", {
        "BATCH_INTERVAL": "0.03",
        "BATCH_INTERVAL_MIN": "0.01",
        "ADAPTIVE_MEMPOOL_HIGH": "64",
    }))

# NTX=400 multi-packer stress
RUNS.append(("N400_mp", {"SINGLE_PACKER": "false", "NTX": "400"}))
RUNS.append(("N400_base", {"NTX": "400"}))


def run_one(name, overrides):
    env = os.environ.copy()
    env.update(BASE)
    env.update(overrides)
    env["LABEL"] = name
    jpath = OUT / f"{name}.json"
    lpath = OUT / f"{name}.log"
    env["RESULTS_JSON"] = str(jpath)
    print(f"\n======== {name} ========", flush=True)
    print(f"  {overrides or 'baseline'}", flush=True)
    t0 = time.time()
    with open(lpath, "w") as lf:
        try:
            r = subprocess.run(
                ["bash", str(HARNESS)],
                cwd=str(ROOT),
                env=env,
                stdout=lf,
                stderr=subprocess.STDOUT,
                timeout=int(os.environ.get("EXP_TIMEOUT", "150")),
            )
            code = r.returncode
        except subprocess.TimeoutExpired:
            code = -9
    wall = time.time() - t0
    row = {"name": name, "overrides": overrides, "harness_exit": code, "wall_harness": wall}
    if jpath.exists():
        d = json.loads(jpath.read_text())
        ntx = int(d.get("ntx") or env.get("NTX") or 200)
        emin = int(d.get("executed_min") or 0)
        w = float(d.get("wall_s") or 0)
        tps = float(d.get("settle_tps") or 0)
        full = emin >= ntx * 0.95
        full_tps = (emin / w) if w > 0 and full else None
        row.update({
            "settle_tps": tps,
            "full_tps": full_tps,
            "executed_min": emin,
            "executed_max": d.get("executed_max"),
            "ntx": ntx,
            "wall_s": w,
            "admit_pct": d.get("admit_pct"),
            "agreement": d.get("agreement"),
            "crypto": d.get("crypto_scheme"),
            "soft_p50": d.get("soft_finality_ms_p50"),
            "full_apply": full,
        })
        print(
            f"  tps={tps:.1f} full_tps={full_tps} exec={emin}/{ntx} wall={w:.2f} "
            f"agree={d.get('agreement')} full={full}",
            flush=True,
        )
    else:
        row["error"] = "no_json"
        print("  FAILED no json", flush=True)
    return row


def main():
    results = []
    for name, ov in RUNS:
        results.append(run_one(name, ov))

    # group analysis
    def group(prefix):
        xs = [r for r in results if r["name"].startswith(prefix) and r.get("full_tps") is not None]
        if not xs:
            # also include partial with settle_tps for transparency
            xs = [r for r in results if r["name"].startswith(prefix) and r.get("settle_tps") is not None]
        return xs

    summary = {"out": str(OUT), "results": results, "groups": {}}
    for pref, label in [
        ("B", "baseline"),
        ("M", "multipacker"),
        ("MF", "mp_batch512"),
        ("MU", "mp_uncert8"),
        ("MI", "mp_intmin"),
        ("MC", "mp_combo"),
        ("A", "aggressive"),
        ("N400", "ntx400"),
    ]:
        xs = [r for r in results if r["name"].startswith(pref)]
        fulls = [r["full_tps"] for r in xs if r.get("full_tps") is not None]
        partials = [r for r in xs if r.get("full_tps") is None and r.get("settle_tps")]
        summary["groups"][label] = {
            "n": len(xs),
            "full_apply_n": len(fulls),
            "full_tps_mean": statistics.mean(fulls) if fulls else None,
            "full_tps_min": min(fulls) if fulls else None,
            "full_tps_max": max(fulls) if fulls else None,
            "full_tps_stdev": statistics.stdev(fulls) if len(fulls) > 1 else 0.0,
            "success_rate_full": len(fulls) / len(xs) if xs else 0,
            "raw": [
                {
                    "name": r["name"],
                    "settle_tps": r.get("settle_tps"),
                    "full_tps": r.get("full_tps"),
                    "exec": r.get("executed_min"),
                    "agree": r.get("agreement"),
                }
                for r in xs
            ],
        }

    # ranking full-apply only
    full_rows = [r for r in results if r.get("full_tps") is not None]
    full_rows.sort(key=lambda r: -r["full_tps"])
    summary["ranking_full_apply"] = [
        {"name": r["name"], "full_tps": r["full_tps"], "exec": r["executed_min"]}
        for r in full_rows
    ]

    base_fulls = [r["full_tps"] for r in results if r["name"].startswith("B") and r.get("full_tps")]
    base_mean = statistics.mean(base_fulls) if base_fulls else 263.0
    summary["baseline_full_mean"] = base_mean
    if full_rows:
        best = full_rows[0]
        summary["best_full"] = best
        summary["best_delta_pct"] = (best["full_tps"] - base_mean) / base_mean * 100

    (OUT / "explore_summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    print("\n" + "=" * 72)
    print("EXPLORATION SUMMARY (full-apply TPS only)")
    print("=" * 72)
    print(f"baseline mean full_tps={base_mean:.1f} n={len(base_fulls)}")
    for label, g in summary["groups"].items():
        if g["full_tps_mean"] is None:
            print(f"  {label}: full_apply {g['full_apply_n']}/{g['n']} (no full runs)")
        else:
            dlt = (g["full_tps_mean"] - base_mean) / base_mean * 100
            print(
                f"  {label}: full {g['full_apply_n']}/{g['n']} "
                f"mean={g['full_tps_mean']:.1f} "
                f"range=[{g['full_tps_min']:.1f},{g['full_tps_max']:.1f}] "
                f"stdev={g['full_tps_stdev']:.1f} Δ={dlt:+.1f}%"
            )
    print("\nTop full-apply runs:")
    for i, r in enumerate(full_rows[:10], 1):
        dlt = (r["full_tps"] - base_mean) / base_mean * 100
        print(f"  {i}. {r['name']}: {r['full_tps']:.1f} TPS ({dlt:+.1f}%) exec={r['executed_min']}")
    print(f"\nWrote {OUT}/explore_summary.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
