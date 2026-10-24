#!/usr/bin/env python3
"""Robustness suite: find highest FULL-APPLY TPS with high success rate.

Metrics:
  - full_apply rate (>=95% exec)
  - full_tps mean/stdev among successes
  - fail modes
Uses account-sharded multi-packer + multi-account floods.
"""
import json, os, subprocess, time, statistics
from pathlib import Path

ROOT = Path("/opt/falcon-dag/falcon-dag-rs")
HARNESS = ROOT / "scripts/multinode_metrics.sh"
OUT = Path(os.environ.get("OUT_DIR", f"/tmp/tps-robust-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)
REPS = int(os.environ.get("REPS", "6"))
COOLDOWN = float(os.environ.get("COOLDOWN", "2.5"))

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
    ACCOUNTS="1",
)

# Configs to evaluate for robustness
CONFIGS = [
    ("SP_A1", "single-packer 1-acct (classic)", {}),
    ("SP_A10", "single-packer 10-acct", {"ACCOUNTS": "10"}),
    ("SP_A20", "single-packer 20-acct", {"ACCOUNTS": "20"}),
    ("MP_A1", "multi-pack sharded 1-acct (expect ~1 packer)", {"SINGLE_PACKER": "false", "ACCOUNTS": "1"}),
    ("MP_A5", "multi-pack sharded 5-acct", {"SINGLE_PACKER": "false", "ACCOUNTS": "5"}),
    ("MP_A10", "multi-pack sharded 10-acct", {"SINGLE_PACKER": "false", "ACCOUNTS": "10"}),
    ("MP_A20", "multi-pack sharded 20-acct", {"SINGLE_PACKER": "false", "ACCOUNTS": "20"}),
    ("MP_A10_B512", "multi-pack 10-acct batch512", {
        "SINGLE_PACKER": "false", "ACCOUNTS": "10", "MAX_BATCH_TXS": "512",
    }),
    ("MP_A10_U8", "multi-pack 10-acct uncert8", {
        "SINGLE_PACKER": "false", "ACCOUNTS": "10", "MAX_UNCERTIFIED_PER_CREATOR": "8",
    }),
    ("MP_A10_FAST", "multi-pack 10-acct interval 0.03/min0.01", {
        "SINGLE_PACKER": "false", "ACCOUNTS": "10",
        "BATCH_INTERVAL": "0.03", "BATCH_INTERVAL_MIN": "0.01", "ADAPTIVE_MEMPOOL_HIGH": "64",
    }),
    ("MP_A20_B512_U8", "multi-pack 20-acct batch512 uncert8", {
        "SINGLE_PACKER": "false", "ACCOUNTS": "20",
        "MAX_BATCH_TXS": "512", "MAX_UNCERTIFIED_PER_CREATOR": "8",
    }),
    ("SP_A10_B512", "single-pack 10-acct batch512", {"ACCOUNTS": "10", "MAX_BATCH_TXS": "512"}),
    ("SP_A10_FAST", "single-pack 10-acct aggressive", {
        "ACCOUNTS": "10", "BATCH_INTERVAL": "0.03", "BATCH_INTERVAL_MIN": "0.01",
        "ADAPTIVE_MEMPOOL_HIGH": "64",
    }),
    ("MP_A10_NTX400", "multi-pack 10-acct NTX=400 stress", {
        "SINGLE_PACKER": "false", "ACCOUNTS": "10", "NTX": "400",
    }),
    ("SP_A10_NTX400", "single-pack 10-acct NTX=400 stress", {
        "ACCOUNTS": "10", "NTX": "400",
    }),
]


def cleanup_ports():
    for p in list(range(29800, 29805)) + list(range(29900, 29905)):
        subprocess.run(["fuser", "-k", f"{p}/tcp"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def run_one(cfg_id, desc, overrides, rep):
    name = f"{cfg_id}_r{rep}"
    env = os.environ.copy()
    env.update(BASE)
    env.update(overrides)
    env["LABEL"] = name
    jpath = OUT / f"{name}.json"
    lpath = OUT / f"{name}.log"
    env["RESULTS_JSON"] = str(jpath)
    cleanup_ports()
    time.sleep(COOLDOWN)
    t0 = time.time()
    with open(lpath, "w") as lf:
        try:
            proc = subprocess.run(
                ["bash", str(HARNESS)], cwd=str(ROOT), env=env,
                stdout=lf, stderr=subprocess.STDOUT,
                timeout=int(os.environ.get("EXP_TIMEOUT", "120")),
            )
            code = proc.returncode
        except subprocess.TimeoutExpired:
            code = -9
    row = {"cfg": cfg_id, "desc": desc, "rep": rep, "name": name, "overrides": overrides, "exit": code}
    if jpath.exists():
        d = json.loads(jpath.read_text())
        ntx = int(d.get("ntx") or env.get("NTX") or 200)
        emin = int(d.get("executed_min") or 0)
        wall = float(d.get("wall_s") or 0)
        tps = float(d.get("settle_tps") or 0)
        full = emin >= ntx * 0.95 and bool(d.get("agreement"))
        # stricter: full apply AND agreement
        full_tps = (emin / wall) if wall > 0 and full else None
        row.update({
            "settle_tps": tps, "full_tps": full_tps, "executed_min": emin, "ntx": ntx,
            "wall_s": wall, "agreement": d.get("agreement"), "admit_pct": d.get("admit_pct"),
            "crypto": d.get("crypto_scheme"), "full_ok": full,
            "soft_p50": d.get("soft_finality_ms_p50"),
        })
        print(f"  {name}: full_tps={full_tps} exec={emin}/{ntx} wall={wall:.2f} agree={d.get('agreement')}", flush=True)
    else:
        row["full_ok"] = False
        print(f"  {name}: NO JSON", flush=True)
    return row


def main():
    print(f"OUT={OUT} REPS={REPS} CRYPTO=falcon-512 account-sharded multi-packer", flush=True)
    all_rows = []
    for cfg_id, desc, ov in CONFIGS:
        print(f"\n======== {cfg_id}: {desc} ========", flush=True)
        for r in range(1, REPS + 1):
            all_rows.append(run_one(cfg_id, desc, ov, r))

    # summarize per config
    by = {}
    for row in all_rows:
        by.setdefault(row["cfg"], []).append(row)

    summary = {"out": str(OUT), "reps": REPS, "configs": {}}
    ranking = []
    for cfg_id, desc, ov in CONFIGS:
        rows = by.get(cfg_id, [])
        oks = [r for r in rows if r.get("full_ok") and r.get("full_tps") is not None]
        full_tps = [r["full_tps"] for r in oks]
        entry = {
            "desc": desc,
            "overrides": ov,
            "n": len(rows),
            "full_ok_n": len(oks),
            "success_rate": len(oks) / len(rows) if rows else 0,
            "full_tps_mean": statistics.mean(full_tps) if full_tps else None,
            "full_tps_min": min(full_tps) if full_tps else None,
            "full_tps_max": max(full_tps) if full_tps else None,
            "full_tps_stdev": statistics.stdev(full_tps) if len(full_tps) > 1 else (0.0 if full_tps else None),
            "runs": [
                {"name": r["name"], "full_tps": r.get("full_tps"), "exec": r.get("executed_min"),
                 "agree": r.get("agreement"), "wall": r.get("wall_s")}
                for r in rows
            ],
        }
        summary["configs"][cfg_id] = entry
        if entry["full_tps_mean"] is not None:
            # robustness score: mean * success_rate (prefer stable)
            score = entry["full_tps_mean"] * entry["success_rate"]
            # require high success for "stable"
            ranking.append((score, entry["success_rate"], entry["full_tps_mean"], cfg_id, desc))

    ranking.sort(reverse=True)
    summary["ranking_stable"] = [
        {"cfg": c, "desc": d, "score": s, "success_rate": sr, "full_tps_mean": m}
        for s, sr, m, c, d in ranking
    ]
    # Best with >= 80% success
    stable = [x for x in ranking if x[1] >= 0.8]
    summary["best_stable_80"] = None
    if stable:
        s, sr, m, c, d = stable[0]
        summary["best_stable_80"] = {"cfg": c, "desc": d, "success_rate": sr, "full_tps_mean": m, "score": s}

    (OUT / "robust_summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    print("\n" + "=" * 78)
    print("ROBUSTNESS RANKING (score = full_tps_mean × success_rate)")
    print("=" * 78)
    print(f"{'cfg':<16} {'rate':>6} {'mean':>8} {'min':>8} {'max':>8} {'stdev':>7}  desc")
    for cfg_id, desc, ov in CONFIGS:
        e = summary["configs"][cfg_id]
        rate = f"{100*e['success_rate']:.0f}%"
        mean = f"{e['full_tps_mean']:.1f}" if e["full_tps_mean"] is not None else "—"
        mn = f"{e['full_tps_min']:.1f}" if e["full_tps_min"] is not None else "—"
        mx = f"{e['full_tps_max']:.1f}" if e["full_tps_max"] is not None else "—"
        sd = f"{e['full_tps_stdev']:.1f}" if e["full_tps_stdev"] is not None else "—"
        print(f"{cfg_id:<16} {rate:>6} {mean:>8} {mn:>8} {mx:>8} {sd:>7}  {desc}")

    print("\nBest configs by stable score:")
    for i, (s, sr, m, c, d) in enumerate(ranking[:8], 1):
        print(f"  {i}. {c}: mean={m:.1f} rate={100*sr:.0f}% score={s:.1f} — {d}")
    if summary["best_stable_80"]:
        b = summary["best_stable_80"]
        print(f"\nBEST ≥80% success: {b['cfg']} @ {b['full_tps_mean']:.1f} TPS ({100*b['success_rate']:.0f}% full-apply)")
    else:
        print("\nNo config reached ≥80% full-apply success.")
    print(f"\nWrote {OUT}/robust_summary.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
