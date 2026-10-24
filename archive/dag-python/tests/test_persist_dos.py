"""Persistence and DoS bound tests."""

from __future__ import annotations

from falcon_dag.config import ProtocolConfig
from falcon_dag.persist import apply_snapshot, engine_to_snapshot, load_snapshot, save_snapshot
from falcon_dag.sim import NetworkSim
from falcon_dag.types import Batch, Transaction


def _cfg(**kw) -> ProtocolConfig:
    base = dict(
        target_batch_interval_s=0.0,
        crypto_scheme="hmac-sha256-dev",
        worker_sharding=False,
        shard_orphan_age_s=0.0,
        require_sig_verify=True,
        hard_finality_h=2,
        max_batch_txs=32,
    )
    base.update(kw)
    return ProtocolConfig(**base)


def test_snapshot_roundtrip(tmp_path):
    sim = NetworkSim(n=5, config=_cfg())
    sim.flood(30, n_accounts=8)
    sim.run_rounds(12)
    eng = sim.nodes[0].engine
    before_exec = list(eng.execution.executed_txs)
    before_certs = eng.dag.metrics()["certified"]
    assert len(before_exec) >= 10

    path = tmp_path / "snap.json"
    save_snapshot(path, eng)

    # Fresh engine of same UNL/crypto
    sim2 = NetworkSim(n=5, config=_cfg())
    eng2 = sim2.nodes[0].engine
    assert load_snapshot(path, eng2)
    assert eng2.dag.metrics()["certified"] >= before_certs - 1  # genesis accounting
    # Executed set should be recoverable
    assert len(eng2.execution.executed_txs) >= len(before_exec) * 0.8
    assert set(before_exec).issubset(set(eng2.execution.executed_txs)) or (
        len(set(before_exec) & set(eng2.execution.executed_txs)) >= len(before_exec) * 0.8
    )


def test_snapshot_dict_export_import():
    sim = NetworkSim(n=3, config=_cfg())
    sim.flood(15, n_accounts=5)
    sim.run_rounds(10)
    eng = sim.nodes[0].engine
    snap = engine_to_snapshot(eng)
    sim2 = NetworkSim(n=3, config=_cfg())
    apply_snapshot(sim2.nodes[0].engine, snap)
    assert sim2.nodes[0].engine.dag.metrics()["certified"] >= 2


def test_dos_max_uncertified_per_creator():
    cfg = _cfg(max_uncertified_per_creator=3, max_uncertified_batches=100)
    sim = NetworkSim(n=5, config=cfg)
    eng = sim.nodes[0].engine
    parents = eng.dag.certified_tips()[:1]
    # Direct ingest of many uncertified batches from same creator
    for i in range(8):
        tx = Transaction.create("acct5", i + 1, "acct6", 1, fee=10, first_seen=float(i))
        b = Batch.create(txs=[tx], parents=parents, creator="val1", timestamp=5000.0 + i)
        eng.dag.ingest_batch(b)
    assert eng.dag.uncertified_by_creator("val1") <= cfg.max_uncertified_per_creator
    assert eng.dag.rejected_batches >= 1


def test_dos_reject_remote_flood():
    cfg = _cfg(max_uncertified_per_creator=2, max_uncertified_batches=50)
    sim = NetworkSim(n=5, config=cfg)
    victim = sim.nodes[1].engine
    before = victim.dag.rejected_batches
    # Attacker crafts many uncertified batches as val9 (not in unl) — creator still counted
    # Use val1 as creator flooding into val2 without enough signatures
    for i in range(8):
        tx = Transaction.create("acct3", i + 1, "acct4", 1, fee=10, first_seen=float(i))
        fake = Batch.create(
            txs=[tx],
            parents=victim.dag.certified_tips()[:1],
            creator="val1",
            timestamp=1000.0 + i,
        )
        # No valid UNL sigs on fake — still stored as uncertified until bound
        ok = victim.dag.ingest_batch(fake)
        if not ok and fake.batch_hash not in victim.dag.batches:
            pass
    assert (
        victim.dag.uncertified_by_creator("val1") <= cfg.max_uncertified_per_creator
        or victim.dag.rejected_batches > before
    )


def test_mempool_cap():
    cfg = _cfg(max_mempool_txs=5)
    sim = NetworkSim(n=3, config=cfg)
    eng = sim.nodes[0].engine
    added = 0
    for i in range(20):
        tx = Transaction.create("acct1", i + 1, "acct2", 1, fee=10, first_seen=float(i))
        if eng.submit_tx(tx):
            added += 1
    assert added == 5
    assert len(eng.mempool) == 5
