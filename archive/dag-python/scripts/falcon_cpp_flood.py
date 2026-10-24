#!/usr/bin/env python3
"""Minimal Falcon-512 payment flood against local Falcon C++ xrpld regtest.

Funds N Falcon wallets from genesis (classical master still signs genesis),
then floods Payments signed with falcon_secret. Reports TPS.
"""

from __future__ import annotations

import argparse
import json
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed


def rpc(port: int, method: str, params: dict | None = None) -> dict:
    body = json.dumps({"method": method, "params": [params or {}]}).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())["result"]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=5005)
    ap.add_argument("--accounts", type=int, default=10)
    ap.add_argument("--payments", type=int, default=200, help="total flood payments")
    ap.add_argument("--fund-drops", type=int, default=50_000_000)
    args = ap.parse_args()
    port = args.port

    info = rpc(port, "server_info")
    print(
        "Falcon C++",
        info.get("build_version"),
        "state=",
        info.get("server_state"),
        "seq=",
        (info.get("validated_ledger") or {}).get("seq"),
    )

    # Genesis classical seed (well-known rippled master)
    genesis = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh"
    genesis_secret = "snoPBrXtMeMyMHUVTgbuqAfg1SUTb"

    wallets = []
    print(f"Creating {args.accounts} Falcon-512 wallets...")
    for i in range(args.accounts):
        w = rpc(port, "wallet_propose", {"key_type": "falcon512"})
        wallets.append(
            {
                "account": w["account_id"],
                "falcon_secret": w["falcon_secret"],
                "seq": 1,
            }
        )

    # Fund from genesis
    print("Funding wallets from genesis...")
    gseq = rpc(
        port,
        "account_info",
        {"account": genesis, "ledger_index": "current"},
    )["account_data"]["Sequence"]
    for i, w in enumerate(wallets):
        tx = {
            "TransactionType": "Payment",
            "Account": genesis,
            "Destination": w["account"],
            "Amount": str(args.fund_drops),
            "Fee": "12",
            "Sequence": gseq + i,
        }
        signed = rpc(port, "sign", {"secret": genesis_secret, "tx_json": tx})
        sub = rpc(port, "submit", {"tx_blob": signed["tx_blob"]})
        eng = sub.get("engine_result") or sub.get("engine_result_code")
        if eng not in ("tesSUCCESS", "terQUEUED", 0):
            print("fund fail", i, eng, sub.get("engine_result_message"))
    # Wait for funds to validate
    time.sleep(8)

    # Flood: each payment from wallet i -> i+1
    print(f"Flooding {args.payments} Falcon-signed payments...")
    t0 = time.perf_counter()
    ok = 0
    fail = 0

    def one_payment(k: int) -> bool:
        w = wallets[k % len(wallets)]
        dest = wallets[(k + 1) % len(wallets)]["account"]
        # sequence tracked naively — race under threads; use lock-free per-wallet lists
        return w, dest, k

    # Sequential for correct sequences (fair steady TPS)
    for k in range(args.payments):
        w = wallets[k % len(wallets)]
        dest = wallets[(k + 1) % len(wallets)]["account"]
        tx = {
            "TransactionType": "Payment",
            "Account": w["account"],
            "Destination": dest,
            "Amount": "1000",
            "Fee": "12",
            "Sequence": w["seq"],
        }
        try:
            signed = rpc(
                port,
                "sign",
                {"falcon_secret": w["falcon_secret"], "tx_json": tx},
            )
            sub = rpc(port, "submit", {"tx_blob": signed["tx_blob"]})
            eng = sub.get("engine_result", "")
            if eng in ("tesSUCCESS", "terQUEUED"):
                ok += 1
                w["seq"] += 1
            else:
                fail += 1
                # try refresh sequence
                try:
                    info = rpc(
                        port,
                        "account_info",
                        {"account": w["account"], "ledger_index": "current"},
                    )
                    w["seq"] = info["account_data"]["Sequence"]
                except Exception:
                    pass
        except Exception as e:
            fail += 1
            if fail < 3:
                print("err", e)

    submit_elapsed = time.perf_counter() - t0
    # Wait for ledgers to absorb
    seq0 = (rpc(port, "server_info").get("validated_ledger") or {}).get("seq", 0)
    time.sleep(10)
    seq1 = (rpc(port, "server_info").get("validated_ledger") or {}).get("seq", 0)
    total_elapsed = time.perf_counter() - t0

    report = {
        "system": "falcon-rpca-cpp",
        "mode": "falcon512-payment-flood",
        "accounts": args.accounts,
        "payments_requested": args.payments,
        "submit_ok": ok,
        "submit_fail": fail,
        "submit_elapsed_s": round(submit_elapsed, 3),
        "submit_tps": round(ok / submit_elapsed, 2) if submit_elapsed else 0,
        "total_elapsed_s": round(total_elapsed, 3),
        "settle_tps_approx": round(ok / total_elapsed, 2) if total_elapsed else 0,
        "ledgers_during": int(seq1) - int(seq0) if seq1 and seq0 else None,
        "notes": "Genesis funded with classical sig; flood uses Falcon-512 wallet signs via xrpld sign RPC.",
    }
    print(json.dumps(report, indent=2))
    return report


if __name__ == "__main__":
    main()
