"""Global mempool with deterministic priority."""

from __future__ import annotations

import time

from falcon_dag.config import DEFAULT_CONFIG, ProtocolConfig
from falcon_dag.types import Transaction


class GlobalMempool:
    def __init__(self, config: ProtocolConfig = DEFAULT_CONFIG) -> None:
        self.config = config
        self._txs: dict[str, Transaction] = {}
        self._by_acct_seq: dict[str, dict[int, set[str]]] = {}
        self._done: set[str] = set()  # ordered or soft-final enough to drop competitors

    def __len__(self) -> int:
        return len(self._txs)

    def add(self, tx: Transaction) -> bool:
        if tx.tx_id in self._txs or tx.tx_id in self._done:
            return False
        if len(self._txs) >= self.config.max_mempool_txs:
            return False
        self._txs[tx.tx_id] = tx
        self._by_acct_seq.setdefault(tx.account, {}).setdefault(tx.sequence, set()).add(
            tx.tx_id
        )
        return True

    def get(self, tx_id: str) -> Transaction | None:
        return self._txs.get(tx_id)

    def remove(self, tx_id: str) -> None:
        tx = self._txs.pop(tx_id, None)
        if tx is None:
            return
        bucket = self._by_acct_seq.get(tx.account, {}).get(tx.sequence)
        if bucket is not None:
            bucket.discard(tx_id)
            if not bucket:
                del self._by_acct_seq[tx.account][tx.sequence]
                if not self._by_acct_seq[tx.account]:
                    del self._by_acct_seq[tx.account]

    def mark_done(self, tx_ids: list[str]) -> None:
        for tid in tx_ids:
            self._done.add(tid)
            tx = self._txs.get(tid)
            if tx is None:
                # still drop competitors by scanning
                continue
            comps = list(self._by_acct_seq.get(tx.account, {}).get(tx.sequence, set()))
            for cid in comps:
                self.remove(cid)

    def priority_key(self, tx: Transaction, now: float) -> tuple:
        # Do not use `or` on first_seen — 0.0 is a valid timestamp.
        age = max(0.0, now - float(tx.first_seen))
        density = tx.fee_density + age * self.config.starvation_boost_per_s
        return (density, -float(tx.first_seen), tx.tx_id)

    def ordered(self, now: float | None = None) -> list[Transaction]:
        now = now if now is not None else time.time()
        return sorted(self._txs.values(), key=lambda t: self.priority_key(t, now), reverse=True)
