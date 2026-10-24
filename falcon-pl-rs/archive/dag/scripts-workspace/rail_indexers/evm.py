"""EVM (ETH / BNB) JSON-RPC indexer — multi-endpoint failover + Deposit trees.

Header commitment:
  deposit_root = merkle(Deposit leaves) or empty
  tx_root      = block.transactionsRoot
  merkle_root  = sha256(tx_root‖deposit_root)
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from typing import Any, Sequence, Union

from .base import HeaderBatch, LockDeposit
from .merkle_util import (
    build_binary_merkle,
    empty_evm_deposit_root,
    evm_bind_root,
    evm_deposit_leaf,
)

# Public endpoints — first working wins (sticky for session).
DEFAULT_ETH_RPCS = [
    "https://ethereum.publicnode.com",
    "https://eth.llamarpc.com",
    "https://rpc.ankr.com/eth",
    "https://1rpc.io/eth",
    "https://cloudflare-eth.com",
    "https://eth.drpc.org",
]

DEFAULT_BNB_RPCS = [
    "https://bsc-dataseed.binance.org",
    "https://bsc-dataseed1.defibit.io",
    "https://bsc-dataseed1.ninicoin.io",
    "https://rpc.ankr.com/bsc",
    "https://1rpc.io/bnb",
    "https://bsc.publicnode.com",
]


def parse_rpc_list(value: str | Sequence[str] | None, defaults: list[str]) -> list[str]:
    if value is None or value == "":
        return list(defaults)
    if isinstance(value, (list, tuple)):
        urls = [str(u).strip() for u in value if str(u).strip()]
    else:
        urls = [u.strip() for u in str(value).split(",") if u.strip()]
    return urls or list(defaults)


def eth_rpc_urls_from_env(cli: str | None = None) -> list[str]:
    env = os.environ.get("ETH_RPC") or os.environ.get("ETH_RPCS") or ""
    if env.strip():
        return parse_rpc_list(env, DEFAULT_ETH_RPCS)
    return parse_rpc_list(cli, DEFAULT_ETH_RPCS)


def bnb_rpc_urls_from_env(cli: str | None = None) -> list[str]:
    env = os.environ.get("BNB_RPC") or os.environ.get("BNB_RPCS") or ""
    if env.strip():
        return parse_rpc_list(env, DEFAULT_BNB_RPCS)
    return parse_rpc_list(cli, DEFAULT_BNB_RPCS)


class EvmJsonRpcIndexer:
    def __init__(
        self,
        rail: str,
        rpc_url: Union[str, Sequence[str]] = "",
        lock_id: str = "",
        lock_contract: str = "",
        rpc_urls: Sequence[str] | None = None,
    ):
        self.rail = rail.upper()
        self.family = "evm"
        if rpc_urls is not None:
            self.rpc_urls = parse_rpc_list(list(rpc_urls), DEFAULT_ETH_RPCS)
        elif self.rail == "BNB":
            self.rpc_urls = parse_rpc_list(rpc_url, DEFAULT_BNB_RPCS)
        else:
            self.rpc_urls = parse_rpc_list(rpc_url, DEFAULT_ETH_RPCS)
        self.rpc_url = self.rpc_urls[0]
        self.lock_id = lock_id
        self.lock_contract = lock_contract.lower().removeprefix("0x")
        self._block_deposits: dict[int, list[dict[str, Any]]] = {}
        self.last_rpc_error: str | None = None

    def _rpc(self, method: str, params: list) -> Any:
        body = json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        ).encode()
        errors: list[str] = []
        # Prefer sticky working URL first
        order = [self.rpc_url] + [u for u in self.rpc_urls if u != self.rpc_url]
        for url in order:
            try:
                req = urllib.request.Request(
                    url,
                    data=body,
                    headers={
                        "Content-Type": "application/json",
                        "User-Agent": "falcon-rail-agent/0.25",
                    },
                    method="POST",
                )
                with urllib.request.urlopen(req, timeout=25) as resp:
                    data = json.loads(resp.read().decode())
                if "error" in data and data["error"]:
                    errors.append(f"{url}: {data['error']}")
                    continue
                self.rpc_url = url
                self.last_rpc_error = None
                return data.get("result")
            except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
                errors.append(f"{url}: {e}")
                continue
        self.last_rpc_error = "; ".join(errors[:5])
        raise RuntimeError(
            f"all EVM RPC endpoints failed for {self.rail}: {self.last_rpc_error}"
        )

    def tip_height(self) -> int:
        return int(self._rpc("eth_blockNumber", []), 16)

    def _deposits_in_block(self, height: int, block_hash: str) -> list[dict[str, Any]]:
        if height in self._block_deposits:
            return self._block_deposits[height]
        if not self.lock_contract or not self.lock_id:
            self._block_deposits[height] = []
            return []
        try:
            topic0 = "0x" + self._deposit_topic0()
            logs = self._rpc(
                "eth_getLogs",
                [
                    {
                        "fromBlock": hex(height),
                        "toBlock": hex(height),
                        "address": "0x" + self.lock_contract,
                        "topics": [topic0],
                    }
                ],
            )
        except Exception:
            self._block_deposits[height] = []
            return []
        if not isinstance(logs, list):
            logs = []
        lock_norm = self.lock_id.removeprefix("0x").lower()
        deps: list[dict[str, Any]] = []
        for log in logs:
            try:
                topics = log.get("topics") or []
                if len(topics) < 2:
                    continue
                lid = topics[1].removeprefix("0x").lower()
                if lock_norm and lid != lock_norm:
                    continue
                data = (log.get("data") or "0x").removeprefix("0x")
                if len(data) < 64:
                    continue
                amount = int(data[0:64], 16)
                txh = (log.get("transactionHash") or "").removeprefix("0x")
                log_index = int(log.get("logIndex") or "0x0", 16)
                leaf = evm_deposit_leaf(txh, log_index, amount, self.lock_id)
                deps.append(
                    {
                        "amount": amount,
                        "tx_hash": txh,
                        "log_index": log_index,
                        "leaf": leaf,
                        "block_hash": block_hash,
                        "height": height,
                        "lock_id": self.lock_id,
                    }
                )
            except Exception:
                continue
        deps.sort(key=lambda d: (d["tx_hash"], d["log_index"]))
        self._block_deposits[height] = deps
        return deps

    def headers_after(
        self, falcon_tip_height: int, limit: int = 32, *, bootstrap_near_tip: bool = False
    ) -> list[HeaderBatch]:
        tip = self.tip_height()
        if falcon_tip_height == 0 and bootstrap_near_tip:
            start = max(1, tip - limit + 1)
            end = tip
        else:
            start = falcon_tip_height + 1
            if start > tip:
                return []
            end = min(tip, falcon_tip_height + limit)
        out: list[HeaderBatch] = []
        empty = empty_evm_deposit_root()
        for h in range(start, end + 1):
            block = self._rpc("eth_getBlockByNumber", [hex(h), False])
            if not block:
                break
            bh = block["hash"].replace("0x", "")
            tx_root = (block.get("transactionsRoot") or "").replace("0x", "")
            deps = self._deposits_in_block(h, bh)
            if deps:
                leaves = [d["leaf"] for d in deps]
                dep_root, _ = build_binary_merkle(leaves)
            else:
                dep_root = empty
            bind = evm_bind_root(tx_root, dep_root)
            work_src = block.get("totalDifficulty") or block["number"]
            try:
                work = str(int(work_src, 16))
            except Exception:
                work = str(h)
            out.append(
                HeaderBatch(
                    height=int(block["number"], 16),
                    block_hash=bh,
                    parent_hash=(block.get("parentHash") or "").replace("0x", ""),
                    merkle_root=bind,
                    work=work,
                    tx_root=tx_root,
                )
            )
        return out

    def pending_deposits(self, lock_id: str, after_height: int = 0) -> list[LockDeposit]:
        if not self.lock_contract:
            return []
        tip = self.tip_height()
        from_h = max(after_height + 1, tip - 500)
        out: list[LockDeposit] = []
        for h in range(from_h, tip + 1):
            try:
                block = self._rpc("eth_getBlockByNumber", [hex(h), False])
                if not block:
                    continue
                bh = block["hash"].replace("0x", "")
            except Exception:
                continue
            deps = self._deposits_in_block(h, bh)
            if not deps:
                continue
            leaves = [d["leaf"] for d in deps]
            dep_root, paths = build_binary_merkle(leaves)
            for i, d in enumerate(deps):
                lid = lock_id or d["lock_id"]
                if lid.removeprefix("0x").lower() != d["lock_id"].removeprefix("0x").lower():
                    continue
                out.append(
                    LockDeposit(
                        amount=d["amount"],
                        external_txid=f"{d['tx_hash']}:{d['log_index']}",
                        block_hash=d["block_hash"],
                        height=d["height"],
                        merkle_path=list(paths[i]),
                        merkle_index=i,
                        lock_id=d["lock_id"],
                        merkle_root=dep_root,
                    )
                )
        return out

    @staticmethod
    def _deposit_topic0() -> str:
        try:
            from .keccak_lite import keccak256_hex
        except ImportError:
            from rail_indexers.keccak_lite import keccak256_hex  # type: ignore
        return keccak256_hex(b"Deposit(bytes32,address,uint256,bytes32)")
