"""Tests for Narwhal-style DA + federated ordering."""

from __future__ import annotations

import pytest

from falcon_dag.config import ProtocolConfig
from falcon_dag.sim import NetworkSim
from falcon_dag.types import Transaction


def cfg(**kw) -> ProtocolConfig:
    base = dict(
        target_batch_interval_s=0.0,
        hard_finality_h=3,
        max_batch_txs=64,
        unl_threshold=0.80,
        max_batch_parents=6,
        worker_sharding=True,
        shard_orphan_age_s=0.0,  # sim: any worker can claim immediately
        require_sig_verify=True,
        # Unit suite stays on HMAC for speed; Falcon-512 covered in test_falcon512.py
        crypto_scheme="hmac-sha256-dev",
    )
    base.update(kw)
    return ProtocolConfig(**base)


class TestAgreement:
    def test_high_load_executed_match(self):
        sim = NetworkSim(n=5, config=cfg())
        sim.flood(150, n_accounts=12)
        sim.run_rounds(30)
        # Execution order is the consensus-critical output
        assert sim.all_executed_match()
        assert len(sim.nodes[0].engine.execution.executed_txs) >= 120
        # Certificate total order should also match after full delivery
        sim.deliver_all()
        for n in sim.nodes:
            n.engine.advance_ordering()
        assert sim.all_order_match()

    def test_soft_final_before_hard(self):
        sim = NetworkSim(n=5, config=cfg(hard_finality_h=20))
        sim.flood(20, n_accounts=5)
        sim.run_rounds(10)
        m = sim.nodes[0].engine.metrics()
        assert m["execution"]["soft_final_txs"] >= 10
        assert m["hard_final_batches"] < m["ordering"]["ordered"]

    def test_hard_final_grows(self):
        sim = NetworkSim(n=5, config=cfg(hard_finality_h=3, max_batch_txs=5))
        for wave in range(12):
            sim.flood(5, n_accounts=5, start_seq=1 + wave)
            sim.run_rounds(3)
        m = sim.nodes[0].engine.metrics()
        assert m["hard_final_batches"] >= 3


class TestConflicts:
    def test_double_spend_one_winner(self):
        sim = NetworkSim(n=5, config=cfg())
        a = Transaction.create("acct1", 1, "acct2", 50, fee=40, first_seen=0.0)
        b = Transaction.create("acct1", 1, "acct3", 50, fee=5, first_seen=1.0)
        sim.submit_tx(a, to=0)
        sim.submit_tx(b, to=1)
        sim.deliver_all()
        sim.run_rounds(15)
        accepted = set(sim.nodes[0].engine.execution.executed_txs)
        assert len({a.tx_id, b.tx_id} & accepted) == 1
        assert sim.all_executed_match()

    def test_insufficient_balance(self):
        balances = {f"acct{i}": 30 for i in range(1, 6)}
        sim = NetworkSim(n=5, config=cfg(), genesis_balances=balances)
        bad = Transaction.create("acct1", 1, "acct2", 100, fee=10)
        good = Transaction.create("acct1", 1, "acct2", 5, fee=5, first_seen=1.0)
        sim.submit_tx(bad)
        sim.submit_tx(good)
        sim.deliver_all()
        sim.run_rounds(12)
        acc = set(sim.nodes[0].engine.execution.executed_txs)
        assert bad.tx_id not in acc
        assert good.tx_id in acc


class TestFaults:
    def test_one_silent_still_certifies(self):
        """4/5 = 80% exactly — should certify and order."""
        sim = NetworkSim(n=5, config=cfg())
        sim.nodes[4].active = False
        sim.flood(30, n_accounts=8)
        for _ in range(25):
            for n in sim.nodes[:4]:
                n.engine.create_batch(force=True)
                n.engine.advance_ordering()
            sim.deliver_all()
        # Active nodes agree
        bases = [tuple(sim.nodes[i].engine.execution.executed_txs) for i in range(4)]
        assert all(b == bases[0] for b in bases)
        assert len(bases[0]) >= 15

    def test_two_silent_no_certify(self):
        """3/5 = 60% < 80% — batches should not certify."""
        sim = NetworkSim(n=5, config=cfg())
        sim.nodes[3].active = False
        sim.nodes[4].active = False
        sim.flood(10, n_accounts=5)
        for _ in range(15):
            for n in sim.nodes[:3]:
                n.engine.create_batch(force=True)
            sim.deliver_all()
        # certified should stay genesis-only (1)
        for i in range(3):
            assert sim.nodes[i].engine.dag.metrics()["certified"] == 1


class TestDA:
    def test_parallel_batches_certify(self):
        sim = NetworkSim(n=5, config=cfg(max_batch_txs=10))
        sim.flood(50, n_accounts=10)
        made = sim.step(force=True)
        assert len(made) >= 2
        sim.run_rounds(15)
        assert sim.nodes[0].engine.dag.metrics()["certified"] > 2
        assert sim.all_executed_match()

    def test_parents_are_certified_tips(self):
        sim = NetworkSim(n=5, config=cfg())
        sim.flood(20, n_accounts=5)
        sim.run_rounds(8)
        for n in sim.nodes:
            for bh, batch in n.engine.dag.batches.items():
                if batch.creator == "genesis":
                    continue
                for p in batch.parents:
                    # parents should become certified eventually
                    assert p in n.engine.dag.batches or p in n.engine.dag.certificates


@pytest.mark.parametrize("n", [3, 5, 7])
def test_network_sizes(n: int):
    sim = NetworkSim(n=n, config=cfg())
    sim.flood(25, n_accounts=min(10, n * 2))
    sim.run_rounds(20)
    assert sim.all_executed_match()
    assert len(sim.nodes[0].engine.execution.executed_txs) >= 10
