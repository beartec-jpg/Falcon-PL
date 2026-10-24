"""Combined Narwhal DA + ordering + execution for one validator."""

from __future__ import annotations

import time
from typing import Callable

from falcon_dag.certificate_dag import CertificateDAG
from falcon_dag.config import DEFAULT_CONFIG, ProtocolConfig
from falcon_dag.crypto import CryptoBackend, HmacKeyRing, create_keyring
from falcon_dag.execution import ExecutionEngine
from falcon_dag.mempool import GlobalMempool
from falcon_dag.ordering import OrderingEngine
from falcon_dag.types import (
    GENESIS_BATCH_HASH,
    Batch,
    Certificate,
    PartialSignature,
    Transaction,
)


class FalconDAGEngine:
    def __init__(
        self,
        validator_id: str,
        unl: dict[str, float],
        config: ProtocolConfig = DEFAULT_CONFIG,
        genesis_balances: dict[str, int] | None = None,
        keyring: CryptoBackend | None = None,
        worker_index: int = 0,
        worker_count: int = 1,
        keys_dir: str | None = None,
        on_batch: Callable[[Batch], None] | None = None,
        on_sig: Callable[[PartialSignature], None] | None = None,
        on_cert: Callable[[Certificate], None] | None = None,
    ) -> None:
        self.validator_id = validator_id
        self.config = config
        self.worker_index = worker_index
        self.worker_count = max(1, worker_count)
        if keyring is not None:
            self.keyring = keyring
        else:
            try:
                self.keyring = create_keyring(
                    config.crypto_scheme,
                    list(unl.keys()),
                    keys_dir=keys_dir,
                    local_id=validator_id,
                )
            except Exception:
                # Fall back to HMAC if Falcon unavailable and scheme requested
                self.keyring = HmacKeyRing.for_unl(list(unl.keys()))
        self.dag = CertificateDAG(validator_id, unl, config, keyring=self.keyring)
        self.mempool = GlobalMempool(config)
        self.ordering = OrderingEngine(self.dag, config)
        self.execution = ExecutionEngine(self.dag, self.ordering, genesis_balances)
        self.on_batch = on_batch
        self.on_sig = on_sig
        self.on_cert = on_cert
        self._last_batch = 0.0
        self.batches_created = 0
        self.certs_seen = 0
        # Fix 7: count legacy full-scan certify attempts (should stay 0 post-Fix 2)
        self.pending_storm_count = 0

    def owns_tx(self, tx_id: str, first_seen: float | None = None, now: float | None = None) -> bool:
        """Narwhal-style worker shard: prefer txs mapped to this worker.

        Orphan rule (Fix 5): non-primary workers adopt a tx with a deterministic
        linear ramp over ``shard_orphan_age_s`` so a silent shard does not stall,
        without instantly granting every packer every tx (as ``first_seen=i`` did).
        """
        if not self.config.worker_sharding or self.worker_count <= 1:
            return True
        primary = int(tx_id[:8], 16) % self.worker_count == self.worker_index
        if primary:
            return True
        if first_seen is None:
            return False
        now = now if now is not None else time.time()
        age = now - float(first_seen)
        orphan = float(self.config.shard_orphan_age_s)
        if orphan <= 0:
            return True
        if age >= orphan:
            return True
        if age <= 0:
            return False
        # Deterministic adoption ramp: fraction age/orphan of non-primary txs.
        frac = age / orphan
        bucket = int(tx_id[:8], 16) % 10_000
        return bucket < int(frac * 10_000)

    def submit_tx(self, tx: Transaction) -> bool:
        self.dag.ingest_tx(tx)
        return self.mempool.add(tx)

    def _validate_batch(self, batch: Batch) -> bool:
        if len(batch.parents) > self.config.max_batch_parents:
            return False
        if len(batch.tx_ids) > self.config.max_batch_txs:
            return False
        if not batch.parents and batch.batch_hash != GENESIS_BATCH_HASH:
            return False
        expected = Batch.compute_hash(
            batch.tx_ids, batch.parents, batch.creator, batch.timestamp
        )
        if expected != batch.batch_hash:
            return False
        return True

    def _sign_batch(self, batch_hash: str) -> PartialSignature:
        sig = self.dag.sign_local(batch_hash, self.validator_id)
        self.dag.add_signature(batch_hash, self.validator_id, sig)
        return PartialSignature(
            batch_hash=batch_hash,
            validator_id=self.validator_id,
            signature=sig,
        )

    def on_remote_batch(self, batch: Batch) -> PartialSignature | None:
        if not self._validate_batch(batch):
            self.dag.rejected_batches += 1
            return None
        self.dag.ingest_batch(batch)
        for t in batch.txs.values():
            self.dag.ingest_tx(t)
            self.mempool.remove(t.tx_id)
        # Sign authenticated attestation
        psig = self._sign_batch(batch.batch_hash)
        cert = self.dag.certificates.get(batch.batch_hash)
        # try certify after our sig
        new_cert = self.dag._try_certify(batch.batch_hash)
        if self.on_sig:
            self.on_sig(psig)
        if new_cert:
            self._on_new_cert(new_cert)
        elif cert is None:
            # Threshold not met yet; no full-scan storm (Fix 2).
            pass
        return psig

    def on_remote_sig(self, sig: PartialSignature) -> Certificate | None:
        cert = self.dag.add_signature(sig.batch_hash, sig.validator_id, sig.signature)
        if cert:
            self._on_new_cert(cert)
        return cert

    def _certify_children(self, batch_hash: str) -> None:
        """Fix 2: only retry direct children when a parent certifies (no full scan)."""
        for child_hash in list(self.dag.children.get(batch_hash, ())):
            if child_hash in self.dag.certificates:
                continue
            cert = self.dag._try_certify(child_hash)
            if cert:
                self._on_new_cert(cert)
                # recurse via _on_new_cert → _certify_children

    def _certify_pending(self) -> None:
        """Deprecated full-scan path — kept for API compat; no longer used on hot path."""
        self.pending_storm_count += 1
        for bh in list(self.dag.batches.keys()):
            if bh in self.dag.certificates:
                continue
            cert = self.dag._try_certify(bh)
            if cert:
                self._on_new_cert(cert)

    def on_remote_cert(self, cert: Certificate) -> None:
        if cert.batch_hash in self.dag.certificates:
            self.advance_ordering()
            return
        batch = self.dag.batches.get(cert.batch_hash)
        if batch is not None:
            # Import signers from cert metadata as already-thresholded — still need sigs on batch
            # Trust path: if we have batch body, re-check local threshold only
            new_cert = self.dag._try_certify(cert.batch_hash)
            if new_cert:
                self._on_new_cert(new_cert, emit=False)
            elif cert.batch_hash not in self.dag.certificates:
                # Accept remote cert only if we can verify enough sigs already on batch
                if self.dag.support_weight(batch) + 1e-12 >= self.config.unl_threshold:
                    self.dag.certificates[cert.batch_hash] = cert
                    self.dag.certs_by_id[cert.cert_id] = cert
                    self.dag.note_remote_cert_installed(cert)
                    self._on_new_cert(cert, emit=False)
        else:
            # Store cert header; wait for batch body for execution
            self.dag.certificates[cert.batch_hash] = cert
            self.dag.certs_by_id[cert.cert_id] = cert
            self.dag.note_remote_cert_installed(cert)
            self._on_new_cert(cert, emit=False)

    def _on_new_cert(self, cert: Certificate, emit: bool = True) -> None:
        self.certs_seen += 1
        self.execution.refresh_soft_final()
        batch = self.dag.batches.get(cert.batch_hash)
        if batch:
            self.mempool.mark_done(list(batch.tx_ids))
        if emit and self.on_cert:
            self.on_cert(cert)
        self.advance_ordering()
        # Fix 2: cascade certify children blocked only on this parent
        self._certify_children(cert.batch_hash)

    def effective_batch_interval(self) -> float:
        if not self.config.adaptive_interval:
            return self.config.target_batch_interval_s
        depth = len(self.mempool)
        lo = self.config.adaptive_mempool_low
        hi = max(lo + 1, self.config.adaptive_mempool_high)
        if depth <= lo:
            t = 0.0
        elif depth >= hi:
            t = 1.0
        else:
            t = (depth - lo) / float(hi - lo)
        return (
            self.config.batch_interval_min_s
            + t * (self.config.batch_interval_max_s - self.config.batch_interval_min_s)
        )

    def create_batch(self, force: bool = False) -> Batch | None:
        now = time.time()
        interval = self.effective_batch_interval()
        if not force and (now - self._last_batch) < interval:
            return None
        if len(self.mempool) == 0:
            return None

        tips = self.dag.certified_tips()[: self.config.max_batch_parents]
        if not tips:
            tips = [GENESIS_BATCH_HASH]

        already: set[str] = set()
        for b in self.dag.batches.values():
            already.update(b.tx_ids)

        state = self.execution.state.clone()
        chosen: list[Transaction] = []
        for tx in self.mempool.ordered(now):
            if len(chosen) >= self.config.max_batch_txs:
                break
            if tx.tx_id in already:
                continue
            if not self.owns_tx(tx.tx_id, first_seen=tx.first_seen, now=now):
                continue
            # Fix 4: skip account/sequence already in an uncertified batch (any packer).
            pending = self.dag.pending_seq(tx.account)
            if pending and tx.sequence <= pending:
                continue
            if not state.can_apply(tx):
                continue
            state.apply(tx)
            chosen.append(tx)

        if not chosen:
            return None

        batch = Batch.create(
            txs=chosen, parents=tips, creator=self.validator_id, timestamp=now
        )
        self._last_batch = now
        self.batches_created += 1
        for t in chosen:
            self.mempool.remove(t.tx_id)
        self.dag.ingest_batch(batch)
        psig = self._sign_batch(batch.batch_hash)
        # attach our sig onto batch object for gossip
        batch.signatures[self.validator_id] = psig.signature
        if self.on_batch:
            self.on_batch(batch)
        if self.on_sig:
            self.on_sig(psig)
        new_cert = self.dag._try_certify(batch.batch_hash)
        if new_cert:
            self._on_new_cert(new_cert)
        return batch

    def advance_ordering(self) -> list[Certificate]:
        newly = self.ordering.try_commit()
        if newly:
            applied = self.execution.apply_new_ordered()
            if applied:
                self.mempool.mark_done([t.tx_id for t in applied])
        return newly

    def tick(self) -> Batch | None:
        batch = self.create_batch(force=False)
        self.advance_ordering()
        # Housekeeping: drop stale uncertified spam / old hard-final history
        self.dag.prune_stale_uncertified()
        self.dag.prune_hard_final_history(
            self.ordering.hard_final, self.ordering.ordered
        )
        return batch

    def export_snapshot(self) -> dict:
        from falcon_dag.persist import engine_to_snapshot

        return engine_to_snapshot(self)

    def import_snapshot(self, snap: dict) -> None:
        from falcon_dag.persist import apply_snapshot

        apply_snapshot(self, snap)

    def metrics(self) -> dict:
        da = self.dag.metrics()
        ord_m = self.ordering.metrics()
        return {
            "validator_id": self.validator_id,
            "mempool": len(self.mempool),
            "batches_created": self.batches_created,
            "worker_index": self.worker_index,
            "worker_count": self.worker_count,
            "batch_interval_s": round(self.effective_batch_interval(), 4),
            "da": da,
            "ordering": ord_m,
            "execution": self.execution.metrics(),
            "hard_final_batches": len(self.ordering.hard_final),
            "dos": {
                "max_uncertified": self.config.max_uncertified_batches,
                "uncertified": self.dag.uncertified_count(),
                "max_mempool": self.config.max_mempool_txs,
            },
            # Fix 7 stall-detection metrics
            "uncertified_tip_age_s": da.get("uncertified_tip_age_s"),
            "cert_lag_s": da.get("cert_lag_s"),
            "ordering_lag": ord_m.get("ordering_lag"),
            "pending_storm_count": self.pending_storm_count,
        }
