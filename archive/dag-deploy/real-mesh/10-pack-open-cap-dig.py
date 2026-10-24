#!/usr/bin/env python3
"""N=6 multipack: compare pack_open_cap auto/1/2/4 under same load. Requires rebuilt binary."""
from __future__ import annotations

import importlib.util
import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parent
OUT = Path(os.environ.get("OUT", f"/tmp/pack-open-dig-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

_spec = importlib.util.spec_from_file_location("scale_n", ROOT / "07-scale-n-curve.py")
sc = importlib.util.module_from_spec(_spec)
assert _spec.loader
_spec.loader.exec_module(sc)

CAPS = [int(x) for x in os.environ.get("OPEN_CAPS", "1,2,4").split(",") if x.strip()]
# 0 = use binary auto (now 2 for multipack after code change)
if os.environ.get("INCLUDE_AUTO", "1") == "1":
    CAPS = [0] + [c for c in CAPS if c != 0]


def run_cell(cap: int) -> dict:
    sc.PACK_OPEN = cap
    sc.SINGLE_PACKER = False
    n = 6
    print(f"\n===== pack_open_cap={cap} (0=auto) =====", flush=True)
    sc.stop_all()
    sc.wipe_data()
    sc.ensure_keys(n)
    sc.sh(f"rsync -az {sc.BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)
    sc.start_mesh(n, sc.NETWORK_BASE + 60 + cap)
    time.sleep(2)
    bank = sc.SeqBank(n)
    # one med phase
    ph = sc.run_ladder_phase(n, f"cap{cap}_med", 50, 25, 16, bank)
    # pack_skip from val1
    m = sc.metrics(sc.F1, sc.VAL_RPC[1])
    ps = m.get("pack_stall") or {}
    snap = {
        "cap": cap,
        "exec_tps_min": ph["exec_tps_min"],
        "submit_tps": ph["submit_tps"],
        "ok": ph["ok"],
        "busy": ph["busy"],
        "err": ph["err"],
        "spread": ph["spread_after"],
        "lag": ph["lag_after"],
        "pass_tight": ph["pass_tight"],
        "exec_delta": ph["exec_delta"],
        "pack_skip_reasons": ps.get("pack_skip_reasons") or m.get("pack_skip_reasons"),
        "own_open_n": ps.get("own_open_n"),
        "batches_created": m.get("batches_created"),
        "certified": m.get("certified"),
        "liveness_policy": ps.get("liveness_policy") or m.get("liveness_policy"),
    }
    print(
        f"  cap={cap} exec_tps_min={snap['exec_tps_min']} submit={snap['submit_tps']} "
        f"spread={snap['spread']} batches={snap['batches_created']} tight={snap['pass_tight']}",
        flush=True,
    )
    (OUT / f"cap_{cap}.json").write_text(json.dumps(snap, indent=2) + "\n")
    sc.stop_all()
    return snap


def main():
    print(f"OUT={OUT} F1={sc.F1} F2={sc.F2} CAPS={CAPS}", flush=True)
    rows = []
    for cap in CAPS:
        try:
            rows.append(run_cell(cap))
        except Exception as e:
            print(f"cap={cap} FAIL {e}", flush=True)
            rows.append({"cap": cap, "error": str(e)})
            sc.stop_all()

    best = max(
        (r for r in rows if "error" not in r),
        key=lambda r: r.get("exec_tps_min") or 0,
        default=None,
    )
    report = {
        "suite": "pack_open_cap_dig",
        "rows": rows,
        "best_cap": None if not best else best.get("cap"),
        "best_exec_tps_min": None if not best else best.get("exec_tps_min"),
    }
    (OUT / "PACK_OPEN_DIG.json").write_text(json.dumps(report, indent=2) + "\n")
    lines = [
        "# pack_open_cap dig (N=6 multipack, 50/s × 25s)",
        "",
        f"OUT: `{OUT}`",
        "",
        "| cap | exec_tps_min | submit_tps | spread | batches(val1) | tight |",
        "|-----|--------------|------------|--------|---------------|-------|",
    ]
    for r in rows:
        if "error" in r:
            lines.append(f"| {r['cap']} | ERROR | {r['error'][:40]} | | | |")
        else:
            lines.append(
                f"| {r['cap']} | **{r['exec_tps_min']}** | {r['submit_tps']} | {r['spread']} | "
                f"{r.get('batches_created')} | {r['pass_tight']} |"
            )
    lines += [
        "",
        f"**Best:** cap={report['best_cap']} exec_tps_min={report['best_exec_tps_min']}",
        "",
        "Note: cap=0 uses binary auto multipack open depth (code default now 2).",
        "",
    ]
    text = "\n".join(lines) + "\n"
    (OUT / "PACK_OPEN_DIG.md").write_text(text)
    (ROOT / "PACK_OPEN_DIG.md").write_text(text)
    print(text, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
