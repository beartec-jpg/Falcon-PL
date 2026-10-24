"""Local view of batches and the certificate DAG (data availability layer)."""

from __future__ import annotations

import time
from collections import defaultdict, deque

from falcon_dag.config import DEFAULT_CONFIG, ProtocolConfig
from falcon_dag.crypto import CryptoBackend, HmacKeyRing, batch_sign_message
from falcon_dag.types import (
    GENESIS_BATCH_HASH,
    GENESIS_CERT_ID,
    Batch,
    Certificate,
    Transaction,
)


class CertificateDAG:
    """Narwhal-style local DA state."""

    def __init__(
        self,
        validator_id: str,
        unl: dict[str, float],
        config: ProtocolConfig = DEFAULT_CONFIG,
        keyring: CryptoBackend | None = None,
    ) -> None:
        self.validator_id = validator_id
        self.config = config
        self.unl = self._normalize(unl)
        self.keyring = keyring or HmacKeyRing.for_unl(list(self.unl.keys()))

        self.batches: dict[str, Batch] = {}
        self.certificates: dict[str, Certificate] = {}  # by batch_hash
        self.certs_by_id: dict[str, Certificate] = {}
        self.children: dict[str, set[str]] = defaultdict(set)
        self.tx_index: dict[str, Transaction] = {}
        self.rejected_sigs: int = 0
        self.rejected_batches: int = 0
        # Equivocation: validator signed two different batches with the same parent set
        # parent_fingerprint -> batch_hash already attested by validator
        self._attest_by_parents: dict[str, dict[str, str]] = defaultdict(dict)
        self.equivocations: list[dict] = []
        self.equivocation_count: int = 0
        # Rate limit: validator_id -> deque of accept timestamps
        self._sig_times: dict[str, deque[float]] = defaultdict(deque)
        self.rate_limited_sigs: int = 0
        self.pruned_batches: int = 0

        # Fix 1: incremental certified tips (O(1) lookup vs full cert scan).
        # Contains certified batch hashes that currently have no certified children.
        self._tip_set: set[str] = {GENESIS_BATCH_HASH}

        # Fix 4: highest sequence per account in any uncertified ingested batch.
        self._pending_seq: dict[str, int] = {}

        # Stall metrics (Fix 7 helpers)
        self._cert_create_lag_sum: float = 0.0
        self._cert_create_lag_n: int = 0

        # Genesis certified anchor
        gen_sigs = {v: "GENESIS" for v in self.unl}
        gen = Certificate(
            cert_id=GENESIS_CERT_ID,
            batch_hash=GENESIS_BATCH_HASH,
            signers=sorted(self.unl.keys()),
            parents=[],
            creator="genesis",
            timestamp=0.0,
            round=0,
        )
        self.certificates[GENESIS_BATCH_HASH] = gen
        self.certs_by_id[GENESIS_CERT_ID] = gen
        self.batches[GENESIS_BATCH_HASH] = Batch(
            batch_hash=GENESIS_BATCH_HASH,
            tx_ids=[],
            parents=[],
            creator="genesis",
            timestamp=0.0,
            signatures=gen_sigs,
        )

    @staticmethod
    def _normalize(unl: dict[str, float]) -> dict[str, float]:
        total = sum(unl.values())
        if total <= 0:
            n = max(1, len(unl))
            return {k: 1.0 / n for k in unl} if unl else {}
        return {k: v / total for k, v in unl.items()}

    def _sig_ok(self, validator_id: str, batch_hash: str, signature: str) -> bool:
        if batch_hash == GENESIS_BATCH_HASH:
            return True
        if not self.config.require_sig_verify:
            return bool(signature) or signature == ""
        if signature == "GENESIS":
            return False
        return self.keyring.verify(
            validator_id, batch_sign_message(batch_hash), signature
        )

    def support_weight(self, batch: Batch) -> float:
        w = 0.0
        for vid, sig in batch.signatures.items():
            if vid not in self.unl:
                continue
            if isinstance(sig, bool):
                # legacy
                if sig:
                    w += self.unl[vid]
                continue
            if self._sig_ok(vid, batch.batch_hash, str(sig)):
                w += self.unl[vid]
        return w

    def is_certified(self, batch_hash: str) -> bool:
        return batch_hash in self.certificates

    def certified_tips(self) -> list[str]:
        """O(tips) — maintained incrementally on certify (Fix 1)."""
        tips = sorted(bh for bh in self._tip_set if bh in self.certificates)
        return tips or [GENESIS_BATCH_HASH]

    def pending_seq(self, account: str) -> int:
        """Highest sequence already in an uncertified batch for ``account``."""
        return int(self._pending_seq.get(account, 0))

    def ingest_tx(self, tx: Transaction) -> None:
        self.tx_index[tx.tx_id] = tx

    def uncertified_count(self) -> int:
        return sum(1 for bh in self.batches if bh not in self.certificates)

    def uncertified_by_creator(self, creator: str) -> int:
        return sum(
            1
            for bh, b in self.batches.items()
            if bh not in self.certificates and b.creator == creator
        )

    def _note_pending_from_batch(self, batch: Batch) -> None:
        for tx in batch.txs.values():
            acct = getattr(tx, "account", None) or getattr(tx, "source", None)
            seq = getattr(tx, "sequence", None)
            if acct is None or seq is None:
                continue
            cur = self._pending_seq.get(acct, 0)
            if int(seq) > cur:
                self._pending_seq[acct] = int(seq)

    def _recompute_pending_seq(self) -> None:
        """Rebuild in-flight sequence map from remaining uncertified batches."""
        self._pending_seq.clear()
        for bh, batch in self.batches.items():
            if bh == GENESIS_BATCH_HASH or bh in self.certificates:
                continue
            self._note_pending_from_batch(batch)

    def _on_batch_certified(self, batch_hash: str, parents: list[str]) -> None:
        """Maintain tip set after a new certificate is installed (Fix 1)."""
        self._tip_set.add(batch_hash)
        for p in parents:
            # Parent is no longer a tip once it has a certified child.
            if any(k in self.certificates for k in self.children.get(p, ())):
                self._tip_set.discard(p)
        # Certified batch leaves the uncertified pending-seq set.
        self._recompute_pending_seq()

    def ingest_batch(self, batch: Batch) -> bool:
        """Merge batch; return True if new."""
        if batch.batch_hash == GENESIS_BATCH_HASH:
            return False
        if len(batch.parents) > self.config.max_batch_parents:
            self.rejected_batches += 1
            return False
        # Recompute hash integrity
        expected = Batch.compute_hash(
            batch.tx_ids, batch.parents, batch.creator, batch.timestamp
        )
        if expected != batch.batch_hash:
            self.rejected_batches += 1
            return False

        is_new = batch.batch_hash not in self.batches
        if is_new:
            # DoS bounds
            if len(self.batches) >= self.config.max_known_batches:
                self.rejected_batches += 1
                return False
            if self.uncertified_count() >= self.config.max_uncertified_batches:
                self.rejected_batches += 1
                return False
            if (
                self.uncertified_by_creator(batch.creator)
                >= self.config.max_uncertified_per_creator
            ):
                self.rejected_batches += 1
                return False
            self.batches[batch.batch_hash] = batch
            for p in batch.parents:
                self.children[p].add(batch.batch_hash)
            # Fix 4: track in-flight sequences while uncertified
            self._note_pending_from_batch(batch)
        else:
            existing = self.batches[batch.batch_hash]
            for vid, sig in batch.signatures.items():
                if vid not in existing.signatures:
                    if self._sig_ok(vid, batch.batch_hash, str(sig)):
                        existing.signatures[vid] = str(sig)
                    else:
                        self.rejected_sigs += 1
            existing.txs.update(batch.txs)
            batch = existing
            if batch.batch_hash not in self.certificates:
                self._note_pending_from_batch(batch)
        for t in batch.txs.values():
            self.tx_index[t.tx_id] = t
        for tid in batch.tx_ids:
            if tid in batch.txs:
                self.tx_index[tid] = batch.txs[tid]
        self._try_certify(batch.batch_hash)
        return is_new

    @staticmethod
    def parents_fingerprint(parents: list[str]) -> str:
        return ",".join(sorted(parents))

    def _check_equivocation(self, validator_id: str, batch: Batch) -> bool:
        """Return True if this is a creator double-proposal (equivocation).

        Only the **batch creator** is bound: they must not propose two different
        batches with the same parent set. Other validators may freely DA-sign
        many parallel batches that share parents (normal Narwhal behaviour).
        """
        if batch.batch_hash == GENESIS_BATCH_HASH:
            return False
        if batch.creator != validator_id:
            return False
        fp = self.parents_fingerprint(batch.parents)
        prev = self._attest_by_parents[validator_id].get(fp)
        if prev is None:
            self._attest_by_parents[validator_id][fp] = batch.batch_hash
            return False
        if prev == batch.batch_hash:
            return False
        self.equivocation_count += 1
        self.equivocations.append(
            {
                "validator_id": validator_id,
                "parents": list(batch.parents),
                "first_batch": prev,
                "second_batch": batch.batch_hash,
            }
        )
        return True

    def _rate_limit_ok(self, validator_id: str) -> bool:
        """Sliding-window limit on accepted signature messages per validator."""
        now = time.time()
        window = self.config.sig_rate_window_s
        limit = self.config.max_sigs_per_validator_per_s
        if limit <= 0:
            return True
        q = self._sig_times[validator_id]
        while q and (now - q[0]) > window:
            q.popleft()
        # max events in window = rate * window (at least 1)
        max_n = max(1, int(limit * window + 0.999))
        if len(q) >= max_n:
            self.rate_limited_sigs += 1
            return False
        q.append(now)
        return True

    def add_signature(
        self, batch_hash: str, validator_id: str, signature: str
    ) -> Certificate | None:
        """Add verified signature. Return Certificate only if newly certified."""
        if validator_id not in self.unl:
            return None
        batch = self.batches.get(batch_hash)
        if batch is None:
            return None
        # Idempotent re-submit of same validator's existing sig: no rate cost
        if validator_id in batch.signatures and str(batch.signatures[validator_id]) == str(
            signature
        ):
            return self._try_certify(batch_hash)
        if not self._rate_limit_ok(validator_id):
            return None
        if not self._sig_ok(validator_id, batch_hash, signature):
            self.rejected_sigs += 1
            return None
        if self._check_equivocation(validator_id, batch):
            # Valid crypto, but double-sign on same parent set — do not count vote
            return None
        batch.signatures[validator_id] = signature
        return self._try_certify(batch_hash)

    def sign_local(self, batch_hash: str, validator_id: str) -> str:
        return self.keyring.sign(validator_id, batch_sign_message(batch_hash))

    def prune_stale_uncertified(self, now: float | None = None) -> int:
        """Drop uncertified batches older than TTL. Returns number pruned."""
        now = now if now is not None else time.time()
        ttl = self.config.uncertified_ttl_s
        if ttl <= 0:
            return 0
        to_drop: list[str] = []
        for bh, batch in self.batches.items():
            if bh == GENESIS_BATCH_HASH or bh in self.certificates:
                continue
            if (now - float(batch.timestamp)) > ttl:
                to_drop.append(bh)
        for bh in to_drop:
            self._drop_batch(bh)
            self.pruned_batches += 1
        if to_drop:
            self._recompute_pending_seq()
        return len(to_drop)

    def _drop_batch(self, batch_hash: str) -> None:
        batch = self.batches.pop(batch_hash, None)
        if batch is None:
            return
        for p in batch.parents:
            kids = self.children.get(p)
            if kids is not None:
                kids.discard(batch_hash)
        was_cert = batch_hash in self.certificates
        self.certificates.pop(batch_hash, None)
        self._tip_set.discard(batch_hash)
        if was_cert:
            # Parent may become a tip again if it lost its last certified child.
            for p in batch.parents:
                if p in self.certificates:
                    kids = self.children.get(p, set())
                    if not any(k in self.certificates for k in kids):
                        self._tip_set.add(p)
        # certs_by_id may still hold orphaned cert ids — leave harmless

    def prune_hard_final_history(self, hard_final: set[str], ordered: list[str]) -> int:
        """Optionally drop very old hard-final batches not needed for tips.

        Keeps genesis, non-hard-final, and the newest max_hard_final_history
        hard-final batches along the ordered list.
        """
        cap = self.config.max_hard_final_history
        if cap <= 0:
            return 0
        hard_ordered = [bh for bh in ordered if bh in hard_final and bh != GENESIS_BATCH_HASH]
        if len(hard_ordered) <= cap:
            return 0
        keep = set(hard_ordered[-cap:])
        keep.add(GENESIS_BATCH_HASH)
        # Never drop batches that are still tips or have uncertified children
        tips = set(self.certified_tips())
        dropped = 0
        for bh in hard_ordered[:-cap]:
            if bh in tips or bh in keep:
                continue
            if any(c not in self.certificates for c in self.children.get(bh, ())):
                continue
            # Only drop if all children are also hard-final/old or none
            self._drop_batch(bh)
            dropped += 1
            self.pruned_batches += 1
        return dropped

    def _try_certify(self, batch_hash: str) -> Certificate | None:
        if batch_hash in self.certificates:
            return None
        batch = self.batches.get(batch_hash)
        if batch is None:
            return None
        if self.support_weight(batch) + 1e-12 < self.config.unl_threshold:
            return None
        parent_rounds: list[int] = []
        for p in batch.parents:
            c = self.certificates.get(p)
            if c is None:
                return None
            parent_rounds.append(c.round)
        rnd = (max(parent_rounds) + 1) if parent_rounds else 1
        cert = Certificate.from_batch(batch, round_num=rnd)
        self.certificates[batch_hash] = cert
        self.certs_by_id[cert.cert_id] = cert
        # Fix 1 tip maintenance + Fix 4 pending recompute
        self._on_batch_certified(batch_hash, list(batch.parents))
        # Fix 7: cert latency sample
        try:
            lag = max(0.0, time.time() - float(batch.timestamp))
            self._cert_create_lag_sum += lag
            self._cert_create_lag_n += 1
        except (TypeError, ValueError):
            pass
        return cert

    def note_remote_cert_installed(self, cert: Certificate) -> None:
        """Tip-set maintenance when a remote cert is installed without _try_certify."""
        if cert.batch_hash not in self.certificates:
            self.certificates[cert.batch_hash] = cert
            self.certs_by_id[cert.cert_id] = cert
        parents = list(cert.parents or [])
        batch = self.batches.get(cert.batch_hash)
        if batch is not None:
            parents = list(batch.parents)
        self._on_batch_certified(cert.batch_hash, parents)

    def oldest_uncertified_tip_age_s(self, now: float | None = None) -> float | None:
        """Age of the oldest uncertified batch that has a certified parent (blocks tips)."""
        now = now if now is not None else time.time()
        oldest: float | None = None
        for bh, batch in self.batches.items():
            if bh == GENESIS_BATCH_HASH or bh in self.certificates:
                continue
            # Blocking if any parent is certified (child waiting on more parents/sigs)
            if not batch.parents or any(p in self.certificates for p in batch.parents):
                age = now - float(batch.timestamp)
                if oldest is None or age > oldest:
                    oldest = age
        return oldest

    def average_cert_sig_bytes(self) -> float | None:
        """Mean signature size across non-genesis certified batches."""
        sizes: list[int] = []
        for bh, batch in self.batches.items():
            if bh == GENESIS_BATCH_HASH:
                continue
            if bh not in self.certificates:
                continue
            for vid, sig in batch.signatures.items():
                if not sig or sig == "GENESIS":
                    continue
                try:
                    sizes.append(len(bytes.fromhex(str(sig))))
                except ValueError:
                    continue
        if not sizes:
            return None
        return round(sum(sizes) / len(sizes), 1)

    def metrics(self) -> dict:
        avg_sig = self.average_cert_sig_bytes()
        n_cert = max(0, len(self.certificates) - 1)  # exclude genesis
        cert_lag = (
            self._cert_create_lag_sum / self._cert_create_lag_n
            if self._cert_create_lag_n
            else 0.0
        )
        return {
            "batches": len(self.batches),
            "certified": len(self.certificates),
            "tips": self.certified_tips(),
            "tip_count": len(self.certified_tips()),
            "rejected_sigs": self.rejected_sigs,
            "rejected_batches": self.rejected_batches,
            "equivocations": self.equivocation_count,
            "rate_limited_sigs": self.rate_limited_sigs,
            "pruned_batches": self.pruned_batches,
            "uncertified": self.uncertified_count(),
            "crypto_scheme": self.keyring.scheme,
            "crypto": self.keyring.metrics() if hasattr(self.keyring, "metrics") else {},
            "avg_sig_bytes": avg_sig,
            "est_cert_bytes_4sigs": (
                round(32 + 4 * avg_sig, 1) if avg_sig is not None else None
            ),
            "non_genesis_certs": n_cert,
            "uncertified_tip_age_s": self.oldest_uncertified_tip_age_s(),
            "cert_lag_s": round(cert_lag, 4),
        }
