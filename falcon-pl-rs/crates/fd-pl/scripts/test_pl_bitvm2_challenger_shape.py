"""Offline tests for #49 Kickoff shape checks in pl-bitvm2-challenger.py."""
import importlib.util
import pathlib
import unittest

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("chal", HERE / "pl-bitvm2-challenger.py")
chal = importlib.util.module_from_spec(spec)
spec.loader.exec_module(chal)

INST = "tb1pinstance"
LOCK = "0020" + "ab" * 32


def tx(outs, in_sats=90_000):
    return {
        "vin": [{"prevout": {"value": in_sats, "scriptpubkey_address": INST}}],
        "vout": [
            {"value": v, "scriptpubkey": spk, "scriptpubkey_address": addr}
            for v, spk, addr in outs
        ],
    }


class Shape(unittest.TestCase):
    def test_honest(self):
        t = tx([(20_000, LOCK, "tb1qlock"), (69_000, "5120ff", INST)])
        self.assertEqual(chal.kickoff_shape_alerts(t, INST, LOCK, 20_000), [])

    def test_extra_output(self):
        t = tx([(20_000, LOCK, "tb1qlock"), (39_000, "5120ff", INST), (30_000, "0014cc", "tb1qthief")])
        self.assertTrue(chal.kickoff_shape_alerts(t, INST, LOCK, 20_000))

    def test_wrong_change(self):
        t = tx([(20_000, LOCK, "tb1qlock"), (39_000, "5120ff", INST)])
        self.assertTrue(chal.kickoff_shape_alerts(t, INST, LOCK, 20_000))

    def test_plain_dest_address(self):
        t = tx([(20_000, "0014dd", "tb1quserdest"), (69_000, "5120ff", INST)])
        self.assertTrue(chal.kickoff_shape_alerts(t, INST, LOCK, 20_000))

    def test_lone_output_excess_fee(self):
        t = tx([(20_000, LOCK, "tb1qlock")])
        self.assertTrue(chal.kickoff_shape_alerts(t, INST, LOCK, 20_000))


if __name__ == "__main__":
    unittest.main()
