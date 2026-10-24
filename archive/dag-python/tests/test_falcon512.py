"""Falcon-512 certificate authentication tests."""

from __future__ import annotations

import pytest

from falcon_dag.config import ProtocolConfig
from falcon_dag.crypto import (
    Falcon512KeyRing,
    batch_sign_message,
    falcon_available,
)
from falcon_dag.sim import NetworkSim
from falcon_dag.types import Transaction

pytestmark = pytest.mark.skipif(
    not falcon_available(), reason="falcon-python not installed"
)


def test_falcon512_key_sizes_and_roundtrip():
    ring = Falcon512KeyRing.generate_unl(["val1", "val2"])
    msg = batch_sign_message("ab" * 32)
    sig = ring.sign("val1", msg)
    assert ring.verify("val1", msg, sig)
    assert not ring.verify("val2", msg, sig)
    assert not ring.verify("val1", msg + "x", sig)
    # Sizes match Falcon-512 / OQS expectations
    assert len(ring.public_keys["val1"]) == 897
    assert len(ring.secret_keys["val1"]) == 1281
    sig_len = len(bytes.fromhex(sig))
    assert 600 <= sig_len <= 752  # compressed variable
    m = ring.metrics()
    assert m["scheme"] == "falcon-512"
    assert m["sample_sig_bytes"] is not None


def test_falcon512_public_only_verify(tmp_path):
    ring = Falcon512KeyRing.generate_unl(["val1", "val2", "val3"])
    ring.save_dir(tmp_path)
    # Node with only val2 secret still verifies val1
    remote = Falcon512KeyRing.load_for_validator(tmp_path, "val2")
    msg = batch_sign_message("cd" * 32)
    sig = ring.sign("val1", msg)
    assert remote.verify("val1", msg, sig)
    # Cannot sign as val1 without secret
    with pytest.raises(KeyError):
        remote.sign("val1", msg)


def test_network_sim_with_falcon512_certs():
    cfg = ProtocolConfig(
        target_batch_interval_s=0.0,
        hard_finality_h=3,
        max_batch_txs=64,
        worker_sharding=True,
        shard_orphan_age_s=0.0,
        require_sig_verify=True,
        crypto_scheme="falcon-512",
    )
    sim = NetworkSim(n=5, config=cfg)
    assert sim.keyring.scheme == "falcon-512"
    sim.flood(40, n_accounts=10)
    sim.run_rounds(20)
    assert sim.all_executed_match()
    assert len(sim.nodes[0].engine.execution.executed_txs) >= 30
    m = sim.nodes[0].engine.dag.metrics()
    assert m["crypto_scheme"] == "falcon-512"
    assert m["rejected_sigs"] == 0


def test_forged_falcon_sig_rejected():
    cfg = ProtocolConfig(
        target_batch_interval_s=0.0,
        require_sig_verify=True,
        crypto_scheme="falcon-512",
        worker_sharding=False,
        shard_orphan_age_s=0.0,
    )
    sim = NetworkSim(n=5, config=cfg)
    tx = Transaction.create("acct1", 1, "acct2", 1, fee=10)
    sim.submit_tx(tx, to=0)
    sim.deliver_all()
    b = sim.nodes[0].engine.create_batch(force=True)
    assert b is not None
    sim.deliver_all()  # val2 must have the batch before sig is checked
    from falcon_dag.types import PartialSignature

    before = sim.nodes[1].engine.dag.rejected_sigs
    sim.nodes[1].engine.on_remote_sig(
        PartialSignature(
            batch_hash=b.batch_hash,
            validator_id="val3",  # claim to be val3 with garbage
            signature="ab" * 330,
        )
    )
    assert sim.nodes[1].engine.dag.rejected_sigs > before


def test_cert_byte_amortization_with_falcon512():
    """Order-path bytes/tx drops as batch size grows (PQ-sig story)."""
    ring = Falcon512KeyRing.generate_unl(["val1", "val2", "val3", "val4", "val5"])
    msg = batch_sign_message("ee" * 32)
    sigs = [ring.sign(f"val{i}", msg) for i in range(1, 5)]  # 4-of-5 threshold
    cert_bytes = 64 + sum(len(bytes.fromhex(s)) for s in sigs)  # hash + sigs
    rows = []
    for n_txs in (1, 10, 50, 128):
        rows.append(cert_bytes / n_txs)
    assert rows[-1] < rows[0]
    assert rows[-1] < 50  # well under one Falcon sig per tx
