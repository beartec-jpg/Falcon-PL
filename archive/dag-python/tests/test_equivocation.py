"""Double-sign / equivocation detection tests."""

from __future__ import annotations

from falcon_dag.config import ProtocolConfig
from falcon_dag.sim import NetworkSim
from falcon_dag.types import Batch, Transaction


def test_equivocation_same_parents_different_batches():
    cfg = ProtocolConfig(
        target_batch_interval_s=0.0,
        crypto_scheme="hmac-sha256-dev",
        worker_sharding=False,
        require_sig_verify=True,
        max_batch_txs=8,
    )
    sim = NetworkSim(n=5, config=cfg)
    t1 = Transaction.create("acct1", 1, "acct2", 1, fee=50, first_seen=1.0)
    t2 = Transaction.create("acct2", 1, "acct3", 1, fee=40, first_seen=2.0)
    sim.submit_tx(t1, to=0)
    sim.submit_tx(t2, to=0)
    sim.deliver_all()

    b1 = sim.nodes[0].engine.create_batch(force=True)
    assert b1 is not None
    parents = list(b1.parents)

    # Creator val1 proposes a second batch with the same parents (equivocation)
    b2 = Batch.create(
        txs=[t2],
        parents=parents,
        creator="val1",
        timestamp=b1.timestamp + 0.001,
    )
    sig2 = sim.nodes[0].engine.dag.sign_local(b2.batch_hash, "val1")
    sim.nodes[0].engine.dag.ingest_batch(b2)
    before = sim.nodes[0].engine.dag.equivocation_count
    cert = sim.nodes[0].engine.dag.add_signature(b2.batch_hash, "val1", sig2)
    assert cert is None
    assert sim.nodes[0].engine.dag.equivocation_count > before
    assert "val1" not in sim.nodes[0].engine.dag.batches[b2.batch_hash].signatures


def test_da_sign_parallel_batches_same_parents_ok():
    """Non-creators may sign many batches that share parents."""
    cfg = ProtocolConfig(
        target_batch_interval_s=0.0,
        crypto_scheme="hmac-sha256-dev",
        worker_sharding=False,
        require_sig_verify=True,
        max_batch_txs=4,
    )
    sim = NetworkSim(n=5, config=cfg)
    for i in range(10):
        sim.submit_tx(
            Transaction.create(f"acct{(i%5)+1}", 1 + i // 5, f"acct{((i+1)%5)+1}", 1, fee=10 + i),
            to=i % 5,
        )
    sim.deliver_all()
    made = sim.step(force=True)
    assert len(made) >= 2
    sim.deliver_all()
    # Parallel batches from different creators with same genesis parent should certify
    assert sim.nodes[0].engine.dag.metrics()["certified"] > 1
    assert sim.nodes[0].engine.dag.equivocation_count == 0


def test_same_batch_resign_not_equivocation():
    cfg = ProtocolConfig(
        target_batch_interval_s=0.0,
        crypto_scheme="hmac-sha256-dev",
        worker_sharding=False,
        require_sig_verify=True,
    )
    sim = NetworkSim(n=5, config=cfg)
    tx = Transaction.create("acct1", 1, "acct2", 1, fee=10)
    sim.submit_tx(tx, to=0)
    sim.deliver_all()
    b = sim.nodes[0].engine.create_batch(force=True)
    assert b is not None
    # Re-sign same batch hash — not equivocation
    sig = sim.nodes[0].engine.dag.sign_local(b.batch_hash, "val1")
    before = sim.nodes[0].engine.dag.equivocation_count
    sim.nodes[0].engine.dag.add_signature(b.batch_hash, "val1", sig)
    assert sim.nodes[0].engine.dag.equivocation_count == before
