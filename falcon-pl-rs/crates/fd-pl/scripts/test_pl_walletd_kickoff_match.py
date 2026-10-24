#!/usr/bin/env python3
"""Offline tests: eth-kickoff must exactly match the burn note's amount and asset.

  python3 crates/fd-pl/scripts/test_pl_walletd_kickoff_match.py

A valid burn note on one rail must not be kicked off as a different amount or on the
other rail. Mismatches fail closed with 409 and nothing is sent. Every external call
is mocked (no key, network or tx).
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
_spec = importlib.util.spec_from_file_location("walletd_km", _HERE / "pl-walletd.py")
wd = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(wd)

ETH_NOTE = "ab" * 32
USDC_NOTE = "cd" * 32
DUP_NOTE = "ef" * 32
NOAMT_NOTE = "12" * 32
DEST = "0x" + "11" * 20


def _status(extra_eth=None):
    eth = [{"note_id": ETH_NOTE, "amount": 1000, "external_to": DEST[2:]},
           {"note_id": DUP_NOTE, "amount": 5, "external_to": DEST[2:]},
           {"note_id": NOAMT_NOTE, "external_to": DEST[2:]}]
    usdc = [{"note_id": USDC_NOTE, "amount": "250", "external_to": DEST},
            {"note_id": DUP_NOTE, "amount": 5, "external_to": DEST[2:]}]
    return "ok " + json.dumps({"body": {"rails": [
        {"asset": "ETH", "withdrawals": eth + (extra_eth or [])},
        {"asset": "USDC", "withdrawals": usdc},
    ]}})


def _post(body: dict) -> dict:
    raw = json.dumps(body).encode()
    h = wd.H.__new__(wd.H)
    h.headers = {"Content-Length": str(len(raw))}
    h.rfile = io.BytesIO(raw)
    sent: dict = {}
    h._json = lambda code, obj: sent.update(code=code, obj=obj)
    h.do_POST()
    return sent


class KickoffMatch(unittest.TestCase):
    def setUp(self):
        self.sends = []

        def run(argv, **kw):
            self.sends.append(argv)
            return subprocess.CompletedProcess(argv, 0, "0x" + "cd" * 32, "")

        self.patches = [
            patch.object(wd.subprocess, "check_output", return_value=_status()),
            patch.object(wd.subprocess, "run", side_effect=run),
            patch.object(wd, "_eth_pk", return_value="0xnot-a-key"),
            patch.dict(os.environ, {"DESTLOCK_EXIT": "1"}),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()

    def _kick(self, note, amount, asset):
        return _post({"action": "eth-kickoff", "noteId": note, "dest": DEST,
                      "amount": str(amount), "asset": asset})

    def test_exact_eth_match_sends(self):
        sent = self._kick(ETH_NOTE, 1000, "ETH")
        self.assertEqual(sent["code"], 200, sent)
        self.assertEqual(len(self.sends), 1)
        argv = self.sends[0]
        self.assertIn("1000", argv)
        self.assertIn("false", argv)

    def test_exact_usdc_match_sends(self):
        sent = self._kick(USDC_NOTE, 250, "USDC")
        self.assertEqual(sent["code"], 200, sent)
        self.assertIn("true", self.sends[0])

    def test_eth_note_as_usdc_refused(self):
        sent = self._kick(ETH_NOTE, 1000, "USDC")
        self.assertEqual(sent["code"], 409)
        self.assertIn("asset", sent["obj"]["error"])
        self.assertEqual(self.sends, [])

    def test_eth_note_as_larger_usdc_refused(self):
        sent = self._kick(ETH_NOTE, 10**12, "USDC")
        self.assertEqual(sent["code"], 409)
        self.assertEqual(self.sends, [])

    def test_usdc_note_as_eth_refused(self):
        sent = self._kick(USDC_NOTE, 250, "ETH")
        self.assertEqual(sent["code"], 409)
        self.assertEqual(self.sends, [])

    def test_amount_larger_or_smaller_refused(self):
        for amt in (1001, 999, 2000):
            sent = self._kick(ETH_NOTE, amt, "ETH")
            self.assertEqual(sent["code"], 409, amt)
            self.assertIn("amount", sent["obj"]["error"])
        self.assertEqual(self.sends, [])

    def test_note_on_two_rails_refused(self):
        sent = self._kick(DUP_NOTE, 5, "ETH")
        self.assertEqual(sent["code"], 409)
        self.assertEqual(self.sends, [])

    def test_note_without_amount_refused(self):
        sent = self._kick(NOAMT_NOTE, 1, "ETH")
        self.assertEqual(sent["code"], 409)
        self.assertEqual(self.sends, [])

    def test_function_raises_mismatch(self):
        with self.assertRaises(wd.KickoffMismatch):
            wd.eth_kickoff(ETH_NOTE, DEST, 1000, True)
        with self.assertRaises(wd.KickoffMismatch):
            wd.eth_kickoff(ETH_NOTE, DEST, 1001, False)
        self.assertEqual(self.sends, [])


if __name__ == "__main__":
    unittest.main()
