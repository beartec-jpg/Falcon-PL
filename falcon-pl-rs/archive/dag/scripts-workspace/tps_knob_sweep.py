#!/usr/bin/env python3
"""Phase 50a: controlled Falcon-512 TPS knob sweep (one change per experiment).

Runs EXP 0–12 via multinode_metrics.sh, logs JSON, prints comparison table.

Usage:
  CRYPTO=falcon-512 python3 scripts/tps_knob_sweep.py
  EXPS=0,1,2 python3 scripts/tps_knob_sweep.py   # subset
  OUT_DIR=/tmp/tps-sweep python3 scripts/tps_knob_sweep.py
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / "scripts" / "multinode_metrics.sh"
OUT_DIR = Path(os.environ.get("OUT_DIR", f"/tmp/tps-sweep-{int(time.time())}"))
CRYPTO = os.environ.get("CRYPTO", "falcon-512")
NTX = os.environ.get("NTX", "200")
BASELINE_TPS = float(os.environ.get("BASELINE_REF", "263"))
GOAL_MIN = 400.0
GOAL_TARGET = 600.0
GOAL_STRETCH = 800.0

# Base knobs match Falcon shared-wallet win run
BASE = {
    "CRYPTO": CRYPTO,
    "NTX": NTX,
    "FLOOD": "1",
    "PACK_SKIP_HOLE_AFTER": "0.5",
    "MAX_BATCH_TXS": "256",
    "BATCH_INTERVAL": "0.05",
    "SEQUENCE_WINDOW": "512",
    "UNL_THRESHOLD": "0.6",
    "ADAPTIVE_BATCH": "true",
    "BATCH_INTERVAL_MIN": "0.02",
    "BATCH_INTERVAL_MAX": "0.4",
    "ADAPTIVE_MEMPOOL_LOW": "16",
    "ADAPTIVE_MEMPOOL_HIGH": "256",
    "MAX_UNCERTIFIED_PER_CREATOR": "4",
    "SINGLE_PACKER": "true",
}

# Each experiment: name, description, risk, overrides (single knob vs BASE except exp0/7/combo)
EXPERIMENTS = [
    {
        "id": "EXP0",
        "name": "baseline_lock",
        "risk": "none",
        "desc": "Lock Falcon-512 reference (shared wallet, skip-hole 0.5, batch 256)",
        "overrides": {},
    },
    {
        "id": "EXP1",
        "name": "batch_512",
        "risk": "low",
        "desc": "max_batch_txs 256 → 512",
        "overrides": {"MAX_BATCH_TXS": "512"},
    },
    {
        "id": "EXP2",
        "name": "batch_1024",
        "risk": "low",
        "desc": "max_batch_txs 256 → 1024",
        "overrides": {"MAX_BATCH_TXS": "1024"},
    },
    {
        "id": "EXP3",
        "name": "aggressive_interval",
        "risk": "medium",
        "desc": "adaptive mempool high lower + faster floor (aggressive interval)",
        "overrides": {
            "BATCH_INTERVAL_MIN": "0.01",
            "ADAPTIVE_MEMPOOL_HIGH": "64",
            "BATCH_INTERVAL": "0.03",
        },
    },
    {
        "id": "EXP4",
        "name": "sequence_window_2048",
        "risk": "low",
        "desc": "account_sequence_window 512 → 2048",
        "overrides": {"SEQUENCE_WINDOW": "2048"},
    },
    {
        "id": "EXP5",
        "name": "uncertified_cap_12",
        "risk": "medium",
        "desc": "max_uncertified_per_creator 4 → 12",
        "overrides": {"MAX_UNCERTIFIED_PER_CREATOR": "12"},
    },
    {
        "id": "EXP6",
        "name": "unl_threshold_0.51",
        "risk": "high",
        "desc": "unl_threshold 0.6 → 0.51 (~3/5 still; faster cert)",
        "overrides": {"UNL_THRESHOLD": "0.51"},
    },
    {
        "id": "EXP7",
        "name": "combo_best_of_1_5",
        "risk": "medium",
        "desc": "Combo placeholder: batch 1024 + seq 2048 + uncert 12 + aggressive interval",
        "overrides": {
            "MAX_BATCH_TXS": "1024",
            "SEQUENCE_WINDOW": "2048",
            "MAX_UNCERTIFIED_PER_CREATOR": "12",
            "BATCH_INTERVAL_MIN": "0.01",
            "ADAPTIVE_MEMPOOL_HIGH": "64",
            "BATCH_INTERVAL": "0.03",
        },
        "note": "Will be re-run after sweep if winners differ",
    },
    {
        "id": "EXP8",
        "name": "multi_packer",
        "risk": "high",
        "desc": "single_packer false (val1+val2 may pack)",
        "overrides": {"SINGLE_PACKER": "false"},
    },
    {
        "id": "EXP9",
        "name": "interval_floor_0.01",
        "risk": "medium",
        "desc": "batch_interval_min 0.02 → 0.01 only",
        "overrides": {"BATCH_INTERVAL_MIN": "0.01"},
    },
    {
        "id": "EXP10",
        "name": "speculative_cert_validation",
        "risk": "high",
        "desc": "DEFERRED — requires code change (speculative cert apply)",
        "deferred": True,
        "overrides": {},
    },
    {
        "id": "EXP11",
        "name": "pull_stress_ntx500",
        "risk": "medium",
        "desc": "NTX=500 pull-path / mesh stress (same knobs as baseline)",
        "overrides": {"NTX": "500"},
    },
    {
        "id": "EXP12",
        "name": "skip_hole_off_fat_batch",
        "risk": "low",
        "desc": "skip-hole OFF + fat batch 1024 (redundancy of skip-hole)",
        "overrides": {"PACK_SKIP_HOLE_AFTER": "0", "MAX_BATCH_TXS": "1024"},
    },
]


def run_exp(exp: dict) -> dict:
    eid = exp["id"]
    if exp.get("deferred"):
        return {
            "id": eid,
            "name": exp["name"],
            "risk": exp["risk"],
            "desc": exp["desc"],
            "status": "deferred",
            "settle_tps": None,
            "delta_vs_baseline_pct": None,
            "note": "Speculative cert validation not implemented; deferred per plan",
        }

    env = os.environ.copy()
    env.update(BASE)
    env.update(exp.get("overrides") or {})
    env["LABEL"] = f"{eid}_{exp['name']}"
    out_json = OUT_DIR / f"{eid}.json"
    env["RESULTS_JSON"] = str(out_json)
    log_path = OUT_DIR / f"{eid}.log"

    print(f"\n======== {eid} {exp['name']} ========", flush=True)
    print(f"  {exp['desc']}", flush=True)
    print(f"  overrides={exp.get('overrides')}", flush=True)

    t0 = time.time()
    with open(log_path, "w") as logf:
        proc = subprocess.run(
            ["bash", str(HARNESS)],
            cwd=str(ROOT),
            env=env,
            stdout=logf,
            stderr=subprocess.STDOUT,
            timeout=int(os.environ.get("EXP_TIMEOUT", "240")),
        )
    wall = time.time() - t0

    result = {
        "id": eid,
        "name": exp["name"],
        "risk": exp["risk"],
        "desc": exp["desc"],
        "overrides": exp.get("overrides") or {},
        "status": "ok" if proc.returncode in (0, 1) else "fail",
        "harness_exit": proc.returncode,
        "wall_s_harness": wall,
        "log": str(log_path),
        "results_json": str(out_json),
    }
    if out_json.exists():
        data = json.loads(out_json.read_text())
        result.update(
            {
                "settle_tps": data.get("settle_tps"),
                "admit_ok": data.get("admit_ok"),
                "admit_fail": data.get("admit_fail"),
                "admit_pct": data.get("admit_pct"),
                "executed_min": data.get("executed_min"),
                "executed_max": data.get("executed_max"),
                "agreement": data.get("agreement"),
                "wall_s": data.get("wall_s"),
                "submit_tps": data.get("submit_tps"),
                "crypto_scheme": data.get("crypto_scheme"),
                "wallet_scheme": data.get("wallet_scheme"),
                "cert_apply_lag_ms_max": data.get("cert_apply_lag_ms_max"),
                "cert_from_create_lag_ms_max": data.get("cert_from_create_lag_ms_max"),
                "soft_finality_ms_p50": data.get("soft_finality_ms_p50"),
                "soft_finality_ms_p99": data.get("soft_finality_ms_p99"),
                "knobs": data.get("knobs"),
            }
        )
    else:
        result["status"] = "no_json"
        # try parse settle from log
        try:
            text = log_path.read_text()
            for line in text.splitlines():
                if "settle_tps=" in line:
                    # apply_pct=... settle_tps=X
                    import re

                    m = re.search(r"settle_tps=([0-9.]+)", line)
                    if m:
                        result["settle_tps"] = float(m.group(1))
                if "crypto_scheme=" in line:
                    result.setdefault("crypto_scheme", line.split("crypto_scheme=")[-1].split()[0])
        except Exception:
            pass

    print(
        f"  → status={result['status']} settle_tps={result.get('settle_tps')} "
        f"exec_min={result.get('executed_min')} crypto={result.get('crypto_scheme')}",
        flush=True,
    )
    return result


def main() -> int:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    want = os.environ.get("EXPS", "")
    if want.strip():
        ids = {x.strip().upper() if x.strip()[0].isdigit() is False else f"EXP{x.strip()}" for x in want.split(",")}
        # normalize 0 -> EXP0
        norm = set()
        for x in want.split(","):
            x = x.strip().upper()
            if not x:
                continue
            if x.startswith("EXP"):
                norm.add(x)
            else:
                norm.add(f"EXP{x}")
        exps = [e for e in EXPERIMENTS if e["id"] in norm]
    else:
        exps = EXPERIMENTS

    print(f"OUT_DIR={OUT_DIR}")
    print(f"CRYPTO={CRYPTO} NTX={NTX} experiments={len(exps)}")
    print(f"HARNESS={HARNESS}")

    # ensure cargo env + binary
    env = os.environ.copy()
    cargo = Path.home() / ".cargo" / "env"
    if cargo.exists():
        # source via bash wrapper for harness only
        pass

    results = []
    for exp in exps:
        try:
            results.append(run_exp(exp))
        except subprocess.TimeoutExpired:
            results.append(
                {
                    "id": exp["id"],
                    "name": exp["name"],
                    "status": "timeout",
                    "settle_tps": None,
                }
            )
        except Exception as e:
            results.append(
                {
                    "id": exp["id"],
                    "name": exp["name"],
                    "status": f"error:{e}",
                    "settle_tps": None,
                }
            )

    # baseline from EXP0 if present
    base_tps = None
    for r in results:
        if r.get("id") == "EXP0" and r.get("settle_tps") is not None:
            base_tps = float(r["settle_tps"])
            break
    if base_tps is None:
        base_tps = BASELINE_TPS

    for r in results:
        tps = r.get("settle_tps")
        if tps is None:
            r["delta_vs_baseline_pct"] = None
            r["delta_vs_ref263_pct"] = None
        else:
            r["delta_vs_baseline_pct"] = (float(tps) - base_tps) / base_tps * 100.0
            r["delta_vs_ref263_pct"] = (float(tps) - BASELINE_TPS) / BASELINE_TPS * 100.0

    # ranking
    runnable = [r for r in results if r.get("settle_tps") is not None]
    runnable.sort(key=lambda r: -float(r["settle_tps"]))

    summary = {
        "out_dir": str(OUT_DIR),
        "crypto": CRYPTO,
        "ntx_default": NTX,
        "baseline_exp0_tps": base_tps,
        "ref_263": BASELINE_TPS,
        "goals": {"min": GOAL_MIN, "target": GOAL_TARGET, "stretch": GOAL_STRETCH},
        "results": results,
        "ranking": [{"id": r["id"], "name": r["name"], "settle_tps": r["settle_tps"]} for r in runnable],
        "winners": [r for r in runnable if float(r["settle_tps"]) > base_tps * 1.05],
        "losers": [r for r in runnable if float(r["settle_tps"]) < base_tps * 0.95],
    }

    # goals
    best = runnable[0] if runnable else None
    if best:
        bt = float(best["settle_tps"])
        summary["best"] = best
        summary["goal_min_met"] = bt >= GOAL_MIN
        summary["goal_target_met"] = bt >= GOAL_TARGET
        summary["goal_stretch_met"] = bt >= GOAL_STRETCH

    summary_path = OUT_DIR / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")

    # human report
    print("\n" + "=" * 72)
    print("TPS KNOB SWEEP COMPARISON (Falcon-512)")
    print("=" * 72)
    print(f"baseline EXP0 settle_tps={base_tps:.2f}  ref263={BASELINE_TPS:.0f}")
    print(f"{'ID':<6} {'name':<28} {'tps':>8} {'Δbase%':>8} {'exec':>6} {'agree':>6} {'risk':<8} status")
    for r in results:
        tps = r.get("settle_tps")
        tps_s = f"{tps:.1f}" if tps is not None else "—"
        dlt = r.get("delta_vs_baseline_pct")
        dlt_s = f"{dlt:+.1f}" if dlt is not None else "—"
        exec_s = str(r.get("executed_min") if r.get("executed_min") is not None else "—")
        agr = "Y" if r.get("agreement") else ("N" if r.get("agreement") is False else "—")
        print(
            f"{r.get('id',''):<6} {r.get('name','')[:28]:<28} {tps_s:>8} {dlt_s:>8} "
            f"{exec_s:>6} {agr:>6} {str(r.get('risk','')):<8} {r.get('status')}"
        )

    print("\nRanking (by settle_tps):")
    for i, r in enumerate(runnable, 1):
        print(f"  {i}. {r['id']} {r['name']}: {r['settle_tps']:.2f} TPS ({r.get('delta_vs_baseline_pct', 0):+.1f}% vs EXP0)")

    if best:
        print(f"\nBEST: {best['id']} {best['name']} @ {best['settle_tps']:.2f} TPS")
        print(f"  goal min 400: {'MET' if summary['goal_min_met'] else 'NOT MET'}")
        print(f"  goal target 600: {'MET' if summary['goal_target_met'] else 'NOT MET'}")
        print(f"  goal stretch 800: {'MET' if summary['goal_stretch_met'] else 'NOT MET'}")

    print(f"\nWinners (>5% over EXP0): {[w['id'] for w in summary['winners']]}")
    print(f"Losers  (<5% under EXP0): {[w['id'] for w in summary['losers']]}")
    print(f"\nWrote {summary_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
