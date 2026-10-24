#!/usr/bin/env python3
"""
Rule-bound SPV rail agent — human-equivalent client, protocol rules only.

  - Pulls headers / lock proofs from a ChainIndexer (synthetic | live)
  - Submits SubmitRailHeader / RailDeposit only
  - tecPROTOCOL is final (never force-mints)
  - Forbidden Issue of rail asset is demonstrated as tecPROTOCOL

Usage:
  # Local smoke (synthetic indexer, Bitcoin-family merkle for BTC)
  python3 scripts/rail_agent.py --rpc http://127.0.0.1:18811 --rail BTC --indexer synthetic

  # Live headers only (public APIs; deposits when watch addr / contract configured)
  python3 scripts/rail_agent.py --rpc http://127.0.0.1:18811 --rail BTC --indexer live \\
      --btc-api https://blockstream.info/api --max-headers 3
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.request
from pathlib import Path

# allow `scripts/` as import root when run as scripts/rail_agent.py
sys.path.insert(0, str(Path(__file__).resolve().parent))

from rail_indexers.factory import make_indexer  # noqa: E402


def rpc(url: str, method: str, params: dict) -> dict:
    body = json.dumps({"method": method, "params": [params]}).encode()
    req = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode())


def submit(url: str, tx: dict) -> dict:
    res = rpc(url, "submit", tx).get("result", {})
    if not isinstance(res, dict):
        raise RuntimeError(f"submit non-object result: {res!r}")
    return res


def require_engine(res: dict, label: str, expect: str | None = "tesSUCCESS") -> str:
    """Fail-closed: missing/None engine_result is an error (CI race / dead node)."""
    code = res.get("engine_result") if isinstance(res, dict) else None
    if code is None or code == "" or str(code).lower() == "none":
        print(f"FAIL {label}: missing engine_result: {res!r}", file=sys.stderr)
        sys.exit(10)
    if expect is not None and code != expect:
        print(f"FAIL {label}: expected {expect} got {code}: {res!r}", file=sys.stderr)
        sys.exit(11)
    return str(code)


def wait_ok(url: str, timeout: float = 8.0) -> None:
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            if rpc(url, "ping", {}).get("result", {}).get("pong"):
                return
        except Exception:
            pass
        time.sleep(0.1)
    raise SystemExit(f"RPC not ready: {url}")


def account_seq(url: str, account: str) -> int:
    return int(rpc(url, "account_info", {"account": account}).get("result", {}).get("sequence", 1))


def rail_info(url: str, rail: str) -> dict:
    return rpc(url, "rail_info", {"rail": rail}).get("result", {})


def main() -> int:
    ap = argparse.ArgumentParser(description="Protocol-rule SPV rail agent")
    ap.add_argument("--rpc", default="http://127.0.0.1:18811")
    ap.add_argument("--rail", default="BTC", choices=["BTC", "ETH", "BNB", "XRP"])
    ap.add_argument("--account", default="acct1")
    ap.add_argument("--recipient", default="acct2")
    ap.add_argument("--fee", type=int, default=1)
    ap.add_argument("--indexer", default="synthetic", choices=["synthetic", "live", "watch"])
    ap.add_argument("--amount", type=int, default=10_000)
    ap.add_argument("--max-headers", type=int, default=32)
    ap.add_argument("--btc-api", default="https://blockstream.info/api")
    ap.add_argument(
        "--btc-watch-address",
        default=os.environ.get("BTC_WATCH", ""),
        help="BTC address for live SPV deposits. Env: BTC_WATCH",
    )
    ap.add_argument(
        "--eth-rpc",
        default=os.environ.get("ETH_RPC", ""),
        help="ETH JSON-RPC URL or comma-list; default multi-endpoint failover. Env: ETH_RPC",
    )
    ap.add_argument(
        "--bnb-rpc",
        default=os.environ.get("BNB_RPC", ""),
        help="BNB JSON-RPC URL or comma-list; default multi-endpoint failover. Env: BNB_RPC",
    )
    ap.add_argument("--xrp-rpc", default="https://xrplcluster.com")
    ap.add_argument(
        "--xrp-watch-account",
        default=os.environ.get("XRP_WATCH", ""),
        help="XRPL classic address receiving protocol Payment locks. Env: XRP_WATCH",
    )
    ap.add_argument(
        "--lock-contract",
        default=os.environ.get("ETH_LOCK", "") or os.environ.get("BNB_LOCK", ""),
        help="EVM FalconRailLock address. Env: ETH_LOCK or BNB_LOCK",
    )
    ap.add_argument(
        "--max-deposits",
        type=int,
        default=int(os.environ.get("MAX_DEPOSITS", "5")),
        help="Max live deposits to attempt (default 5)",
    )
    ap.add_argument("--sleep", type=float, default=0.35)
    ap.add_argument("--prove-forbidden", action="store_true", default=True)
    ap.add_argument(
        "--export-claims",
        action="store_true",
        help="after ops, print withdrawal_notes + claim roots for open notes",
    )
    args = ap.parse_args()

    wait_ok(args.rpc)
    info = rail_info(args.rpc, args.rail)
    if info.get("error"):
        print("rail_info error:", info, file=sys.stderr)
        return 1

    lock_id = info.get("lock_id") or ""
    print(
        json.dumps(
            {
                "agent": "rail_agent",
                "policy": "protocol_rules_only",
                "federated_mint": False,
                "committee": None,
                "rail": args.rail,
                "lock_id": lock_id,
                "falcon_tip": info.get("tip_height"),
                "indexer": args.indexer,
                "mint_path": info.get("mint_path"),
                "btc_watch": args.btc_watch_address or None,
                "xrp_watch": args.xrp_watch_account or None,
                "lock_contract": args.lock_contract or None,
            },
            indent=2,
        )
    )

    if args.indexer == "watch":
        print("watch: indexer idle; configure live source and re-run with --indexer live")
        return 0

    mode = "synthetic" if args.indexer == "synthetic" else "live"
    # Per-rail lock contract: ETH_LOCK vs BNB_LOCK when both set
    lock_contract = args.lock_contract
    if mode == "live" and args.rail == "ETH" and os.environ.get("ETH_LOCK"):
        lock_contract = os.environ["ETH_LOCK"]
    if mode == "live" and args.rail == "BNB" and os.environ.get("BNB_LOCK"):
        lock_contract = os.environ["BNB_LOCK"]

    indexer = make_indexer(
        args.rail,
        mode=mode,
        lock_id=lock_id,
        amount=args.amount,
        btc_api=args.btc_api,
        btc_watch_address=args.btc_watch_address,
        eth_rpc=args.eth_rpc,
        bnb_rpc=args.bnb_rpc,
        xrp_rpc=args.xrp_rpc,
        xrp_watch_account=args.xrp_watch_account,
        lock_contract=lock_contract,
    )

    falcon_tip = int(info.get("tip_height") or 0)
    seq = account_seq(args.rpc, args.account)
    bootstrap = mode == "live" and falcon_tip == 0
    # Need enough headers beyond deposits for min_confirmations when watching
    min_conf = int(info.get("min_confirmations") or 6)
    header_limit = args.max_headers
    watching = bool(
        args.btc_watch_address
        or args.xrp_watch_account
        or lock_contract
    )
    if mode == "live" and watching:
        header_limit = max(header_limit, min_conf + 3)

    # 1) Advance light client with indexer headers only
    try:
        headers = indexer.headers_after(
            falcon_tip, limit=header_limit, bootstrap_near_tip=bootstrap
        )
    except Exception as e:
        print(f"indexer headers error: {e}", file=sys.stderr)
        return 2

    if mode == "live" and hasattr(indexer, "rpc_url"):
        print(json.dumps({"active_rpc": getattr(indexer, "rpc_url", None), "rail": args.rail}))

    for hdr in headers:
        tx = {
            "kind": "submit_rail_header",
            "account": args.account,
            "sequence": seq,
            "asset": args.rail,
            "fee": args.fee,
            "bridge": hdr.as_bridge(),
        }
        res = submit(args.rpc, tx)
        try:
            code = require_engine(res, f"header h={hdr.height}", expect=None)
        except SystemExit:
            if mode == "synthetic":
                raise
            print(f"header h={hdr.height} -> missing result (live soft)", res)
            continue
        print(f"header h={hdr.height} -> {code}")
        if code == "tesSUCCESS":
            seq += 1
            falcon_tip = hdr.height
            time.sleep(args.sleep)
        elif code == "tecPROTOCOL":
            print("protocol rejected header; agent stops (no override)", file=sys.stderr)
            if mode == "synthetic":
                return 3
        else:
            print(f"unexpected: {res}", file=sys.stderr)
            if mode == "synthetic":
                return 3

    # 2) Deposits from indexer (empty on live without watch/lock config)
    try:
        # only consider deposits already under Falcon tip window
        info2 = rail_info(args.rpc, args.rail)
        tip_now = int(info2.get("tip_height") or 0)
        min_conf = int(info2.get("min_confirmations") or 6)
        after_h = max(0, tip_now - header_limit - 1) if tip_now else 0
        deps = indexer.pending_deposits(lock_id, after_height=after_h)
    except Exception as e:
        print(f"indexer deposits error: {e}", file=sys.stderr)
        deps = []

    print(
        json.dumps(
            {
                "deposit_candidates": len(deps),
                "watching": watching,
                "max_deposits": args.max_deposits,
            }
        )
    )
    attempted = 0
    minted = 0
    skipped_conf = 0
    rejected = 0
    for dep in deps:
        if attempted >= args.max_deposits:
            print(f"max-deposits {args.max_deposits} reached")
            break
        info2 = rail_info(args.rpc, args.rail)
        tip = int(info2.get("tip_height") or 0)
        min_conf = int(info2.get("min_confirmations") or 6)
        if tip < dep.height + min_conf:
            print(
                f"skip deposit {str(dep.external_txid)[:20]}… "
                f"need tip>={dep.height + min_conf} have {tip}"
            )
            skipped_conf += 1
            continue
        bridge = dep.as_bridge()
        tx = {
            "kind": "rail_deposit",
            "account": args.account,
            "sequence": seq,
            "asset": args.rail,
            "destination": args.recipient,
            "amount": dep.amount,
            "fee": args.fee,
            "bridge": bridge,
        }
        res = submit(args.rpc, tx)
        attempted += 1
        code = require_engine(res, f"deposit {str(dep.external_txid)[:16]}", expect=None)
        print(f"deposit amt={dep.amount} tx={str(dep.external_txid)[:20]}… -> {code}")
        if code == "tesSUCCESS":
            seq += 1
            minted += 1
            time.sleep(args.sleep)
        elif code == "tecPROTOCOL":
            rejected += 1
            print("deposit rejected by protocol (agent cannot force mint)")
        else:
            print(res)
            if mode == "synthetic":
                return 12
    if mode == "live":
        print(
            json.dumps(
                {
                    "deposits_attempted": attempted,
                    "deposits_minted": minted,
                    "deposits_skipped_conf": skipped_conf,
                    "deposits_rejected": rejected,
                }
            )
        )

    if mode == "synthetic" and deps:
        time.sleep(0.3)
        bal = (
            rpc(args.rpc, "account_info", {"account": args.recipient})
            .get("result", {})
            .get("balances", {})
            .get(args.rail, 0)
        )
        print(json.dumps({"recipient": args.recipient, "balance": bal, "rail": args.rail}))

    if args.prove_forbidden:
        bad = {
            "kind": "issue",
            "account": args.account,
            "sequence": account_seq(args.rpc, args.account),
            "asset": args.rail,
            "amount": 1,
            "destination": args.account,
            "fee": args.fee,
        }
        bad_res = submit(args.rpc, bad)
        code = require_engine(bad_res, f"forbidden issue {args.rail}", expect=None)
        print(f"forbidden issue {args.rail} -> {code} (expect tecPROTOCOL)")
        if code == "tesSUCCESS":
            print("FATAL: discretionary mint accepted", file=sys.stderr)
            return 4

    if args.export_claims:
        notes = rpc(args.rpc, "withdrawal_notes", {"rail": args.rail})
        print("withdrawal_notes:", json.dumps(notes, indent=2))
        ri = rail_info(args.rpc, args.rail)
        print("claim_root:", ri.get("claim_root"), "open:", ri.get("open_withdrawals"))

    print("rail_agent OK (rules only)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
