"""Execute transactions from ordered certificates."""

from __future__ import annotations

from falcon_dag.account_state import AccountState
from falcon_dag.certificate_dag import CertificateDAG
from falcon_dag.ordering import OrderingEngine
from falcon_dag.types import GENESIS_BATCH_HASH, Transaction


class ExecutionEngine:
    def __init__(
        self,
        dag: CertificateDAG,
        ordering: OrderingEngine,
        genesis_balances: dict[str, int] | None = None,
    ) -> None:
        self.dag = dag
        self.ordering = ordering
        self.state = AccountState()
        if genesis_balances:
            for a, b in genesis_balances.items():
                self.state.ensure(a, balance=b, sequence=1)
        else:
            for i in range(1, 21):
                self.state.ensure(f"acct{i}", balance=1_000_000_000, sequence=1)
        self._genesis = self.state.clone()
        self.executed_txs: list[str] = []
        self.soft_final_txs: set[str] = set()  # in any certified batch
        self._executed_batches: set[str] = {GENESIS_BATCH_HASH}

    def refresh_soft_final(self) -> None:
        for bh, batch in self.dag.batches.items():
            if bh in self.dag.certificates:
                for tid in batch.tx_ids:
                    self.soft_final_txs.add(tid)

    def apply_new_ordered(self) -> list[Transaction]:
        """Re-execute full ordered certificate sequence (pure + deterministic).

        Replaying from genesis whenever order is refreshed guarantees all
        honest nodes with the same ordered cert list reach the same state.
        """
        self.state = self._genesis.clone()
        self.executed_txs = []
        self._executed_batches = {GENESIS_BATCH_HASH}
        applied: list[Transaction] = []
        seen_tx: set[str] = set()
        for bh in self.ordering.ordered:
            if bh == GENESIS_BATCH_HASH:
                continue
            batch = self.dag.batches.get(bh)
            if batch is None:
                self._executed_batches.add(bh)
                continue
            for tid in batch.tx_ids:
                if tid in seen_tx:
                    continue
                tx = self.dag.tx_index.get(tid) or batch.txs.get(tid)
                if tx is None:
                    continue
                if self.state.apply(tx):
                    self.executed_txs.append(tid)
                    applied.append(tx)
                    seen_tx.add(tid)
            self._executed_batches.add(bh)
        self.refresh_soft_final()
        return applied

    def metrics(self) -> dict:
        return {
            "executed_txs": len(self.executed_txs),
            "soft_final_txs": len(self.soft_final_txs),
        }
