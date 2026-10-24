"""Account balances and sequences for deterministic execution."""

from __future__ import annotations

from dataclasses import dataclass, field

from falcon_dag.types import Transaction


@dataclass
class AccountState:
    balances: dict[str, int] = field(default_factory=dict)
    sequences: dict[str, int] = field(default_factory=dict)

    def ensure(self, account: str, balance: int = 0, sequence: int = 1) -> None:
        if account not in self.balances:
            self.balances[account] = balance
            self.sequences[account] = sequence

    def clone(self) -> AccountState:
        return AccountState(dict(self.balances), dict(self.sequences))

    def can_apply(self, tx: Transaction) -> bool:
        self.ensure(tx.account)
        self.ensure(tx.destination)
        if self.sequences[tx.account] != tx.sequence:
            return False
        if self.balances[tx.account] < tx.amount + tx.fee:
            return False
        return True

    def apply(self, tx: Transaction) -> bool:
        if not self.can_apply(tx):
            return False
        self.balances[tx.account] -= tx.amount + tx.fee
        self.balances[tx.destination] = self.balances.get(tx.destination, 0) + tx.amount
        self.sequences[tx.account] = tx.sequence + 1
        return True
