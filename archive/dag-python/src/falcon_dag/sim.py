"""In-process multi-validator Narwhal + ordering simulation."""

from __future__ import annotations

import time

import random
from dataclasses import dataclass, field
from typing import Iterable

from falcon_dag.config import DEFAULT_CONFIG, ProtocolConfig
from falcon_dag.crypto import create_keyring, falcon_available
from falcon_dag.engine import FalconDAGEngine
from falcon_dag.types import Batch, Certificate, PartialSignature, Transaction


@dataclass
class SimNode:
    validator_id: str
    engine: FalconDAGEngine
    inbox: list[dict] = field(default_factory=list)
    active: bool = True
    # Adversarial flags
    byzantine_invalid_sig: bool = False
    byzantine_bad_batch: bool = False


class NetworkSim:
    def __init__(
        self,
        n: int = 5,
        config: ProtocolConfig = DEFAULT_CONFIG,
        genesis_balances: dict[str, int] | None = None,
        latency_ms: float = 0.0,
        drop_rate: float = 0.0,
        seed: int = 42,
    ) -> None:
        self.config = config
        self.latency_ms = latency_ms
        self.drop_rate = drop_rate
        self.rng = random.Random(seed)
        # delayed messages: list of (deliver_at_logical, msg)
        self._delayed: list[tuple[float, dict]] = []
        self._clock = 0.0

        ids = [f"val{i+1}" for i in range(n)]
        unl = {vid: 1.0 / n for vid in ids}
        scheme = getattr(config, "crypto_scheme", "falcon-512")
        if scheme.startswith("falcon") and not falcon_available():
            scheme = "hmac-sha256-dev"
        # One shared ring so all nodes verify the same UNL public keys
        self.keyring = create_keyring(scheme, ids)
        self.nodes: list[SimNode] = []
        self.partitions: list[set[str]] | None = None

        for i, vid in enumerate(ids):
            node = SimNode(validator_id=vid, engine=None)  # type: ignore[arg-type]
            eng = FalconDAGEngine(
                validator_id=vid,
                unl=unl,
                config=config,
                genesis_balances=genesis_balances,
                keyring=self.keyring,
                worker_index=i,
                worker_count=n,
                on_batch=lambda b, _n=node: self._broadcast(
                    {"type": "batch", "batch": b, "from": _n.validator_id}
                ),
                on_sig=lambda s, _n=node: self._broadcast(
                    {"type": "sig", "sig": s, "from": _n.validator_id}
                ),
                on_cert=lambda c, _n=node: self._broadcast(
                    {"type": "cert", "cert": c, "from": _n.validator_id}
                ),
            )
            node.engine = eng
            node._seen_certs = set()  # type: ignore[attr-defined]
            self.nodes.append(node)

    def set_partition(self, groups: list[Iterable[str]] | None) -> None:
        if groups is None:
            self.partitions = None
        else:
            self.partitions = [set(g) for g in groups]

    def _can_talk(self, a: str, b: str) -> bool:
        if a == b:
            return False
        if self.partitions is None:
            return True
        for g in self.partitions:
            if a in g and b in g:
                return True
        return False

    def _broadcast(self, msg: dict) -> None:
        sender = msg.get("from")
        for n in self.nodes:
            if not self._can_talk(str(sender), n.validator_id):
                continue
            if self.drop_rate > 0 and self.rng.random() < self.drop_rate:
                continue
            if self.latency_ms > 0:
                # jitter 50%–150% of base latency
                delay = (self.latency_ms / 1000.0) * self.rng.uniform(0.5, 1.5)
                self._delayed.append((self._clock + delay, {**msg, "_to": n.validator_id}))
            else:
                n.inbox.append(msg)

    def advance_clock(self, dt: float = 0.05) -> int:
        """Advance logical time and deliver due delayed messages."""
        self._clock += dt
        delivered = 0
        still: list[tuple[float, dict]] = []
        for t, msg in self._delayed:
            if t <= self._clock:
                to = msg.get("_to")
                for n in self.nodes:
                    if n.validator_id == to:
                        m = {k: v for k, v in msg.items() if k != "_to"}
                        n.inbox.append(m)
                        delivered += 1
                        break
            else:
                still.append((t, msg))
        self._delayed = still
        return delivered

    def deliver_all(self, max_passes: int = 100) -> int:
        delivered = 0
        # flush delayed if no latency
        if self.latency_ms > 0:
            # pump clock enough to drain typical delays
            for _ in range(50):
                delivered += self.advance_clock(self.latency_ms / 1000.0 * 0.5)
                delivered += self._drain_inboxes()
                if not self._delayed and all(not n.inbox for n in self.nodes):
                    break
        else:
            delivered += self._drain_inboxes(max_passes)
        return delivered

    def _drain_inboxes(self, max_passes: int = 100) -> int:
        delivered = 0
        for _ in range(max_passes):
            progress = False
            for n in self.nodes:
                while n.inbox:
                    msg = n.inbox.pop(0)
                    progress = True
                    delivered += 1
                    self._handle(n, msg)
            if not progress:
                break
        return delivered

    def _handle(self, n: SimNode, msg: dict) -> None:
        if not n.active and msg["type"] != "tx":
            if msg["type"] == "tx":
                t = msg["tx"]
                n.engine.submit_tx(
                    Transaction.from_dict(t.to_dict() if hasattr(t, "to_dict") else t)
                )
            return

        if msg["type"] == "batch":
            b: Batch = msg["batch"]
            remote = Batch.from_dict(b.to_dict())
            if n.byzantine_invalid_sig:
                # corrupt any carried signatures
                remote.signatures = {
                    k: "deadbeef" * 8 for k in remote.signatures
                }
            n.engine.on_remote_batch(remote)
        elif msg["type"] == "sig":
            s: PartialSignature = msg["sig"]
            ps = PartialSignature.from_dict(s.to_dict())
            if n.byzantine_invalid_sig:
                ps.signature = "00" * 32
            n.engine.on_remote_sig(ps)
        elif msg["type"] == "cert":
            c: Certificate = msg["cert"]
            cid = c.cert_id
            seen = getattr(n, "_seen_certs", None)
            if seen is not None and cid in seen:
                return
            if seen is not None:
                seen.add(cid)
            n.engine.on_remote_cert(Certificate.from_dict(c.to_dict()))
        elif msg["type"] == "tx":
            t = msg["tx"]
            if isinstance(t, Transaction):
                n.engine.submit_tx(Transaction.from_dict(t.to_dict()))
            else:
                n.engine.submit_tx(Transaction.from_dict(t))

    def submit_tx(self, tx: Transaction, to: int = 0) -> None:
        self.nodes[to].engine.submit_tx(tx)
        self._broadcast(
            {"type": "tx", "tx": tx, "from": self.nodes[to].validator_id}
        )

    def flood(self, n_txs: int, n_accounts: int = 10, start_seq: int = 1) -> list[Transaction]:
        # Fix 5: real wall-clock timestamps so owns_tx orphan ramp matches production.
        # (Previously first_seen=float(i) made age huge and disabled worker sharding.)
        seq = {f"acct{i}": start_seq for i in range(1, n_accounts + 1)}
        out: list[Transaction] = []
        now = time.time()
        for i in range(n_txs):
            src = f"acct{(i % n_accounts) + 1}"
            dst = f"acct{((i + 1) % n_accounts) + 1}"
            s = seq[src]
            seq[src] = s + 1
            tx = Transaction.create(
                src, s, dst, 1, fee=10 + i % 5, first_seen=now + i * 1e-6
            )
            self.submit_tx(tx, to=i % len(self.nodes))
            out.append(tx)
        self.deliver_all()
        return out

    def step(self, force: bool = True) -> list[Batch]:
        made: list[Batch] = []
        for n in self.nodes:
            if not n.active:
                continue
            if n.byzantine_bad_batch:
                # try to inject a malformed batch hash via create then corrupt
                b = n.engine.create_batch(force=force)
                if b:
                    # already signed; byzantine path tested via invalid sig flag mostly
                    made.append(b)
                continue
            b = n.engine.create_batch(force=force)
            if b:
                made.append(b)
            n.engine.advance_ordering()
        self.deliver_all()
        for n in self.nodes:
            n.engine.advance_ordering()
        return made

    def run_rounds(self, rounds: int = 20) -> None:
        for _ in range(rounds):
            self.step(force=True)
            self.deliver_all()

    def all_executed_match(self) -> bool:
        bases = [tuple(n.engine.execution.executed_txs) for n in self.nodes]
        return all(b == bases[0] for b in bases)

    def all_order_match(self) -> bool:
        bases = [tuple(n.engine.ordering.ordered) for n in self.nodes]
        return all(b == bases[0] for b in bases)

    def metrics(self) -> list[dict]:
        return [n.engine.metrics() for n in self.nodes]
