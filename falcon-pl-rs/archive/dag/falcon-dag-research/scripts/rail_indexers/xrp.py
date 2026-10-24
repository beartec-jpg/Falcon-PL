"""XRPL live indexer — ledger headers + protocol Payment deposit trees.

Bind (same shape as EVM):
  deposit_root = merkle of Payments to watch account (or empty)
  tx_root      = ledger transaction_hash (or ledger_hash)
  header.merkle_root = sha256(tx_root‖deposit_root)

Leaf: sha256(tx_hash[32]‖amount_be8‖lock_id[32]‖sha256(destination)) — fd_exec::xrp_deposit_leaf
"""

from __future__ import annotations

import json
import urllib.request
from typing import Any

from .base import HeaderBatch, LockDeposit
from .merkle_util import (
    build_binary_merkle,
    empty_xrp_deposit_root,
    evm_bind_root,
    xrp_deposit_leaf,
)


class XrpIndexer:
    def __init__(
        self,
        rail: str = "XRP",
        json_rpc: str = "https://xrplcluster.com",
        lock_id: str = "",
        watch_account: str = "",
    ):
        self.rail = rail
        self.family = "xrp_ledger"
        self.json_rpc = json_rpc
        self.lock_id = lock_id
        self.watch_account = watch_account  # classic address receiving protocol locks
        self._ledger_deps: dict[int, list[dict[str, Any]]] = {}

    def _rpc(self, method: str, params: dict) -> Any:
        body = json.dumps({"method": method, "params": [params]}).encode()
        req = urllib.request.Request(
            self.json_rpc,
            data=body,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=45) as resp:
            data = json.loads(resp.read().decode())
        return data.get("result") or data

    def tip_height(self) -> int:
        r = self._rpc("ledger_closed", {})
        return int(r.get("ledger_index") or 0)

    def _payments_in_ledger(self, height: int) -> tuple[str, str, list[dict[str, Any]]]:
        """Returns (ledger_hash, tx_root, deposits)."""
        if height in self._ledger_deps:
            # re-fetch meta only once cached as list of deps; store meta separately
            pass
        r = self._rpc(
            "ledger",
            {
                "ledger_index": height,
                "transactions": True,
                "expand": True,
                "owner_funds": False,
            },
        )
        ledger = r.get("ledger") or r
        lhash = (ledger.get("ledger_hash") or ledger.get("hash") or str(height)).replace(
            "0x", ""
        )
        tx_root = (
            ledger.get("transaction_hash") or ledger.get("account_hash") or lhash
        ).replace("0x", "")
        deps: list[dict[str, Any]] = []
        if not self.watch_account or not self.lock_id:
            self._ledger_deps[height] = deps
            return lhash, tx_root, deps
        txs = ledger.get("transactions") or []
        for tx in txs:
            if not isinstance(tx, dict):
                continue
            if tx.get("TransactionType") != "Payment":
                continue
            dest = tx.get("Destination") or ""
            if dest != self.watch_account:
                continue
            # Amount can be string drops or IOU object — only native XRP drops for v1
            amt_raw = tx.get("Amount")
            if not isinstance(amt_raw, str):
                continue
            try:
                amount = int(amt_raw)
            except ValueError:
                continue
            if amount <= 0:
                continue
            txh = (tx.get("hash") or "").replace("0x", "")
            if not txh:
                continue
            leaf = xrp_deposit_leaf(txh, amount, self.lock_id, dest)
            deps.append(
                {
                    "tx_hash": txh,
                    "amount": amount,
                    "destination": dest,
                    "leaf": leaf,
                    "height": height,
                    "ledger_hash": lhash,
                    "lock_id": self.lock_id,
                }
            )
        deps.sort(key=lambda d: d["tx_hash"])
        self._ledger_deps[height] = deps
        return lhash, tx_root, deps

    def headers_after(
        self, falcon_tip_height: int, limit: int = 32, *, bootstrap_near_tip: bool = False
    ) -> list[HeaderBatch]:
        tip = self.tip_height()
        if falcon_tip_height == 0 and bootstrap_near_tip:
            start = max(1, tip - limit + 1)
            end = tip
        else:
            start = falcon_tip_height + 1
            if start <= 0 or start > tip:
                return []
            end = min(tip, falcon_tip_height + limit)
        out: list[HeaderBatch] = []
        empty = empty_xrp_deposit_root()
        parent = ""
        # When bootstrapping mid-chain, parent of first header may be unknown — Falcon allows
        # any first header when tip is empty; subsequent must link.
        for h in range(start, end + 1):
            try:
                lhash, tx_root, deps = self._payments_in_ledger(h)
            except Exception:
                break
            if deps:
                leaves = [d["leaf"] for d in deps]
                dep_root, _ = build_binary_merkle(leaves)
            else:
                dep_root = empty
            bind = evm_bind_root(tx_root, dep_root)
            out.append(
                HeaderBatch(
                    height=h,
                    block_hash=lhash,
                    parent_hash=parent,
                    merkle_root=bind,
                    work=str(h),
                    tx_root=tx_root,
                )
            )
            parent = lhash
        return out

    def pending_deposits(self, lock_id: str, after_height: int = 0) -> list[LockDeposit]:
        if not self.watch_account:
            return []
        tip = self.tip_height()
        from_h = max(after_height + 1, tip - 200)
        out: list[LockDeposit] = []
        for h in range(from_h, tip + 1):
            try:
                lhash, _tx_root, deps = self._payments_in_ledger(h)
            except Exception:
                continue
            if not deps:
                continue
            leaves = [d["leaf"] for d in deps]
            dep_root, paths = build_binary_merkle(leaves)
            for i, d in enumerate(deps):
                lid = lock_id or d["lock_id"]
                if lid != d["lock_id"]:
                    continue
                out.append(
                    LockDeposit(
                        amount=d["amount"],
                        external_txid=f"{d['tx_hash']}:{d['destination']}",
                        block_hash=d["ledger_hash"],
                        height=d["height"],
                        merkle_path=list(paths[i]),
                        merkle_index=i,
                        lock_id=d["lock_id"],
                        merkle_root=dep_root,
                    )
                )
                # external_to for leaf dest is embedded in external_txid after ':'
        return out
