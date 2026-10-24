#!/usr/bin/env python3
"""Offline tests for walletd openClaim gas (F4, V3_TEST_PLAN): limit = estimate x1.25, price = 2x network
inside [floor, cap], a reverting estimate stops before send, fallback when no RPC can estimate.

  python3 crates/fd-pl/scripts/test_pl_walletd_openclaim_gas.py

Every external call is mocked (no key, network or tx).
"""
from __future__ import annotations

import importlib.util
import os
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("walletd", _HERE / "pl-walletd.py")
wd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wd)

ADDR = "0x" + "aa" * 20


def fake_run(estimate=None, gas_price="1000000", addr_ok=True, est_rc=0, est_err=""):
    calls = []

    def run(argv, **kw):
        calls.append(argv)
        if argv[1:3] == ["wallet", "address"]:
            return subprocess.CompletedProcess(argv, 0 if addr_ok else 1, ADDR + "\n" if addr_ok else "", "")
        if argv[1] == "estimate":
            if estimate is None:
                return subprocess.CompletedProcess(argv, est_rc or 1, "", est_err or "rpc down")
            return subprocess.CompletedProcess(argv, 0, f"{estimate}\n", "")
        if argv[1] == "gas-price":
            if gas_price is None:
                return subprocess.CompletedProcess(argv, 1, "", "down")
            return subprocess.CompletedProcess(argv, 0, f"{gas_price}\n", "")
        raise AssertionError(argv)

    return run, calls


class OpenClaimGas(unittest.TestCase):
    def go(self, run, env=None):
        with patch.dict(os.environ, env or {}, clear=False), \
                patch.object(wd, "_eth_pk", lambda: "0x" + "01" * 32), \
                patch.object(wd, "ETH_RPCS", ["https://a.invalid", "https://b.invalid"]), \
                patch.object(wd.subprocess, "run", side_effect=run):
            return wd._open_claim_gas("0x" + "bb" * 20, ["0x" + "cc" * 32])

    def test_estimate_times_1_25_and_price_floor(self):
        run, calls = fake_run(estimate=660_000, gas_price="1000000")  # 0.001 gwei network
        lim, price = self.go(run)
        self.assertEqual(lim, 825_000)
        self.assertEqual(price, 100_000_000)  # 0.1 gwei floor
        est = [c for c in calls if c[1] == "estimate"][0]
        self.assertIn("--from", est)
        self.assertEqual(est[est.index("--from") + 1], ADDR)
        self.assertNotIn("--private-key", est)

    def test_price_doubles_and_caps(self):
        run, _ = fake_run(estimate=660_000, gas_price="500000000")
        self.assertEqual(self.go(run)[1], 1_000_000_000)
        run, _ = fake_run(estimate=660_000, gas_price="5000000000")
        self.assertEqual(self.go(run)[1], 2_000_000_000)

    def test_fallback_when_no_estimate(self):
        run, _ = fake_run(estimate=None, gas_price=None)
        self.assertEqual(self.go(run), (850_000, 2_000_000_000))
        run, _ = fake_run(estimate=None, addr_ok=False)
        self.assertEqual(self.go(run, {"FPL_OPEN_CLAIM_GAS": "900000"})[0], 900_000)

    def test_bad_env_falls_back(self):
        run, _ = fake_run(estimate=None)
        self.assertEqual(self.go(run, {"FPL_OPEN_CLAIM_GAS": "400000"})[0], 850_000)
        self.assertEqual(self.go(run, {"FPL_OPEN_CLAIM_GAS": "x"})[0], 850_000)

    def test_revert_stops(self):
        run, _ = fake_run(estimate=None, est_rc=1, est_err="Error: execution reverted: exists")
        with self.assertRaises(RuntimeError):
            self.go(run)

    def test_estimate_above_cap_stops(self):
        run, _ = fake_run(estimate=3_000_000)
        with self.assertRaises(RuntimeError):
            self.go(run)

    def test_never_old_fixed_400k(self):
        run, _ = fake_run(estimate=None, gas_price=None)
        self.assertGreaterEqual(self.go(run)[0], 700_000)


if __name__ == "__main__":
    unittest.main(verbosity=1)
