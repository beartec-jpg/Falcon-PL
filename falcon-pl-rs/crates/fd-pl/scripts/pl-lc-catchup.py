#!/usr/bin/env python3
"""Dump FPL headers after FalconBridge startHeight and sign them on each LC seat.

Secrets never leave the seat. Statements + sigs are public.
ETH/USDC claim_leaves are empty today, so historical claim_root is 32 zero bytes.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
BODIES = Path(os.environ.get("PL_BODIES", str(ROOT / "snapshots/bodies/v1")))
OUT = Path(os.environ.get("LC_CATCHUP_DIR", str(ROOT / "run/snark/catchup")))
START = int(os.environ.get("LC_START_HEIGHT", "116957"))
NETWORK_ID = int(os.environ.get("FPL_NETWORK_ID", "2300"))
SNARK = Path(
    os.environ.get(
        "FPL_SNARK",
        str(Path.home() / "falcon-pl/falcon-pl-rs/target/release/falcon-pl-snark"),
    )
)
SSH_OPTS = [
    "-o",
    "ConnectTimeout=15",
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=accept-new",
]
ZERO32 = "00" * 32

# (id, ssh, remote_root)
SEATS = [
    ("v1", "host1", "/home/falcon/falcon-pl-public-testnet-2300"),
    ("v2", "root@100.64.0.2", "/root/falcon-pl-public-testnet-2300"),
    ("v3", "host2", "/home/falcon/falcon-pl-public-testnet-2300"),
    ("v4", "root@100.64.0.4", "/root/falcon-pl-public-testnet-2300"),
    ("v5", "root@100.64.0.5", "/home/falcon/falcon-pl-public-testnet-2300"),
    ("v6", "root@100.64.0.6", "/root/falcon-pl-public-testnet-2300"),
]


def tip_from_bodies() -> int:
    env_tip = os.environ.get("LC_END_HEIGHT")
    if env_tip:
        return int(env_tip)
    best = START
    for p in BODIES.glob("h-*.hash"):
        try:
            h = int(p.name[2 : p.name.index(".")])
        except ValueError:
            continue
        if h > best:
            best = h
    return best


def dump_statements(start_h: int, end_h: int) -> list[dict]:
    rows = []
    for h in range(start_h, end_h + 1):
        hp = BODIES / f"h-{h}.hash"
        if not hp.exists():
            raise SystemExit(f"missing archive hash {hp}")
        header = hp.read_text().strip().lower()
        body = json.loads((BODIES / f"{header}.json").read_text())
        parent = str(body.get("parent_hash") or "").strip().lower()
        if body.get("height") != h:
            raise SystemExit(f"body height mismatch {h}")
        if str(body.get("ledger_hash") or "").strip().lower() != header:
            raise SystemExit(f"body hash mismatch {h}")
        rows.append(
            {
                "height": h,
                "parent": parent,
                "header": header,
                "claim_root": ZERO32,
                "network_id": NETWORK_ID,
            }
        )
    return rows


def rsync_to(host: str, src: Path, dst: str) -> None:
    subprocess.check_call(
        ["rsync", "-az", "-e", "ssh " + " ".join(SSH_OPTS), str(src), f"{host}:{dst}"]
    )


def rsync_from(host: str, src: str, dst: Path) -> None:
    dst.mkdir(parents=True, exist_ok=True)
    subprocess.check_call(
        ["rsync", "-az", "-e", "ssh " + " ".join(SSH_OPTS), f"{host}:{src}", str(dst)]
    )


def ssh(host: str, cmd: str) -> None:
    subprocess.check_call(["ssh", *SSH_OPTS, host, cmd])


def main() -> int:
    if not SNARK.exists():
        print("missing falcon-pl-snark", SNARK, file=sys.stderr)
        return 2
    end = tip_from_bodies()
    start = START + 1
    if end < start:
        print(f"nothing to sign start={start} end={end}")
        return 0
    print(f"dumping headers {start}..{end} ({end - start + 1})", flush=True)
    rows = dump_statements(start, end)
    OUT.mkdir(parents=True, exist_ok=True)
    stmt = OUT / "statements.json"
    stmt.write_text(json.dumps(rows) + "\n")
    pubs = ROOT / "keys" / "lc"
    collected = OUT / "sigs"
    collected.mkdir(parents=True, exist_ok=True)

    for vid, host, rroot in SEATS:
        print(f"sign {vid} on {host}", flush=True)
        rsync_to(host, SNARK, f"{rroot}/bin/falcon-pl-snark")
        ssh(host, f"mkdir -p {rroot}/run/snark/catchup {rroot}/keys/lc/sigs")
        rsync_to(host, stmt, f"{rroot}/run/snark/catchup/statements.json")
        ssh(
            host,
            f"{rroot}/bin/falcon-pl-snark sign-batch"
            f" --secret {rroot}/keys/lc/{vid}.json"
            f" --statements {rroot}/run/snark/catchup/statements.json"
            f" --out-dir {rroot}/keys/lc/sigs"
            f" --network-id {NETWORK_ID}",
        )
        rsync_from(host, f"{rroot}/keys/lc/sigs/", collected)

    # fan public sigs (not secrets) so a local prover can see ≥4
    for vid, host, rroot in SEATS:
        rsync_to(host, Path(str(collected) + "/"), f"{rroot}/keys/lc/sigs/")
    print(f"wrote {stmt} and collected {len(list(collected.glob('*.json')))} sig files", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
