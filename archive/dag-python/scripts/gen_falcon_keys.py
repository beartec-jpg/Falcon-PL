#!/usr/bin/env python3
"""Generate Falcon-512 UNL keys for Falcon DAG regtest."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from falcon_dag.crypto import Falcon512KeyRing, falcon_available


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--out",
        default=str(ROOT / "data" / "regtest" / "keys"),
        help="Directory for unl_public.json + valN.json",
    )
    ap.add_argument("--validators", type=int, default=5)
    args = ap.parse_args()

    if not falcon_available():
        print("ERROR: falcon-python not installed. pip install falcon-python", file=sys.stderr)
        sys.exit(1)

    ids = [f"val{i+1}" for i in range(args.validators)]
    ring = Falcon512KeyRing.generate_unl(ids)
    ring.save_dir(args.out)
    m = ring.metrics()
    print(f"Wrote Falcon-512 keys for {ids} → {args.out}")
    print(f"  scheme={m['scheme']} pk={m['public_key_bytes']}B sk={m['secret_key_bytes']}B")
    print(f"  sample_sig={m['sample_sig_bytes']}B")
    print(f"  public UNL: {args.out}/unl_public.json")


if __name__ == "__main__":
    main()
