#!/usr/bin/env python3
"""Print FalconQcBridgeV2 constructor fields from live ctl status.

Does not deploy. Exits 2 if v4 tip fields are missing (qc_start_height not armed).
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

CTL = Path(os.environ.get("FPL_CTL", "/home/falcon/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl"))
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
NEED = [
    "tip_height",
    "tip_hash",
    "inner_hash",
    "claim_root",
    "next_committee_root",
    "next_committee_leaves",
]


def main() -> int:
    raw = subprocess.check_output([str(CTL), "--addr", RPC, "status"], text=True, timeout=30)
    body = json.loads(raw[raw.find("{") :]).get("body") or {}
    missing = []
    for k in NEED:
        v = body.get(k)
        if v in (None, "", [], 0) and k != "tip_height":
            missing.append(k)
        if k == "tip_height" and int(v or 0) <= 0:
            missing.append(k)
    leaves = body.get("next_committee_leaves") or []
    rec = {
        "tip_height": body.get("tip_height"),
        "qc_header_version": body.get("qc_header_version"),
        "qc_start_height": body.get("qc_start_height"),
        "inner_hash": body.get("inner_hash") or "",
        "claim_root": (body.get("claim_root") or "")[:16],
        "next_committee_root": (body.get("next_committee_root") or "")[:16],
        "next_committee_leaves_n": len(leaves) if isinstance(leaves, list) else 0,
        "sealed_committee": body.get("sealed_committee") or [],
        "missing": missing,
    }
    print(json.dumps(rec, indent=2))
    if missing:
        print("not ready for V2 ctor — need a v4 tip (arm qc_start_height after pk.bin)", file=sys.stderr)
        return 2
    if len(leaves) != 6:
        print(f"need 6 next_committee_leaves, got {len(leaves)}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
