#!/usr/bin/env python3
"""PR10 soak helper: sample mesh vs V2 fplTip lag. One-host is lagged unwrap.

Does not HARD-fail lag. Prints slope so operators can file an IVC RFC only if
keep-up fails — do not reintroduce jumps.
"""
from __future__ import annotations

import json
import os
import subprocess
import time
from pathlib import Path

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = Path(os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl")))
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
FPL_RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
ETH_RPC = os.environ.get("ETH_RPC", "https://ethereum-sepolia-rpc.publicnode.com")
BRIDGE = os.environ.get("FALCON_QC_BRIDGE", os.environ.get("FALCON_QC_V2_BRIDGE", "")).strip()
SAMPLES = int(os.environ.get("FPL_LAG_SAMPLES", "12"))
INTERVAL = int(os.environ.get("FPL_LAG_INTERVAL", "60"))
OUT = Path(os.environ.get("FPL_LAG_OUT", str(ROOT / "run/snark/falcon-qc-v2-prover/lag-slope.json")))


def mesh() -> int:
    raw = subprocess.check_output([str(CTL), "--addr", FPL_RPC, "status"], text=True, timeout=20)
    body = json.loads(raw[raw.find("{") :]).get("body") or {}
    return int(body.get("tip_height") or 0)


def eth() -> int:
    if not BRIDGE:
        return 0
    out = subprocess.check_output(
        [str(CAST), "call", BRIDGE, "fplTip()(uint64)", "--rpc-url", ETH_RPC],
        text=True,
        timeout=30,
    ).strip()
    tok = out.split()[0]
    return int(tok, 16) if tok.startswith("0x") else int(tok or "0")


def main() -> int:
    rows = []
    for i in range(SAMPLES):
        m, e = mesh(), eth()
        lag = max(0, m - e)
        rec = {"t": time.time(), "mesh": m, "eth": e, "lag": lag}
        rows.append(rec)
        print(f"sample {i+1}/{SAMPLES} mesh={m} eth={e} lag={lag}", flush=True)
        if i + 1 < SAMPLES:
            time.sleep(INTERVAL)
    if len(rows) >= 2:
        dt = max(1.0, rows[-1]["t"] - rows[0]["t"])
        slope = (rows[-1]["lag"] - rows[0]["lag"]) / dt * 3600
    else:
        slope = 0.0
    doc = {"samples": rows, "lag_per_hour": slope, "note": "positive slope = falling behind; 4-host target ~0"}
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"lag_per_hour={slope:.1f} wrote {OUT}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
