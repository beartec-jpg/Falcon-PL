#!/usr/bin/env python3
"""MOCK falcon-pl-ctl for the V3 anvil-fork dry run. Not a node.

Used only by v3-fork-dry-run.py, which points FPL_CTL here so the real
pl-eth-auto-mint.py and pl-eth-claim-proof.py can run against an anvil fork
with no FPL node. It never opens a socket and never signs anything.

State lives in the JSON file named by FPL_V3_DRY_MOCK_STATE (the driver writes
`status`, `amounts`, and `claim_status`). Commands:

  --addr X status                    -> state["status"] (or state["claim_status"]
                                        when FPL_V3_DRY_MOCK_STATUS=claim)
  --addr X account --account NAME    -> {"sequence": n, "assets": {...}}
  --addr X submit-eth-v3-deposit ... -> records the call (args plus the
                                        receipt and MPT files it points at) under
                                        state["submissions"]. A deposit id it has
                                        already recorded is refused with the
                                        node's admission text "V3 deposit already
                                        resolved". A mint (non-empty --credit-to)
                                        bumps that account's mock balance by the
                                        deposit amount so auto-mint's credit wait
                                        returns. Real mint/refund decisions are
                                        made later by the Rust consensus replay.

Anything else exits 2.
"""
from __future__ import annotations

import fcntl
import json
import os
import sys
from pathlib import Path

STATE = Path(os.environ.get("FPL_V3_DRY_MOCK_STATE", ""))


def load() -> dict:
    return json.loads(STATE.read_text())


def save(st: dict) -> None:
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(st, indent=2) + "\n")
    tmp.replace(STATE)


def opt(args: list[str], name: str, default: str = "") -> str:
    if name in args:
        i = args.index(name)
        if i + 1 < len(args):
            return args[i + 1]
    return default


def main(argv: list[str]) -> int:
    if not STATE.name:
        print("mock ctl: FPL_V3_DRY_MOCK_STATE unset", file=sys.stderr)
        return 2
    args = list(argv)
    if args[:1] == ["--addr"]:
        args = args[2:]
    if not args:
        print("mock ctl: no command", file=sys.stderr)
        return 2
    cmd, rest = args[0], args[1:]
    with open(str(STATE) + ".lock", "a+") as lk:
        fcntl.flock(lk.fileno(), fcntl.LOCK_EX)
        st = load()
        if cmd == "status":
            which = os.environ.get("FPL_V3_DRY_MOCK_STATUS", "")
            body = st.get("claim_status") if which == "claim" else st.get("status")
            print(json.dumps(body or {}))
            return 0
        if cmd == "account":
            name = opt(rest, "--account").strip().lower()
            seqs = st.setdefault("sequences", {})
            bals = st.setdefault("balances", {})
            print(json.dumps({"account": name, "sequence": int(seqs.get(name, 1)), "assets": bals.get(name, {})}))
            return 0
        if cmd == "submit-eth-v3-deposit":
            dep = opt(rest, "--deposit-id").lower().removeprefix("0x")
            account = opt(rest, "--account").strip().lower()
            credit = opt(rest, "--credit-to").strip().lower()
            asset = opt(rest, "--asset").upper()
            resolved = st.setdefault("resolved", {})
            if dep in resolved:
                st.setdefault("refused", []).append({"deposit_id": dep, "credit_to": credit})
                save(st)
                print("Error: submit rejected: V3 deposit already resolved", file=sys.stderr)
                return 1
            raw_file = opt(rest, "--raw-tx-file")
            path_file = opt(rest, "--merkle-path-file")
            rec = {
                "argv": rest,
                "account": account,
                "asset": asset,
                "deposit_id": dep,
                "credit_to": credit,
                "external_txid": opt(rest, "--external-txid"),
                "block_hash": opt(rest, "--block-hash"),
                "height": int(opt(rest, "--height", "0")),
                "merkle_index": int(opt(rest, "--merkle-index", "0")),
                "raw_tx": Path(raw_file).read_text().strip() if raw_file else "",
                "merkle_path": json.loads(Path(path_file).read_text()) if path_file else [],
                "phase": st.get("phase", ""),
            }
            st.setdefault("submissions", []).append(rec)
            resolved[dep] = "mint" if credit else "refund"
            seqs = st.setdefault("sequences", {})
            seqs[account] = int(seqs.get(account, 1)) + 1
            if credit:
                amt = int((st.get("amounts") or {}).get(dep, 0))
                bal = st.setdefault("balances", {}).setdefault(credit, {})
                bal[asset] = int(bal.get(asset, 0)) + amt
            save(st)
            print(f"submit-eth-v3-deposit tx=mock-{len(st['submissions'])} " + json.dumps({"ok": True, "mock": True}))
            return 0
    print(f"mock ctl: unsupported command {cmd!r}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
