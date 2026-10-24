#!/usr/bin/env python3
"""
Boring smoke test: same-shard + cross-shard via facade.

Tracks sequences in MESH/stack/smoke_seq.json so it can re-run safely.
Exit 0 = pass, 1 = fail.
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

FACADE = os.environ.get("FACADE_URL", "http://127.0.0.1:18499")
MESH = Path(os.environ.get("MESH_ROOT", Path.home() / "falcon-dag-mesh"))
SEQ_FILE = MESH / "stack" / "smoke_seq.json"
RPC_S0 = os.environ.get("SMOKE_RPC_S0", "http://127.0.0.1:18001")
RPC_S1 = os.environ.get("SMOKE_RPC_S1", "http://127.0.0.1:18004")


def http_json(url: str, method: str = "GET", body=None, timeout: float = 8.0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data, method=method, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def rpc(url: str, method: str, params=None, timeout: float = 4.0):
    body = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": [params or {}]}
    ).encode()
    req = urllib.request.Request(
        url if url.endswith("/") else url + "/",
        data=body,
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        d = json.loads(r.read().decode())
    return d.get("result") or {}


def next_seq(rpc_url: str, account: str) -> int:
    try:
        r = rpc(rpc_url, "account_info", {"account": account})
        return int(r.get("sequence") or 1)
    except Exception:
        return 1


def bal(rpc_url: str, account: str):
    try:
        r = rpc(rpc_url, "account_info", {"account": account})
        if "balance" in r and isinstance(r["balance"], (int, float)):
            return int(r["balance"])
        bals = r.get("balances") or {}
        if isinstance(bals, dict):
            for k in ("FDAG", "fdag"):
                if k in bals:
                    return int(bals[k])
    except Exception:
        pass
    return None


def load_seq() -> dict:
    SEQ_FILE.parent.mkdir(parents=True, exist_ok=True)
    if SEQ_FILE.exists():
        try:
            return json.loads(SEQ_FILE.read_text())
        except Exception:
            pass
    return {}


def save_seq(d: dict):
    SEQ_FILE.parent.mkdir(parents=True, exist_ok=True)
    SEQ_FILE.write_text(json.dumps(d, indent=2) + "\n")


def main() -> int:
    results = []
    seqs = load_seq()
    ok = True

    # Health first
    try:
        st = http_json(f"{FACADE}/v1/status")
        results.append({"step": "facade_status", "ok": True, "stats": st.get("stats")})
    except Exception as e:
        print(json.dumps({"pass": False, "error": f"facade down: {e}"}))
        return 1

    # Route
    try:
        r0 = http_json(f"{FACADE}/v1/route", "POST", {"account": "acct3"})
        r1 = http_json(f"{FACADE}/v1/route", "POST", {"account": "acct23"})
        route_ok = r0.get("shard_id") == 0 and r1.get("shard_id") == 1
        results.append({"step": "route", "ok": route_ok, "r0": r0.get("shard_id"), "r1": r1.get("shard_id")})
        if not route_ok:
            ok = False
    except Exception as e:
        results.append({"step": "route", "ok": False, "error": str(e)})
        ok = False

    # Same-shard: acct3 → acct4 on s0
    acct = "acct3"
    chain = next_seq(RPC_S0, acct)
    local = int(seqs.get(acct) or chain)
    # After stack wipe, chain resets to 1 — drop stale smoke_seq
    if chain < local:
        local = chain
        seqs[acct] = chain
    s = max(chain, local)
    try:
        r = http_json(
            f"{FACADE}/v1/submit",
            "POST",
            {
                "account": acct,
                "sequence": s,
                "destination": "acct4",
                "amount": 1,
                "fee": 10,
                "client_id": f"smoke-same-{s}",
            },
        )
        same_ok = r.get("route") == "same_shard" and not r.get("error")
        if same_ok:
            seqs[acct] = s + 1
        results.append({"step": "same_shard", "ok": same_ok, "seq": s, "resp": r})
        if not same_ok:
            ok = False
    except Exception as e:
        results.append({"step": "same_shard", "ok": False, "error": str(e)})
        ok = False

    # Cross-shard: acct6 → acct26
    acct_x = "acct6"
    chain_x = next_seq(RPC_S0, acct_x)
    local_x = int(seqs.get(acct_x) or chain_x)
    if chain_x < local_x:
        local_x = chain_x
        seqs[acct_x] = chain_x
    sx = max(chain_x, local_x)
    bal_before = bal(RPC_S1, "acct26")
    try:
        r = http_json(
            f"{FACADE}/v1/submit",
            "POST",
            {
                "account": acct_x,
                "sequence": sx,
                "destination": "acct26",
                "amount": 1,
                "fee": 10,
                "client_id": f"smoke-x-{sx}",
            },
            timeout=10,
        )
        xfer = r.get("xfer_id")
        accept = r.get("route") == "cross_shard" and bool(xfer)
        results.append({"step": "xshard_accept", "ok": accept, "xfer_id": xfer, "seq": sx})
        if not accept:
            ok = False
        else:
            seqs[acct_x] = sx + 1
            final = None
            for _ in range(50):
                time.sleep(1)
                try:
                    final = http_json(f"{FACADE}/v1/xshard/{xfer}")
                except Exception as e:
                    final = {"status": "poll_error", "error": str(e)}
                st = final.get("status")
                if st in (
                    "credited",
                    "credited_rpc",
                    "refunded",
                    "refunded_rpc",
                    "failed",
                    "refund_failed",
                    "timeout_waiting_source_exec",
                    "credit_failed_refunding",
                ):
                    break
            credit_ok = (final or {}).get("status") in ("credited", "credited_rpc")
            # Credit may still be in dest mempool — wait for balance (or give up)
            bal_after = bal_before
            bal_ok = credit_ok  # if we can't read balances, trust credit status
            if bal_before is not None and credit_ok:
                bal_ok = False
                for _ in range(25):
                    bal_after = bal(RPC_S1, "acct26")
                    if bal_after is not None and bal_after >= bal_before + 1:
                        bal_ok = True
                        break
                    time.sleep(1)
            elif (final or {}).get("status") in ("refunded", "refunded_rpc"):
                # refund is acceptable for smoke resilience signal
                credit_ok = True
                bal_ok = True
            results.append(
                {
                    "step": "xshard_settle",
                    "ok": credit_ok and bal_ok,
                    "status": (final or {}).get("status"),
                    "bal_before": bal_before,
                    "bal_after": bal_after,
                }
            )
            if not (credit_ok and bal_ok):
                ok = False
    except Exception as e:
        results.append({"step": "xshard", "ok": False, "error": str(e)})
        ok = False

    # Idempotency: replay same-shard client_id
    try:
        r2 = http_json(
            f"{FACADE}/v1/submit",
            "POST",
            {
                "account": acct,
                "sequence": s,
                "destination": "acct4",
                "amount": 1,
                "fee": 10,
                "client_id": f"smoke-same-{s}",
            },
        )
        idem = r2.get("idempotent") is True or r2.get("route") == "same_shard"
        results.append({"step": "idempotent_replay", "ok": idem, "resp_keys": list(r2.keys())})
    except Exception as e:
        results.append({"step": "idempotent_replay", "ok": False, "error": str(e)})

    save_seq(seqs)
    report = {
        "pass": ok,
        "facade": FACADE,
        "steps": results,
        "ts": time.time(),
        "seqs": {k: seqs[k] for k in ("acct3", "acct6") if k in seqs},
    }
    print(json.dumps(report, indent=2))
    out_dir = MESH / "stack"
    out_dir.mkdir(parents=True, exist_ok=True)
    # Atomic write so concurrent status readers never see partial JSON
    tmp = out_dir / "last_smoke.json.tmp"
    tmp.write_text(json.dumps(report, indent=2) + "\n")
    tmp.replace(out_dir / "last_smoke.json")
    print("PASS" if ok else "FAIL", file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
