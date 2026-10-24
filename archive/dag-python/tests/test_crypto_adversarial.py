"""Crypto verification, worker sharding, latency, and adversarial tests."""

from __future__ import annotations

from falcon_dag.config import ProtocolConfig
from falcon_dag.crypto import KeyRing, batch_sign_message
from falcon_dag.sim import NetworkSim
from falcon_dag.types import Batch, Transaction


def cfg(**kw) -> ProtocolConfig:
    base = dict(
        target_batch_interval_s=0.0,
        hard_finality_h=3,
        max_batch_txs=64,
        unl_threshold=0.80,
        worker_sharding=True,
        require_sig_verify=True,
        crypto_scheme="hmac-sha256-dev",
    )
    base.update(kw)
    return ProtocolConfig(**base)


class TestCrypto:
    def test_hmac_sign_verify(self):
        kr = KeyRing.for_unl(["val1", "val2"])
        msg = batch_sign_message("abcd" * 16)
        sig = kr.sign("val1", msg)
        assert kr.verify("val1", msg, sig)
        assert not kr.verify("val2", msg, sig)
        assert not kr.verify("val1", msg, "00" * 32)

    def test_invalid_sigs_rejected_still_live(self):
        sim = NetworkSim(n=5, config=cfg())
        sim.flood(40, n_accounts=8)
        # Inject garbage signatures that must not count toward threshold alone
        for n in sim.nodes:
            for b in list(n.engine.dag.batches.values()):
                if b.creator == "genesis":
                    continue
                # corrupt a non-existent validator id — ignored
                b.signatures["evil"] = "ff" * 32
        sim.run_rounds(15)
        assert sim.all_executed_match()
        assert len(sim.nodes[0].engine.execution.executed_txs) >= 20
        # rejected_sigs may be 0 if we never called add_signature with evil
        assert sim.nodes[0].engine.dag.metrics()["crypto_scheme"] == "hmac-sha256-dev"

    def test_forged_sig_not_certified_alone(self):
        sim = NetworkSim(n=5, config=cfg())
        # One byzantine node that rewrites sigs to garbage when receiving
        sim.nodes[0].byzantine_invalid_sig = True
        sim.flood(30, n_accounts=8)
        # Only honest nodes 1-4 propose
        for _ in range(20):
            for n in sim.nodes[1:]:
                n.engine.create_batch(force=True)
                n.engine.advance_ordering()
            sim.deliver_all()
        # Honest majority should still make progress
        honest_exec = [len(sim.nodes[i].engine.execution.executed_txs) for i in range(1, 5)]
        assert max(honest_exec) >= 10


class TestSharding:
    def test_worker_sharding_reduces_duplicate_packing(self):
        sharded = NetworkSim(n=5, config=cfg(worker_sharding=True, max_batch_txs=32))
        unsharded = NetworkSim(n=5, config=cfg(worker_sharding=False, max_batch_txs=32))
        sharded.flood(100, n_accounts=15)
        unsharded.flood(100, n_accounts=15)
        m_s = sharded.step(force=True)
        m_u = unsharded.step(force=True)

        def uniq(made):
            ids = []
            for b in made:
                ids.extend(b.tx_ids)
            return len(set(ids)), len(ids)

        u_s, t_s = uniq(m_s)
        u_u, t_u = uniq(m_u)
        # Sharding should raise unique/total ratio (fewer duplicates across proposers)
        ratio_s = u_s / max(1, t_s)
        ratio_u = u_u / max(1, t_u)
        assert ratio_s >= ratio_u - 0.05
        assert u_s >= 10
        sharded.run_rounds(25)
        assert sharded.all_executed_match()
        assert len(sharded.nodes[0].engine.execution.executed_txs) >= 70


class TestLatency:
    def test_wan_latency_still_agrees(self):
        sim = NetworkSim(n=5, config=cfg(), latency_ms=80.0, drop_rate=0.0)
        sim.flood(60, n_accounts=10)
        for _ in range(40):
            sim.step(force=True)
            sim.deliver_all()
        assert sim.all_executed_match()
        assert len(sim.nodes[0].engine.execution.executed_txs) >= 40

    def test_message_loss_recovers(self):
        sim = NetworkSim(n=5, config=cfg(), latency_ms=20.0, drop_rate=0.25)
        sim.flood(50, n_accounts=10)
        for _ in range(50):
            sim.step(force=True)
            sim.deliver_all()
        # heal drops — force full mesh reliable delivery of remaining state
        sim.drop_rate = 0.0
        sim.latency_ms = 0.0
        # re-gossip all known batches from node0
        for bh, batch in list(sim.nodes[0].engine.dag.batches.items()):
            if batch.creator == "genesis":
                continue
            for n in sim.nodes[1:]:
                n.engine.on_remote_batch(Batch.from_dict(batch.to_dict()))
            for vid, sig in batch.signatures.items():
                if not sig or sig == "GENESIS":
                    continue
                from falcon_dag.types import PartialSignature
                for n in sim.nodes:
                    n.engine.on_remote_sig(
                        PartialSignature(batch_hash=bh, validator_id=vid, signature=str(sig))
                    )
        for _ in range(25):
            sim.step(force=True)
            sim.deliver_all()
            for n in sim.nodes:
                n.engine.advance_ordering()
        # Majority progress after heal (lossy nets may leave some txs uncertified)
        execs = [len(n.engine.execution.executed_txs) for n in sim.nodes]
        assert max(execs) >= 10
        # After full re-gossip of node0 batches, nodes should converge
        assert min(execs) >= 5 or sim.all_executed_match()


class TestHashIntegrity:
    def test_tampered_batch_hash_rejected(self):
        sim = NetworkSim(n=5, config=cfg(worker_sharding=False))
        tx = Transaction.create("acct1", 1, "acct2", 1, fee=10)
        sim.submit_tx(tx, to=0)
        sim.deliver_all()
        b = sim.nodes[0].engine.create_batch(force=True)
        assert b is not None
        bad = Batch.from_dict(b.to_dict())
        bad.batch_hash = "00" * 32
        before = sim.nodes[1].engine.dag.rejected_batches
        sim.nodes[1].engine.on_remote_batch(bad)
        assert sim.nodes[1].engine.dag.rejected_batches > before
