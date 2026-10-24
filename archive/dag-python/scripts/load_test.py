#!/usr/bin/env python3
"""Submit payments into Falcon DAG regtest."""

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
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18001)
    ap.add_argument("--txs", type=int, default=100)
    ap.add_argument("--accounts", type=int, default=10)
    ap.add_argument("--wait", type=float, default=6.0)
    args = ap.parse_args()

    seq = {f"acct{i}": 1 for i in range(1, args.accounts + 1)}
    ok = 0
    t0 = time.time()
    for i in range(args.txs):
        src = f"acct{(i % args.accounts) + 1}"
        dst = f"acct{((i + 1) % args.accounts) + 1}"
        s = seq[src]
        seq[src] = s + 1
        r = rpc(
            args.port if i % 2 == 0 else 18001 + (i % 5),
            "submit",
            {"account": src, "sequence": s, "destination": dst, "amount": 1, "fee": 10},
        )
        if r.get("result", {}).get("engine_result") == "tesSUCCESS":
            ok += 1
    print(f"Submitted {ok}/{args.txs} in {time.time()-t0:.2f}s; waiting {args.wait}s...")
    time.sleep(args.wait)
    info = rpc(args.port, "server_info")["result"]["info"]
    m = info["metrics"]
    print(json.dumps({"architecture": info.get("architecture"), "metrics": m}, indent=2))
    c = rpc(args.port, "canonical")["result"]
    print(f"canonical executed txs: {len(c.get('txs', []))}")
    print(f"ordered batches: {len(c.get('ordered_batches', []))}")


if __name__ == "__main__":
    main()
