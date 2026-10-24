"""Local-net indexer: deterministic headers + one lock deposit (rules still enforce on Falcon)."""

from __future__ import annotations

from .base import HeaderBatch, LockDeposit
from .merkle_util import (
    build_binary_merkle,
    empty_evm_deposit_root,
    evm_bind_root,
    evm_deposit_leaf,
    synth_hash32,
    synthetic_inclusion_bitcoin,
)


class SyntheticIndexer:
    """Builds family-correct synthetic proofs for smoke tests — still not a mint authority."""

    def __init__(self, rail: str, lock_id: str, amount: int = 10_000):
        self.rail = rail.upper()
        self.lock_id = lock_id
        self.amount = amount
        self.family = {
            "BTC": "bitcoin",
            "ETH": "evm",
            "BNB": "evm",
            "XRP": "xrp_ledger",
        }.get(self.rail, "bitcoin")
        self._headers: list[HeaderBatch] = []
        self._deposit: LockDeposit | None = None
        self._build()

    def _build(self) -> None:
        if self.family == "bitcoin":
            # internal merkle leaf; external_txid is display-order (byte-reversed) like Esplora
            from .merkle_util import reverse_hex

            leaf = synth_hash32(f"dev_lock_{self.rail.lower()}_001")
            s0 = synth_hash32("s0")
            s1 = synth_hash32("s1")
            path, root = synthetic_inclusion_bitcoin(leaf, [s0, s1], 0)
            external_txid = reverse_hex(leaf)
            merkle_index = 0
            merkle_path = list(path)
            header_merkle = root  # BTC: header root is block tx merkle
            tx_root = ""
            deposit_merkle = root
        elif self.family == "evm":
            txh = synth_hash32(f"evm_tx_{self.rail.lower()}")
            log_index = 0
            leaf = evm_deposit_leaf(txh, log_index, self.amount, self.lock_id)
            dep_root, paths = build_binary_merkle([leaf])
            tx_root = synth_hash32(f"evm_txroot_{self.rail.lower()}")
            header_merkle = evm_bind_root(tx_root, dep_root)
            external_txid = f"{txh}:{log_index}"
            merkle_index = 0
            merkle_path = list(paths[0])
            deposit_merkle = dep_root
        else:
            # XRP: binary deposit leaf + bind to synthetic ledger tx_root
            from .merkle_util import empty_xrp_deposit_root, xrp_deposit_leaf

            txh = synth_hash32(f"xrp_tx_{self.rail.lower()}")
            dest = "rFalconProtocolLockSynthetic00001"
            leaf = xrp_deposit_leaf(txh, self.amount, self.lock_id, dest)
            dep_root, paths = build_binary_merkle([leaf])
            tx_root = synth_hash32(f"xrp_ledger_txroot_{self.rail.lower()}")
            header_merkle = evm_bind_root(tx_root, dep_root)
            external_txid = f"{txh}:{dest}"
            merkle_index = 0
            merkle_path = list(paths[0])
            deposit_merkle = dep_root

        parent = ""
        for h in range(1, 8):
            bh = f"hdr{self.rail}{h}"
            # non-deposit blocks: EVM/XRP still bind empty deposit tree to a synthetic tx_root
            if self.family in ("evm", "xrp_ledger") and h != 1:
                if self.family == "evm":
                    empty = empty_evm_deposit_root()
                    tr = synth_hash32(f"evm_txroot_{self.rail}_{h}")
                else:
                    from .merkle_util import empty_xrp_deposit_root

                    empty = empty_xrp_deposit_root()
                    tr = synth_hash32(f"xrp_txroot_{self.rail}_{h}")
                hm = evm_bind_root(tr, empty)
                tr_use = tr
            else:
                hm = header_merkle
                tr_use = tx_root
            if self.family == "bitcoin":
                mr = header_merkle if h == 1 else f"root{h}"
                tr_out = ""
            else:
                mr = hm
                tr_out = tr_use
            self._headers.append(
                HeaderBatch(
                    height=h,
                    block_hash=bh,
                    parent_hash=parent,
                    merkle_root=mr,
                    work=str(h * 1000),
                    tx_root=tr_out,
                )
            )
            parent = bh

        self._deposit = LockDeposit(
            amount=self.amount,
            external_txid=external_txid,
            block_hash=f"hdr{self.rail}1",
            height=1,
            merkle_path=merkle_path,
            merkle_index=merkle_index,
            lock_id=self.lock_id,
            merkle_root=deposit_merkle,
        )

    def tip_height(self) -> int:
        return self._headers[-1].height if self._headers else 0

    def headers_after(
        self, falcon_tip_height: int, limit: int = 32, *, bootstrap_near_tip: bool = False
    ) -> list[HeaderBatch]:
        _ = bootstrap_near_tip
        out = [h for h in self._headers if h.height > falcon_tip_height]
        return out[:limit]

    def pending_deposits(self, lock_id: str, after_height: int = 0) -> list[LockDeposit]:
        if not self._deposit or lock_id != self.lock_id:
            return []
        if self._deposit.height <= after_height:
            return []
        return [self._deposit]
