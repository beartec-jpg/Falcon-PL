from __future__ import annotations

from .base import ChainIndexer
from .btc import BtcEsploraIndexer
from .evm import (
    DEFAULT_BNB_RPCS,
    DEFAULT_ETH_RPCS,
    EvmJsonRpcIndexer,
    bnb_rpc_urls_from_env,
    eth_rpc_urls_from_env,
)
from .synthetic import SyntheticIndexer
from .xrp import XrpIndexer


def make_indexer(
    rail: str,
    *,
    mode: str = "synthetic",
    lock_id: str = "",
    amount: int = 10_000,
    btc_api: str = "https://blockstream.info/api",
    btc_watch_address: str = "",
    eth_rpc: str = "",
    bnb_rpc: str = "",
    xrp_rpc: str = "https://xrplcluster.com",
    xrp_watch_account: str = "",
    lock_contract: str = "",
) -> ChainIndexer:
    """
    mode:
      synthetic — local proofs for smoke (no external network)
      live      — public chain APIs (headers; deposits when configured)

    ETH_RPC / BNB_RPC env: comma-separated endpoint lists (override CLI defaults).
    """
    rail = rail.upper()
    if mode == "synthetic":
        return SyntheticIndexer(rail, lock_id=lock_id, amount=amount)
    if rail == "BTC":
        return BtcEsploraIndexer(
            rail=rail, base_url=btc_api, lock_id=lock_id, watch_address=btc_watch_address
        )
    if rail == "ETH":
        urls = eth_rpc_urls_from_env(eth_rpc or None)
        return EvmJsonRpcIndexer(
            rail=rail,
            rpc_urls=urls,
            lock_id=lock_id,
            lock_contract=lock_contract,
        )
    if rail == "BNB":
        urls = bnb_rpc_urls_from_env(bnb_rpc or None)
        return EvmJsonRpcIndexer(
            rail=rail,
            rpc_urls=urls,
            lock_id=lock_id,
            lock_contract=lock_contract,
        )
    if rail == "XRP":
        return XrpIndexer(
            rail=rail,
            json_rpc=xrp_rpc,
            lock_id=lock_id,
            watch_account=xrp_watch_account,
        )
    raise ValueError(f"unknown rail {rail}")


__all__ = [
    "make_indexer",
    "DEFAULT_ETH_RPCS",
    "DEFAULT_BNB_RPCS",
]
