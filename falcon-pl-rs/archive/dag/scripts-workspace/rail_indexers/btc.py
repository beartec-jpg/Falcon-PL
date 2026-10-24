"""Bitcoin mainnet/testnet indexer (Blockstream/Esplora HTTP).

Live inbound:
  - Headers: real block id / prev / merkle_root / height
  - Deposits: payments to --btc-watch-address with Esplora merkle-proof
  - external_txid = display txid; Falcon reverses to internal leaf for dsha256 merkle

lock_id must match Falcon rail.lock_id (agent supplies from rail_info).
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any

from .base import HeaderBatch, LockDeposit
from .merkle_util import reverse_hex


class BtcEsploraIndexer:
    def __init__(
        self,
        rail: str = "BTC",
        base_url: str = "https://blockstream.info/api",
        lock_id: str = "",
        watch_address: str = "",
    ):
        self.rail = rail
        self.family = "bitcoin"
        self.base_url = base_url.rstrip("/")
        self.lock_id = lock_id
        self.watch_address = watch_address

    def _get(self, path: str) -> Any:
        url = f"{self.base_url}{path}"
        req = urllib.request.Request(url, headers={"User-Agent": "falcon-rail-agent/0.23"})
        with urllib.request.urlopen(req, timeout=45) as resp:
            raw = resp.read().decode()
            if raw[:1] in ("{", "["):
                return json.loads(raw)
            return raw.strip()

    def tip_height(self) -> int:
        return int(self._get("/blocks/tip/height"))

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
        for h in range(start, end + 1):
            try:
                bh = self._get(f"/block-height/{h}")
                if not isinstance(bh, str):
                    bh = str(bh)
                block = self._get(f"/block/{bh}")
                out.append(
                    HeaderBatch(
                        height=int(block.get("height", h)),
                        block_hash=block.get("id", bh),
                        parent_hash=block.get("previousblockhash") or "",
                        merkle_root=block.get("merkle_root") or "",
                        work=str(int(block.get("height", h)) * 1_000_000),
                    )
                )
            except (
                urllib.error.URLError,
                urllib.error.HTTPError,
                TimeoutError,
                ValueError,
                KeyError,
                TypeError,
            ):
                break
        return out

    def pending_deposits(self, lock_id: str, after_height: int = 0) -> list[LockDeposit]:
        """
        Scan address txs and attach Esplora merkle proofs.
        Without watch_address, returns [] (headers still advance tip).
        """
        if not self.watch_address:
            return []
        try:
            txs = self._get(f"/address/{self.watch_address}/txs")
        except Exception:
            return []
        if not isinstance(txs, list):
            return []
        lid = lock_id or self.lock_id
        out: list[LockDeposit] = []
        for tx in txs:
            try:
                status = tx.get("status") or {}
                if not status.get("confirmed"):
                    continue
                height = int(status.get("block_height") or 0)
                if height <= after_height:
                    continue
                txid = (tx.get("txid") or "").lower()
                if len(txid) != 64:
                    continue
                block_hash = status.get("block_hash") or ""
                amount = 0
                for vout in tx.get("vout") or []:
                    spk = vout.get("scriptpubkey_address") or ""
                    if spk == self.watch_address:
                        amount += int(vout.get("value") or 0)
                if amount <= 0:
                    continue
                proof = self._get(f"/tx/{txid}/merkle-proof")
                path = list(proof.get("merkle") or [])
                pos = int(proof.get("pos") or 0)
                # Esplora path siblings are internal hashes; leaf is reverse(display_txid).
                # Falcon deposit_proof_leaf reverses external_txid for Bitcoin family.
                out.append(
                    LockDeposit(
                        amount=amount,
                        external_txid=txid,
                        block_hash=block_hash,
                        height=height,
                        merkle_path=path,
                        merkle_index=pos,
                        lock_id=lid,
                        merkle_root="",  # verified against header.merkle_root on Falcon
                    )
                )
            except Exception:
                continue
        # Prefer older first for confirmations
        out.sort(key=lambda d: d.height)
        return out


def reverse_hex_txid(display: str) -> str:
    """Helper for tests/docs — same as Falcon btc_txid_to_internal_leaf."""
    return reverse_hex(display)
