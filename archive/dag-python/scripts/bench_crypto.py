#!/usr/bin/env python3
"""Compare HMAC-dev vs Falcon-512 sign/verify cost for Falcon DAG certs."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from falcon_dag.crypto import benchmark_crypto, falcon_available


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rounds", type=int, default=40)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    results = {"hmac-sha256-dev": benchmark_crypto("hmac-sha256-dev", args.rounds)}
    if falcon_available():
        results["falcon-512"] = benchmark_crypto("falcon-512", args.rounds)
    else:
        results["falcon-512"] = {"error": "falcon-python not installed"}

    if args.json:
        print(json.dumps(results, indent=2))
        return

    print("=== Falcon DAG crypto micro-benchmark ===\n")
    for name, r in results.items():
        if "error" in r:
            print(f"{name}: {r['error']}\n")
            continue
        print(f"## {name}")
        print(f"  keygen: {r['keygen_ms']} ms (one keypair)")
        print(
            f"  sign:   p50={r['sign']['p50_ms']} ms  p95={r['sign']['p95_ms']} ms  "
            f"mean={r['sign']['mean_ms']} ms"
        )
        print(
            f"  verify: p50={r['verify']['p50_ms']} ms  p95={r['verify']['p95_ms']} ms  "
            f"mean={r['verify']['mean_ms']} ms"
        )
        print(
            f"  sig:    mean={r['sig_bytes']['mean']} B  "
            f"[{r['sig_bytes']['min']}–{r['sig_bytes']['max']}]"
        )
        print()

    if "falcon-512" in results and "sign" in results["falcon-512"]:
        h = results["hmac-sha256-dev"]["sign"]["mean_ms"]
        f = results["falcon-512"]["sign"]["mean_ms"]
        if h > 0:
            print(f"Falcon-512 sign ≈ {f/h:.0f}× slower than HMAC-dev (mean)")
        print(
            "This is why unit tests default to HMAC-dev; regtest uses Falcon-512."
        )


if __name__ == "__main__":
    main()
