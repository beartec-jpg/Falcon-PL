#!/usr/bin/env python3
"""Offline tests for the live-walletd behaviour upstreamed into main (Refs #55).

  python3 crates/fd-pl/scripts/test_pl_walletd_live_upstream.py

Covers: V2 bridge env precedence/default, RPC fallback list, eth-kickoff needs
DESTLOCK_EXIT=1 up front and only pays the burn's external_to, eth-open-claim
validates the account. Every external call is mocked (no key, network or tx).
"""
from __future__ import annotations

import importlib.util
import io
import json
import os
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent


def _load(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, _HERE / file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


wd = _load("walletd", "pl-walletd.py")

NOTE = "ab" * 32
GOOD = "0x" + "11" * 20
BAD = "0x" + "22" * 20


def _post(body: dict) -> dict:
    raw = json.dumps(body).encode()
    h = wd.H.__new__(wd.H)
    h.headers = {"Content-Length": str(len(raw))}
    h.rfile = io.BytesIO(raw)
    sent: dict = {}
    h._json = lambda code, obj: sent.update(code=code, obj=obj)
    h.do_POST()
    return sent


def _status():
    return "ok " + json.dumps({"body": {"rails": [
        {"asset": "ETH", "withdrawals": [{"note_id": NOTE, "amount": 1, "external_to": GOOD[2:]}]},
        "junk",
    ]}})


class Config(unittest.TestCase):
    def _reload(self, env):
        with patch.dict(os.environ, env, clear=True):
            return _load("walletd_cfg", "pl-walletd.py")

    def test_bridge_precedence_and_default(self):
        m = self._reload({})
        self.assertEqual(m.QC_V2_BRIDGE, "0x811854827627024B38926Ea9DCc0f88ACd5fB23e")
        self.assertTrue(m.QC_V2_LIVE)
        m = self._reload({"FALCON_QC_V2_BRIDGE": "0xaa", "FPL_SEPOLIA_QC_V2_BRIDGE": "0xbb"})
        self.assertEqual(m.QC_V2_BRIDGE, "0xbb")
        self.assertEqual(m.V2_BRIDGE, "0xbb")
        m = self._reload({"FALCON_QC_V2_BRIDGE": "0xaa"})
        self.assertEqual(m.QC_V2_BRIDGE, "0xaa")

    def test_rpc_list_dedupes_and_keeps_env_first(self):
        m = self._reload({"ETH_RPC": "https://ethereum-sepolia-rpc.publicnode.com"})
        self.assertEqual(m.ETH_RPCS, ["https://ethereum-sepolia-rpc.publicnode.com",
                                      "https://sepolia.gateway.tenderly.co"])
        m = self._reload({})
        self.assertEqual(m.ETH_RPCS[0], "https://sepolia.gateway.tenderly.co")
        self.assertEqual(len(m.ETH_RPCS), 2)

    def test_cast_uint_falls_back_on_failure(self):
        calls = []

        def run(argv, **kw):
            calls.append(argv[argv.index("--rpc-url") + 1])
            if len(calls) == 1:
                return subprocess.CompletedProcess(argv, 1, "", "rpc down")
            return subprocess.CompletedProcess(argv, 0, "42\n", "")

        with patch.object(wd, "ETH_RPCS", ["https://a.invalid", "https://b.invalid"]), \
                patch.object(wd.subprocess, "run", side_effect=run):
            self.assertEqual(wd._cast_uint("fplTip()(uint64)"), 42)
        self.assertEqual(calls, ["https://a.invalid", "https://b.invalid"])


class EthKickoff(unittest.TestCase):
    def setUp(self):
        self.sends = []

        def run(argv, **kw):
            self.sends.append(argv)
            return subprocess.CompletedProcess(argv, 0, "0x" + "cd" * 32, "")

        self.patches = [
            patch.object(wd.subprocess, "check_output", return_value=_status()),
            patch.object(wd.subprocess, "run", side_effect=run),
            patch.object(wd, "_eth_pk", return_value="0xnot-a-key"),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()

    def test_handler_refuses_without_destlock_exit(self):
        for v in (None, "", "0", "true"):
            env = {k: x for k, x in os.environ.items() if k != "DESTLOCK_EXIT"}
            if v is not None:
                env["DESTLOCK_EXIT"] = v
            with patch.dict(os.environ, env, clear=True):
                sent = _post({"action": "eth-kickoff", "noteId": NOTE, "dest": GOOD, "amount": "1"})
            self.assertEqual(sent["code"], 403, v)
        self.assertEqual(self.sends, [])

    def test_function_refuses_without_destlock_exit(self):
        with patch.dict(os.environ, {"DESTLOCK_EXIT": "0"}):
            with self.assertRaisesRegex(RuntimeError, "DESTLOCK_EXIT=1 required"):
                wd.eth_kickoff(NOTE, GOOD, 1, False)
        self.assertEqual(self.sends, [])

    def test_dest_must_be_external_to(self):
        with patch.dict(os.environ, {"DESTLOCK_EXIT": "1"}):
            with self.assertRaisesRegex(RuntimeError, "external_to"):
                wd.eth_kickoff(NOTE, BAD, 1, False)
            self.assertEqual(self.sends, [])
            rec = wd.eth_kickoff(NOTE, GOOD.upper().replace("0X", "0x"), 1, False)
        self.assertTrue(rec["ok"])
        self.assertEqual(len(self.sends), 1)

    def test_unknown_note_refused(self):
        with patch.dict(os.environ, {"DESTLOCK_EXIT": "1"}):
            with self.assertRaisesRegex(RuntimeError, "not in packed rail withdrawals"):
                wd.eth_kickoff("cd" * 32, GOOD, 1, False)
        self.assertEqual(self.sends, [])


class EthOpenClaimAccount(unittest.TestCase):
    def test_account_validated_before_send(self):
        with patch.object(wd, "eth_open_claim", side_effect=AssertionError("must not run")):
            for acct in ("", "x", "../keys", "a/b", "from-only"):
                body = {"action": "eth-open-claim", "noteId": NOTE, "dest": GOOD, "amount": "1", "asset": "ETH"}
                if acct == "from-only":
                    body["from"] = "alice"  # live drops the `from` alias
                elif acct:
                    body["account"] = acct
                sent = _post(body)
                self.assertEqual(sent["code"], 400, acct)
                self.assertRegex(sent["obj"]["error"], "^(account must be a PL (account )?name|account required)$")


if __name__ == "__main__":
    unittest.main()
