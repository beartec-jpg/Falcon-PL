#!/usr/bin/env python3
"""BRIDGE depositor refusal rule (mirror of fpl-operatord check-deposit).

Before broadcasting a peg-in CONFIRM, the depositor checks that EVERY operator
peg-out graph in final.json is bound on the Falcon ledger by the account that
registered that operator's BTC key (Active), and that the bound KickOff2 /
AssertFinal txids equal the graph's. Otherwise: do not broadcast.

  check_graph_bindings.py <graph dir | final.json> [--ctl falcon-pl-ctl --rpc URL | --status-json FILE]
Exit 0 = OK (or bridge not active yet: rule not enforced), 2 = REFUSE, 1 = error.
Read-only: runs `falcon-pl-ctl status`; no keys involved.
"""
import argparse, hashlib, json, os, subprocess, sys


def _vi(b, i):
    x = b[i]
    if x < 0xfd:
        return x, i + 1
    n = {0xfd: 2, 0xfe: 4, 0xff: 8}[x]
    return int.from_bytes(b[i + 1:i + 1 + n], "little"), i + 1 + n


def txid(tx_hex):
    b = bytes.fromhex(tx_hex.strip())
    if len(b) > 6 and b[4] == 0 and b[5] == 1:  # segwit: txid excludes marker/flag/witness
        i = s = 6
        n, i = _vi(b, i)
        for _ in range(n):
            i += 36
            l, i = _vi(b, i)
            i += l + 4
        n, i = _vi(b, i)
        for _ in range(n):
            i += 8
            l, i = _vi(b, i)
            i += l
        b = b[:4] + b[s:i] + b[-4:]
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()[::-1].hex()


def check(final, status):
    bo = status.get("body", status).get("bridge_operators", {})
    graphs = final.get("peg_out_graphs") or []
    if not graphs:
        raise ValueError("no peg-out graphs in final.json")
    ops = bo.get("operators") or {}
    dep0 = None
    for i, g in enumerate(graphs):
        opk = (g.get("operator_public_key") or "").lower()
        x = opk[2:] if len(opk) == 66 else opk
        dep = (g.get("peg_in_confirm_txid") or "").lower()
        if len(x) != 64 or len(dep) != 64:
            raise ValueError(f"graph {i}: missing operator key or peg_in_confirm_txid")
        dep0 = dep0 or dep
        if dep != dep0:
            raise ValueError(f"graph {i}: different peg-in confirm txid")
        k2 = txid(g["kick_off_2_transaction"]["tx"])
        af = txid(g["assert_final_transaction"]["tx"])
        acct = next((a for a, r in ops.items() if (r.get("btc_xonly_hex") or "").lower() == x), None)
        if acct is None:
            raise ValueError(f"graph {i}: operator {x} not registered on the ledger")
        if ops[acct].get("status") != "Active":
            raise ValueError(f"graph {i}: operator {x} not active ({ops[acct].get('status')})")
        b = (bo.get("graphs") or {}).get(f"{dep}:{x}")
        if b is None:
            raise ValueError(f"graph {i}: operator {x} has not bound this graph on the ledger")
        if b.get("account") != acct:
            raise ValueError(f"graph {i}: binding posted by {b.get('account')} not by registered account {acct}")
        if (b.get("kick_off_2_txid") or "").lower() != k2 or (b.get("assert_final_txid") or "").lower() != af:
            raise ValueError(f"graph {i}: on-ledger binding does not match final.json (kick_off_2 {k2}, assert_final {af})")
    return len(graphs)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("final")
    ap.add_argument("--ctl", default="falcon-pl-ctl")
    ap.add_argument("--rpc", default="")
    ap.add_argument("--status-json", default="")
    a = ap.parse_args()
    p = os.path.join(a.final, "final.json") if os.path.isdir(a.final) else a.final
    try:
        final = json.load(open(p))
        if a.status_json:
            status = json.load(open(a.status_json))
        else:
            cmd = [a.ctl] + (["--addr", a.rpc] if a.rpc else []) + ["status"]
            status = json.loads(subprocess.check_output(cmd, text=True))
    except Exception as e:  # noqa: BLE001
        print(f"error: {e}", file=sys.stderr)
        return 1
    if not status.get("body", status).get("bridge_operators", {}).get("active"):
        print("bridge not active on this node: binding rule not enforced yet")
        return 0
    try:
        n = check(final, status)
    except (ValueError, KeyError) as e:
        print(f"REFUSE: {e}")
        return 2
    print(f"OK: all {n} operator graphs bound on the ledger and matching")
    return 0


if __name__ == "__main__":
    sys.exit(main())
