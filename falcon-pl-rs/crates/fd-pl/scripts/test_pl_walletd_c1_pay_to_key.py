#!/usr/bin/env python3
"""Offline tests for audit C-1: walletd faucet commits the recipient key (PayToKey).

  python3 crates/fd-pl/scripts/test_pl_walletd_c1_pay_to_key.py
"""
from __future__ import annotations

import hashlib
import importlib.util
import os
import unittest
from pathlib import Path
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("walletd_c1", _HERE / "pl-walletd.py")
wd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wd)

RAW = bytes(range(256)) * 3 + bytes(129)  # 897 bytes, not a real key
PK = RAW.hex()
KH = hashlib.sha256(RAW).hexdigest()


class C1(unittest.TestCase):
    def test_off_by_default(self):
        with patch.dict(os.environ, {"FPL_FAUCET_PAY_TO_KEY": ""}):
            self.assertEqual(wd.faucet_key_hash("alice.x", {"publicKey": PK}), "")

    def test_existing_account_gets_plain_pay(self):
        with patch.dict(os.environ, {"FPL_FAUCET_PAY_TO_KEY": "1"}):
            with patch.object(wd, "account", return_value={"exists": True}):
                self.assertEqual(wd.faucet_key_hash("alice.x", {"publicKey": PK}), "")
                with patch.object(wd, "_names_load", return_value=[{"name": "alice.x", "publicKey": PK}]):
                    self.assertEqual(wd.faucet_key_hash("alice.x", {}), "")
            with patch.object(wd, "account", return_value={"exists": False}):
                self.assertEqual(wd.faucet_key_hash("alice.x", {"publicKey": PK}), KH)
            with patch.object(wd, "account", side_effect=RuntimeError("node down")):
                with self.assertRaises(ValueError):
                    wd.faucet_key_hash("alice.x", {"publicKey": PK})

    @patch.object(wd, "account", return_value={"exists": False})
    def test_on_uses_request_key_or_reservation(self, _acct):
        with patch.dict(os.environ, {"FPL_FAUCET_PAY_TO_KEY": "1"}):
            self.assertEqual(wd.faucet_key_hash("alice.x", {"publicKey": PK}), KH)
            self.assertEqual(wd.faucet_key_hash("alice.x", {"publicKey": "fb" + PK}), KH)
            self.assertEqual(wd.faucet_key_hash("alice.x", {"keyHash": KH.upper()}), KH)
            with patch.object(wd, "_names_load", return_value=[{"name": "alice.x", "publicKey": PK}]):
                self.assertEqual(wd.faucet_key_hash("alice.x", {}), KH)
            with patch.object(wd, "_names_load", return_value=[]):
                with self.assertRaises(ValueError):
                    wd.faucet_key_hash("nobody.x", {})
            self.assertEqual(wd.faucet_key_hash("rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe", {}), "")
            with self.assertRaises(ValueError):
                wd.faucet_key_hash("alice.x", {"keyHash": "zz"})


if __name__ == "__main__":
    unittest.main()
