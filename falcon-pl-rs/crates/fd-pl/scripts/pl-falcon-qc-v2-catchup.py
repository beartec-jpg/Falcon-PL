#!/usr/bin/env python3
"""v2 QC catch-up: rsync sigs-v2 from seats. Sequential only (every height).

Does not sample. Nodes dual-write v2 attestations on commit once qc_start_height
is armed. This script only collects them.
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
DST = Path(os.environ.get("FALCON_QC_SIGS", str(ROOT / "keys/falcon-qc/sigs-v2")))
SSH_OPTS = [
    "-o",
    "ConnectTimeout=15",
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=accept-new",
]
SEATS = [
    ("v1", "host1", "/home/falcon/falcon-pl-public-testnet-2300"),
    ("v2", "root@100.64.0.2", "/root/falcon-pl-public-testnet-2300"),
    ("v3", "host2", "/home/falcon/falcon-pl-public-testnet-2300"),
    ("v4", "root@100.64.0.4", "/root/falcon-pl-public-testnet-2300"),
    ("v5", "root@100.64.0.5", "/root/falcon-pl-public-testnet-2300"),
    ("v6", "root@100.64.0.6", "/root/falcon-pl-public-testnet-2300"),
]


def main() -> int:
    DST.mkdir(parents=True, exist_ok=True)
    for seat, host, root in SEATS:
        src = f"{root}/keys/falcon-qc/sigs-v2/"
        print(f"rsync {seat} {host}:{src}", flush=True)
        try:
            subprocess.check_call(
                ["rsync", "-az", "-e", "ssh " + " ".join(SSH_OPTS), f"{host}:{src}", str(DST) + "/"]
            )
        except subprocess.CalledProcessError as e:
            print(f"warn {seat}: {e}", flush=True)
    n = len(list(DST.glob("*.json")))
    print(f"sigs-v2 files={n} dir={DST}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
