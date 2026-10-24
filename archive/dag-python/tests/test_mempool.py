from falcon_dag.mempool import GlobalMempool
from falcon_dag.types import Transaction
from falcon_dag.config import ProtocolConfig


def test_priority_and_starvation():
    mp = GlobalMempool(ProtocolConfig(starvation_boost_per_s=1.0))
    old = Transaction.create("acct1", 1, "acct2", 1, fee=1, first_seen=0.0)
    new = Transaction.create("acct2", 1, "acct3", 1, fee=5, first_seen=100.0)
    mp.add(old)
    mp.add(new)
    # At t=100: old age=100 → density 101; new age=0 → density 5
    assert mp.ordered(now=100.0)[0].tx_id == old.tx_id
    # No boost: higher fee wins
    mid = GlobalMempool(ProtocolConfig(starvation_boost_per_s=0.0))
    mid.add(old)
    mid.add(new)
    assert mid.ordered(now=100.0)[0].tx_id == new.tx_id


def test_mark_done_drops_competitors():
    mp = GlobalMempool()
    a = Transaction.create("acct1", 1, "acct2", 1, fee=10)
    b = Transaction.create("acct1", 1, "acct3", 1, fee=20)
    mp.add(a)
    mp.add(b)
    mp.mark_done([a.tx_id])
    assert len(mp) == 0
