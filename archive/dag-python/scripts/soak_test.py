#!/usr/bin/env python3
"""Short soak: continuous submit + check agreement across regtest validators."""

from __future__ import annotations

import argparse
import json
import time
import urllib.request


def rpc(port: int, method: str, params: dict | None = None) -> dict:
    body = json.dumps({"method": method, "params": [params or {}]}).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.loads(r.read())


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--duration", type=float, default=30.0)
    ap.add_argument("--rate", type=float, default=20.0, help="txs per second target")
    ap.add_argument("--accounts", type=int, default=10)
    args = ap.parse_args()

    ports = [18001, 18002, 18003, 18004, 18005]
    # Use high account range to avoid sequence clashes with prior load tests
    seq = {f"acct{i}": 1 for i in range(1, args.accounts + 1)}
    # probe free sequences quickly
    for a in list(seq.keys()):
        for s in range(1, 200):
            r = rpc(
                18001,
                "submit",
                {
                    "account": a,
                    "sequence": s,
                    "destination": "acct1",
                    "amount": 1,
                    "fee": 10,
                },
            )
            if r.get("result", {}).get("engine_result") == "tesSUCCESS":
                seq[a] = s + 1
                break
            seq[a] = s + 1

    t0 = time.time()
    submitted = 0
    errors = 0
    interval = 1.0 / max(args.rate, 0.1)
    i = 0
    print(f"soak {args.duration}s @ ~{args.rate} tx/s")
    while time.time() - t0 < args.duration:
        src = f"acct{(i % args.accounts) + 1}"
        dst = f"acct{((i + 1) % args.accounts) + 1}"
        s = seq[src]
        try:
            r = rpc(
                ports[i % 5],
                "submit",
                {
                    "account": src,
                    "sequence": s,
                    "destination": dst,
                    "amount": 1,
                    "fee": 10,
                },
            )
            if r.get("result", {}).get("engine_result") == "tesSUCCESS":
                seq[src] = s + 1
                submitted += 1
            else:
                seq[src] = s + 1
                errors += 1
        except Exception:
            errors += 1
        i += 1
        time.sleep(interval)

    time.sleep(3)
    infos = [rpc(p, "server_info")["result"]["info"] for p in ports]
    canons = [tuple(rpc(p, "canonical")["result"]["txs"]) for p in ports]
    agree = all(c == canons[0] for c in canons)
    schemes = [inf["metrics"]["da"].get("crypto_scheme") for inf in infos]
    execs = [inf["metrics"]["execution"]["executed_txs"] for inf in infos]
    rejs = [inf["metrics"]["da"].get("rejected_batches", 0) for inf in infos]
    equiv = [inf["metrics"]["da"].get("equivocations", 0) for inf in infos]

    report = {
        "duration_s": round(time.time() - t0, 2),
        "submitted_ok": submitted,
        "submit_errors": errors,
        "agree_canonical": agree,
        "executed": execs,
        "crypto_schemes": schemes,
        "rejected_batches": rejs,
        "equivocations": equiv,
        "tps_submit": round(submitted / max(time.time() - t0, 0.1), 1),
    }
    print(json.dumps(report, indent=2))
    if not agree:
        raise SystemExit(1)
    print("SOAK OK")


if __name__ == "__main__":
    main()
