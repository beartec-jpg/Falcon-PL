"""Signature rate limits, pruning, and restart recovery drills."""

from __future__ import annotations

import time

from falcon_dag.config import ProtocolConfig
from falcon_dag.persist import load_snapshot, save_snapshot
from falcon_dag.sim import NetworkSim
from falcon_dag.types import Batch, PartialSignature, Transaction


def _cfg(**kw) -> ProtocolConfig:
    base = dict(
        target_batch_interval_s=0.0,
        crypto_scheme="hmac-sha256-dev",
        worker_sharding=False,
        shard_orphan_age_s=0.0,
        require_sig_verify=True,
        hard_finality_h=2,
        max_batch_txs=32,
        uncertified_ttl_s=1.0,
        max_sigs_per_validator_per_s=5.0,
        sig_rate_window_s=1.0,
    )
    base.update(kw)
    return ProtocolConfig(**base)


def test_sig_rate_limit_trips():
    cfg = _cfg(max_sigs_per_validator_per_s=3.0, sig_rate_window_s=1.0)
    sim = NetworkSim(n=5, config=cfg)
    eng = sim.nodes[0].engine
    # Create one real batch so signatures have a target
    tx = Transaction.create("acct1", 1, "acct2", 1, fee=10)
    eng.submit_tx(tx)
    b = eng.create_batch(force=True)
    assert b is not None
    # Flood distinct validators' messages is fine; flood SAME validator
    # with different batch targets — craft extra batches
    parents = eng.dag.certified_tips()[:1]
    accepted = 0
    limited = 0
    for i in range(20):
        t = Transaction.create("acct2", i + 1, "acct3", 1, fee=10, first_seen=float(i))
        fake = Batch.create(
            txs=[t], parents=parents, creator="val2", timestamp=time.time()
        )
        eng.dag.ingest_batch(fake)
        # val1 signs many different batches rapidly
        sig = eng.dag.sign_local(fake.batch_hash, "val1")
        before = eng.dag.rate_limited_sigs
        eng.dag.add_signature(fake.batch_hash, "val1", sig)
        if eng.dag.rate_limited_sigs > before:
            limited += 1
        else:
            accepted += 1
    assert limited >= 1
    assert accepted <= 5  # window allows ~3-5


def test_prune_stale_uncertified():
    cfg = _cfg(uncertified_ttl_s=0.05)
    sim = NetworkSim(n=5, config=cfg)
    eng = sim.nodes[0].engine
    parents = eng.dag.certified_tips()[:1]
    for i in range(3):
        t = Transaction.create("acct1", i + 1, "acct2", 1, fee=10, first_seen=float(i))
        # old timestamp so TTL fires
        b = Batch.create(
            txs=[t], parents=parents, creator="val2", timestamp=time.time() - 10
        )
        eng.dag.ingest_batch(b)
    assert eng.dag.uncertified_count() >= 1
    n = eng.dag.prune_stale_uncertified(now=time.time())
    assert n >= 1
    assert eng.dag.pruned_batches >= 1


def test_restart_recovery_drill(tmp_path):
    """Simulate kill/restart: run network, snapshot, new engine loads state."""
    cfg = _cfg(hard_finality_h=2, max_batch_txs=16)
    sim = NetworkSim(n=5, config=cfg)
    sim.flood(40, n_accounts=10)
    sim.run_rounds(15)
    eng = sim.nodes[0].engine
    executed = list(eng.execution.executed_txs)
    certified = eng.dag.metrics()["certified"]
    assert len(executed) >= 20

    path = tmp_path / "node-snap.json"
    save_snapshot(path, eng)

    # Fresh process equivalent
    sim2 = NetworkSim(n=5, config=cfg)
    eng2 = sim2.nodes[0].engine
    assert load_snapshot(path, eng2)
    assert eng2.dag.metrics()["certified"] >= certified - 1
    # Recovered execution should match closely
    recovered = set(eng2.execution.executed_txs)
    assert len(recovered & set(executed)) >= int(len(executed) * 0.9)
    # Continue after restart
    sim2.flood(20, n_accounts=10, start_seq=10)
    sim2.run_rounds(12)
    assert len(sim2.nodes[0].engine.execution.executed_txs) > len(executed) * 0.9
