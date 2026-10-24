from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Optional, Protocol, runtime_checkable


@dataclass
class HeaderBatch:
    """One external block/ledger header → Falcon SubmitRailHeader.bridge fields."""

    height: int
    block_hash: str
    parent_hash: str
    merkle_root: str
    work: str  # decimal string for u128 compare
    # EVM: transactionsRoot; header.merkle_root = sha256(tx_root‖deposit_root)
    tx_root: str = ""

    def as_bridge(self) -> dict[str, Any]:
        d = {
            "height": self.height,
            "block_hash": self.block_hash,
            "parent_hash": self.parent_hash,
            "merkle_root": self.merkle_root,
            "work": self.work,
        }
        if self.tx_root:
            d["tx_root"] = self.tx_root
        return d


@dataclass
class LockDeposit:
    """Verified lock observation → Falcon RailDeposit.bridge + amount."""

    amount: int
    external_txid: str
    block_hash: str
    height: int
    merkle_path: list[str]
    merkle_index: int
    lock_id: str
    merkle_root: str
    recipient_hint: str = ""  # optional Falcon account encoded in lock memo

    def as_bridge(self) -> dict[str, Any]:
        return {
            "external_txid": self.external_txid,
            "block_hash": self.block_hash,
            "height": self.height,
            "merkle_path": self.merkle_path,
            "merkle_index": self.merkle_index,
            "lock_id": self.lock_id,
            "merkle_root": self.merkle_root,
        }


@runtime_checkable
class ChainIndexer(Protocol):
    """Unprivileged observer. No keys that can mint on Falcon."""

    rail: str
    family: str

    def tip_height(self) -> int:
        """Best known external tip (for polling)."""
        ...

    def headers_after(
        self, falcon_tip_height: int, limit: int = 32, *, bootstrap_near_tip: bool = False
    ) -> list[HeaderBatch]:
        """Headers after Falcon tip. If tip=0 and bootstrap_near_tip, start near chain tip."""
        ...

    def pending_deposits(
        self, lock_id: str, after_height: int = 0
    ) -> list[LockDeposit]:
        """Lock events to the protocol lock_id with merkle proofs."""
        ...
