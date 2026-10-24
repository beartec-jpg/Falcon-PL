#!/usr/bin/env python3
"""Offline tests for audit M-1: walletd listen address, shared-key auth, faucet limits.

  python3 crates/fd-pl/scripts/test_pl_walletd_m1_auth_limits.py

No key file, network or tx is used: ctl is mocked and the limiter state lives in a temp dir.
"""
from __future__ import annotations

import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("walletd_m1", _HERE / "pl-walletd.py")
wd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wd)

KEY = "k" * 48
CLASSIC = "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe"


def call(method: str, body: dict | None = None, auth: str | None = None, path: str = "/") -> dict:
    h = wd.H.__new__(wd.H)
    raw = json.dumps(body or {}).encode()
    h.headers = {"Content-Length": str(len(raw))}
    if auth is not None:
        h.headers["Authorization"] = auth
    h.rfile = io.BytesIO(raw)
    h.path = path
    sent: dict = {}
    h._json = lambda code, obj: sent.update(code=code, obj=obj)
    getattr(h, "do_" + method)()
    return sent


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.calls = []

        def ctl(*args, timeout=30):
            self.calls.append(args)
            return "ok tx_id=" + "ab" * 32

        env = {k: v for k, v in os.environ.items() if not k.startswith(("WALLETD_", "FPL_FAUCET"))}
        self.patches = [
            patch.dict(os.environ, env, clear=True),
            patch.object(wd, "ctl", side_effect=ctl),
            patch.object(wd, "account", return_value={"account_type": "hot", "sequence": 1}),
            patch.object(wd, "WALLETD_KEY_FILE", str(Path(self.tmp.name) / "none.key")),
            patch.object(wd, "FAUCET_LIMITER", wd.FaucetLimiter(str(Path(self.tmp.name) / "faucet.json"))),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()
        self.tmp.cleanup()


class Bind(Base):
    def test_default_bind_is_loopback(self):
        src = (_HERE / "pl-walletd.py").read_text()
        self.assertIn('or "127.0.0.1").strip() or "127.0.0.1"', src)
        self.assertIn('os.environ.get("FPL_WALLETD_HOST")', src)
        self.assertNotIn('("0.0.0.0", 8788)', src)

    def test_non_loopback_without_key_refused(self):
        with self.assertRaises(SystemExit):
            wd.check_bind_safe("100.64.0.1", "")
        wd.check_bind_safe("100.64.0.1", KEY)
        wd.check_bind_safe("127.0.0.1", "")

    def test_all_interfaces_refused_even_with_key(self):
        for host in ("0.0.0.0", "::"):
            with self.assertRaises(SystemExit):
                wd.check_bind_safe(host, KEY)


class Auth(Base):
    def test_key_required_when_configured(self):
        os.environ["WALLETD_API_KEY"] = KEY
        body = {"action": "faucet", "to": CLASSIC}
        for bad in (None, "", "Bearer", "Bearer wrong", "Basic " + KEY, "bearer " + KEY, "Bearer " + KEY[:-1]):
            self.assertEqual(call("POST", body, bad)["code"], 401, bad)
            self.assertEqual(call("GET", None, bad, "/btc-deposit")["code"], 401, bad)
        self.assertEqual(self.calls, [])
        self.assertEqual(call("POST", body, "Bearer " + KEY)["code"], 200)
        self.assertEqual(call("GET", None, None, "/health")["code"], 200)

    def test_key_file(self):
        kf = Path(self.tmp.name) / "shared.key"
        kf.write_text(KEY + "\n")
        with patch.object(wd, "WALLETD_KEY_FILE", str(kf)):
            self.assertEqual(call("POST", {"action": "faucet", "to": "bob"})["code"], 401)
            self.assertEqual(call("POST", {"action": "faucet", "to": "bob"}, "Bearer " + KEY)["code"], 200)

    def test_no_key_loopback_mode_allows(self):
        self.assertEqual(call("POST", {"action": "faucet", "to": "bob"})["code"], 200)


class FaucetLimits(Base):
    def test_one_drip_per_recipient_per_day(self):
        self.assertEqual(call("POST", {"action": "faucet", "to": "bob"})["code"], 200)
        again = call("POST", {"action": "faucet", "to": "bob"})
        self.assertEqual(again["code"], 429, again)
        self.assertEqual(call("POST", {"action": "faucet", "to": CLASSIC})["code"], 200)
        self.assertEqual(call("POST", {"action": "faucet", "to": CLASSIC})["code"], 429)
        self.assertEqual(call("POST", {"action": "faucet", "account": "bob"})["code"], 429)
        self.assertEqual(call("POST", {"action": "faucet", "to": "carol"})["code"], 200)
        self.assertEqual(len(self.calls), 3)

    def test_window_and_max_from_env(self):
        os.environ["FPL_FAUCET_RECIPIENT_MAX"] = "2"
        os.environ["FPL_FAUCET_RECIPIENT_WINDOW_S"] = "60"
        lim = wd.FAUCET_LIMITER
        lim.record("bob", now=1000)
        self.assertIsNone(lim.check("bob", now=1001))
        lim.record("bob", now=1001)
        self.assertIsNotNone(lim.check("bob", now=1002))
        self.assertIsNone(lim.check("bob", now=1062))

    def test_global_cap(self):
        os.environ["FPL_FAUCET_GLOBAL_MAX"] = "3"
        for who in ("a1", "a2", "a3"):
            self.assertEqual(call("POST", {"action": "faucet", "to": who})["code"], 200)
        self.assertEqual(call("POST", {"action": "faucet", "to": "a4"})["code"], 429)

    def test_failed_drip_not_counted(self):
        with patch.object(wd, "ctl", side_effect=RuntimeError("node down")):
            self.assertEqual(call("POST", {"action": "faucet", "to": "bob"})["code"], 503)
        self.assertEqual(call("POST", {"action": "faucet", "to": "bob"})["code"], 200)

    def test_state_survives_restart(self):
        self.assertEqual(call("POST", {"action": "faucet", "to": "bob"})["code"], 200)
        fresh = wd.FaucetLimiter(wd.FAUCET_LIMITER.path)
        with patch.object(wd, "FAUCET_LIMITER", fresh):
            self.assertEqual(call("POST", {"action": "faucet", "to": "bob"})["code"], 429)

    def test_corrupt_state_file_is_ignored(self):
        Path(wd.FAUCET_LIMITER.path).write_text("{not json")
        fresh = wd.FaucetLimiter(wd.FAUCET_LIMITER.path)
        self.assertIsNone(fresh.check("bob"))


class NoCors(Base):
    def test_no_wildcard_cors(self):
        src = (_HERE / "pl-walletd.py").read_text()
        self.assertNotIn('Access-Control-Allow-Origin", "*"', src)


if __name__ == "__main__":
    unittest.main()
