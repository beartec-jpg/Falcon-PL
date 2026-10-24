"""Option B — DAG-aware federated ordering of certificates.

Total order is a pure function of the set of certified batches and their
parent links: topological order with deterministic tie-break
(causal_round, timestamp, batch_hash).

Cert sets grow monotonically under normal operation. Ordering is extended
incrementally from the existing prefix (Fix 3), reusing a persistent
causal-round memo so multi-packer cert storms stay O(new) not O(certs²).
"""

from __future__ import annotations

from falcon_dag.certificate_dag import CertificateDAG
from falcon_dag.config import DEFAULT_CONFIG, ProtocolConfig
from falcon_dag.types import GENESIS_BATCH_HASH, GENESIS_CERT_ID, Certificate


class OrderingEngine:
    def __init__(
        self,
        dag: CertificateDAG,
        config: ProtocolConfig = DEFAULT_CONFIG,
    ) -> None:
        self.dag = dag
        self.config = config
        self.ordered: list[str] = [GENESIS_BATCH_HASH]
        self.ordered_set: set[str] = {GENESIS_BATCH_HASH}
        self.ordered_certs: list[str] = [GENESIS_CERT_ID]
        self.hard_final: set[str] = {GENESIS_BATCH_HASH}
        self._last_cert_count = 1
        # Fix 3: persistent causal-round memo across try_commit calls.
        self._causal_memo: dict[str, int] = {GENESIS_BATCH_HASH: 0}

    def _causal_round(self, batch_hash: str, memo: dict[str, int]) -> int:
        if batch_hash in memo:
            return memo[batch_hash]
        if batch_hash == GENESIS_BATCH_HASH:
            memo[batch_hash] = 0
            return 0
        cert = self.dag.certificates.get(batch_hash)
        if cert is None:
            memo[batch_hash] = 0
            return 0
        parents = cert.parents or []
        if not parents:
            memo[batch_hash] = 1
            return 1
        r = 1 + max(self._causal_round(p, memo) for p in parents)
        memo[batch_hash] = r
        return r

    def _compute_total_order(self) -> list[str]:
        """Total order over certified batches — incremental when cert set grows."""
        certs = dict(self.dag.certificates)
        if GENESIS_BATCH_HASH not in certs:
            return [GENESIS_BATCH_HASH]

        memo = self._causal_memo

        # Fix 3: start from existing ordered prefix when it is still valid.
        ordered: list[str] = []
        for bh in self.ordered:
            if bh == GENESIS_BATCH_HASH or bh in certs:
                ordered.append(bh)
            else:
                # Cert set shrank (prune) — fall back to full recompute.
                ordered = [GENESIS_BATCH_HASH]
                break
        if not ordered or ordered[0] != GENESIS_BATCH_HASH:
            ordered = [GENESIS_BATCH_HASH]

        ordered_set = set(ordered)
        remaining = set(certs.keys()) - ordered_set

        while remaining:
            ready: list[str] = []
            for bh in remaining:
                parents = certs[bh].parents or []
                if all(p in ordered_set for p in parents):
                    ready.append(bh)
            if not ready:
                # Break cycles / missing parents — force by sort key
                ready = list(remaining)
                ready.sort(
                    key=lambda h: (
                        self._causal_round(h, memo),
                        certs[h].timestamp,
                        h,
                    )
                )
                h = ready[0]
                ordered.append(h)
                ordered_set.add(h)
                remaining.discard(h)
                continue
            ready.sort(
                key=lambda h: (
                    self._causal_round(h, memo),
                    certs[h].timestamp,
                    h,
                )
            )
            committed_any = False
            for h in ready:
                parents = certs[h].parents or []
                if parents and not all(p in ordered_set for p in parents):
                    continue
                ordered.append(h)
                ordered_set.add(h)
                remaining.discard(h)
                committed_any = True
            if not committed_any:
                h = ready[0]
                ordered.append(h)
                ordered_set.add(h)
                remaining.discard(h)

        return ordered

    def try_commit(self) -> list[Certificate]:
        """Extend total order; return newly appended certificates since last call."""
        new_order = self._compute_total_order()
        prev = list(self.ordered)
        self.ordered = new_order
        self.ordered_set = set(new_order)
        self.ordered_certs = []
        for bh in new_order:
            c = self.dag.certificates.get(bh)
            self.ordered_certs.append(c.cert_id if c else bh)

        self._update_hard_finality()

        newly: list[Certificate] = []
        if new_order[: len(prev)] == prev:
            for bh in new_order[len(prev) :]:
                c = self.dag.certificates.get(bh)
                if c:
                    newly.append(c)
        else:
            for bh in new_order:
                if bh == GENESIS_BATCH_HASH:
                    continue
                c = self.dag.certificates.get(bh)
                if c:
                    newly.append(c)
        self._last_cert_count = len(self.dag.certificates)
        return newly

    def _update_hard_finality(self) -> None:
        n = len(self.ordered)
        h = self.config.hard_finality_h
        self.hard_final = {GENESIS_BATCH_HASH}
        for i, bh in enumerate(self.ordered):
            if n - i - 1 >= h:
                self.hard_final.add(bh)

    def metrics(self) -> dict:
        n_cert = len(self.dag.certificates)
        return {
            "ordered": len(self.ordered),
            "hard_final": len(self.hard_final),
            "tip_ordered": self.ordered[-1] if self.ordered else None,
            # Fix 7: certs waiting to be ordered
            "ordering_lag": max(0, n_cert - len(self.ordered)),
        }
