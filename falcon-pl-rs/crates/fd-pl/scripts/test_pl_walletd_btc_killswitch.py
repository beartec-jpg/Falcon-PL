#!/usr/bin/env python3
"""Offline tests for the pl-walletd.py BTC spend kill switch (FPL_BTC_KICKOFF_ENABLED).

  python3 crates/fd-pl/scripts/test_pl_walletd_btc_killswitch.py

btc-kickoff and btc-take must answer 403 {"ok": false, "error": "btc kickoff disabled"}
unless FPL_BTC_KICKOFF_ENABLED is exactly "1", and must do so before any key file is
opened, any UTXO is looked up or anything is signed. Every external call is mocked:
no key, network, falcon-pl-ctl or Bitcoin transaction is used.
"""
from __future__ import annotations

import importlib.util
import io
import json
import os
import unittest
import urllib.request
from pathlib import Path
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent


def _load(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, _HERE / file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


wd = _load("walletd", "pl-walletd.py")

ENV = "FPL_BTC_KICKOFF_ENABLED"
DEST = "tb1p" + "q" * 58
KICKOFF = {"action": "btc-kickoff", "account": "alice-test", "dest": DEST, "amount": "5000"}
TAKE = {
    "action": "btc-take",
    "account": "alice-test",
    "dest": DEST,
    "amount": "5000",
    "destSecret": "11" * 32,
    "prevTxid": "22" * 32,
    "vout": "0",
    "sats": "5000",
}
OFF_VALUES = [None, "", "0", "true", "TRUE", "yes", "on", " 1", "1 ", "01", "2"]
DISABLED = {"ok": False, "error": "btc kickoff disabled"}


class _Tripwire(AssertionError):
    pass


def _post(body: dict) -> dict:
    raw = json.dumps(body).encode()
    h = wd.H.__new__(wd.H)
    h.headers = {"Content-Length": str(len(raw))}
    h.rfile = io.BytesIO(raw)
    sent: dict = {}
    h._json = lambda code, obj: sent.update(code=code, obj=obj)
    h.do_POST()
    return sent


def _env(value):
    env = {k: v for k, v in os.environ.items() if k != ENV}
    if value is not None:
        env[ENV] = value
    return patch.dict(os.environ, env, clear=True)


class Refused(unittest.TestCase):
    """Switch off: 403, and nothing below the handler runs (no open, no network, no ctl)."""

    def setUp(self):
        self.opened: list[str] = []

        def spy_open(file, *a, **k):
            # Record and fail fast: never touch the real filesystem (abort seed, claimer
            # key, FROST lock) even if the gate regresses.
            self.opened.append(str(file))
            raise _Tripwire(f"open({file!r}) with the kill switch off")

        def trip(*a, **k):
            raise _Tripwire("must not be reached with the kill switch off")

        self.patches = [
            patch("builtins.open", side_effect=spy_open),
            patch.object(wd, "ctl", side_effect=trip),
            patch.object(wd, "instance_utxo", side_effect=trip),
            patch.object(wd, "rail_btc", side_effect=trip),
            patch.object(wd, "_load_abort_seed", side_effect=trip),
            patch.object(wd, "abort_commit_hex", side_effect=trip),
            patch.object(wd.subprocess, "run", side_effect=trip),
            patch.object(wd.tempfile, "mkstemp", side_effect=trip),
            patch.object(urllib.request, "urlopen", side_effect=trip),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()

    def _assert_refused(self, body, value):
        for mode in ("bitvm2", "watch"):
            self.opened.clear()
            with _env(value), patch.object(wd, "BTC_EXIT_MODE", mode):
                sent = _post(body)
            self.assertEqual(sent.get("code"), 403, (body["action"], value, mode, sent))
            self.assertEqual(sent.get("obj"), DISABLED)
            self.assertEqual(self.opened, [], f"file opened with switch={value!r}: {self.opened}")

    def test_kickoff_refused_when_unset(self):
        self._assert_refused(KICKOFF, None)

    def test_kickoff_refused_when_zero(self):
        self._assert_refused(KICKOFF, "0")

    def test_kickoff_refused_when_true(self):
        self._assert_refused(KICKOFF, "true")

    def test_take_refused_when_unset_zero_true(self):
        for v in (None, "0", "true"):
            self._assert_refused(TAKE, v)

    def test_every_non_one_value_is_off(self):
        for v in OFF_VALUES:
            for body in (KICKOFF, TAKE):
                self._assert_refused(body, v)

    def test_refused_before_validation(self):
        # Even a malformed request gets 403, not a 400 that would leak validation.
        for body in ({"action": "btc-kickoff"}, {"action": "btc-take", "amount": "x"},
                     {"action": " BTC-Kickoff "}, {"action": "btc-take\n"}):
            self._assert_refused(body, None)

    def test_inner_functions_refuse_too(self):
        # Defence in depth: a direct call (not via the handler) also refuses first.
        with _env(None):
            with self.assertRaisesRegex(wd.BtcKickoffDisabled, "btc kickoff disabled"):
                wd.btc_kickoff("alice-test", 5000, DEST)
            with self.assertRaisesRegex(wd.BtcKickoffDisabled, "btc kickoff disabled"):
                wd.btc_bitvm2_kickoff("alice-test", 5000, DEST)
            with self.assertRaisesRegex(wd.BtcKickoffDisabled, "btc kickoff disabled"):
                wd.btc_dest_lock_take("alice-test", DEST, 5000, "11" * 32, "22" * 32, 0, 5000)
        self.assertEqual(self.opened, [])

    def test_switch_read_per_request(self):
        # Turning it off at runtime takes effect on the next request (no restart cached value).
        with _env("1"):
            self.assertTrue(wd.btc_kickoff_enabled())
        with _env("0"):
            self.assertFalse(wd.btc_kickoff_enabled())
        self._assert_refused(KICKOFF, "0")


class OtherActionsUnaffected(unittest.TestCase):
    def test_eth_and_unknown_actions_not_gated(self):
        with _env(None):
            with patch.object(wd, "eth_open_claim", return_value={"ok": True}):
                sent = _post({"action": "eth-open-claim", "noteId": "0x" + "00" * 32,
                              "dest": "0x" + "00" * 20, "amount": "1", "asset": "ETH", "account": "acct"})
            self.assertEqual(sent["code"], 200)
            sent = _post({"action": "btc-kickoff-x"})
            self.assertEqual(sent, {"code": 400, "obj": {"error": "unknown action"}})


class NonBtcActionsRejectBtcAssets(unittest.TestCase):
    """eth-open-claim, claim-proof, eth-kickoff and mint-* take an `asset` that the site passes
    through. A BTC-like asset must be rejected with 400 before any signing path (BTC or ETH),
    whatever the kill switch says. eth_open_claim only takes a usdc bool, so it can never
    build a BTC claim; the handler's ETH/USDC allowlist is what stops BTC here."""

    ASSETS = ["BTC", "btc", " BTC ", "FBTC", "fbtc", "tBTC", "WBTC", "BTC-TESTNET", "SATS", "bitcoin"]
    BASES = {
        "eth-open-claim": {"noteId": "0x" + "00" * 32, "dest": "0x" + "00" * 20, "amount": "1", "account": "acct"},
        "claim-proof": {"dest": "0x" + "00" * 20, "amount": "1", "account": "acct"},
        "eth-kickoff": {"noteId": "0x" + "00" * 32, "dest": "0x" + "00" * 20, "amount": "1"},
        "mint-eth-deposit": {"account": "acct", "txHash": "0x" + "55" * 32},
        "mint-status": {"account": "acct", "txHash": "0x" + "55" * 32},
    }

    def setUp(self):
        def trip(*a, **k):
            raise _Tripwire("signing/IO path reached with a BTC asset")

        self.patches = [
            patch("builtins.open", side_effect=trip),
            patch.object(wd, "ctl", side_effect=trip),
            patch.object(wd, "btc_kickoff", side_effect=trip),
            patch.object(wd, "btc_bitvm2_kickoff", side_effect=trip),
            patch.object(wd, "btc_dest_lock_take", side_effect=trip),
            patch.object(wd, "instance_utxo", side_effect=trip),
            patch.object(wd, "eth_open_claim", side_effect=trip),
            patch.object(wd, "eth_kickoff", side_effect=trip),
            patch.object(wd, "claim_proof", side_effect=trip),
            patch.object(wd, "mint_queue", side_effect=trip),
            patch.object(wd, "mint_status", side_effect=trip),
            patch.object(wd.subprocess, "run", side_effect=trip),
            patch.object(wd.subprocess, "Popen", side_effect=trip),
            patch.object(urllib.request, "urlopen", side_effect=trip),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()

    def test_btc_like_assets_rejected_on_every_asset_action(self):
        for switch in (None, "1"):
            for action, base in self.BASES.items():
                for asset in self.ASSETS:
                    # DESTLOCK_EXIT=1 so eth-kickoff gets past its up-front 403 and the
                    # asset check itself is exercised.
                    with _env(switch), patch.dict(os.environ, {"DESTLOCK_EXIT": "1"}):
                        sent = _post({**base, "action": action, "asset": asset})
                    self.assertEqual(sent.get("code"), 400, (switch, action, asset, sent))
                    self.assertRegex(sent["obj"]["error"], "asset must be ETH or USDC")

    def test_chain_param_is_ignored_not_routed(self):
        # No action reads a chain/network field; adding one cannot reroute ETH to BTC.
        with _env("1"):
            sent = _post({**self.BASES["eth-open-claim"], "action": "eth-open-claim",
                          "asset": "BTC", "chain": "bitcoin", "network": "testnet"})
        self.assertEqual(sent["code"], 400)


class Allowed(unittest.TestCase):
    """Switch exactly "1": the existing path runs (signing mocked, no key or network)."""

    def setUp(self):
        self.ctl_calls: list[tuple] = []

        def fake_ctl(*args, timeout=30):
            self.ctl_calls.append(args)
            return "stub-signed-tx-hex\n"

        self.patches = [
            patch.object(wd, "ctl", side_effect=fake_ctl),
            patch.object(wd, "instance_utxo", return_value=("33" * 32, 1, 100000)),
            patch.object(wd, "abort_commit_hex", return_value="44" * 32),
            patch.object(urllib.request, "urlopen", side_effect=AssertionError("no network")),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()

    def test_kickoff_allowed_with_one(self):
        with _env("1"), patch.object(wd, "BTC_EXIT_MODE", "bitvm2"):
            sent = _post(KICKOFF)
        self.assertEqual(sent["code"], 200, sent)
        self.assertTrue(sent["obj"]["ok"])
        self.assertEqual(sent["obj"]["signed_btc_tx"], "stub-signed-tx-hex")
        self.assertEqual(len(self.ctl_calls), 1)
        self.assertEqual(self.ctl_calls[0][0], "btc-bitvm2-user-kickoff")

    def test_take_allowed_with_one(self):
        with _env("1"):
            sent = _post(TAKE)
        self.assertEqual(sent["code"], 200, sent)
        self.assertEqual(sent["obj"]["mode"], "dest-lock-take")
        self.assertEqual(self.ctl_calls[0][0], "btc-dest-lock-take")

    def test_existing_validation_still_applies_when_on(self):
        with _env("1"):
            sent = _post({**KICKOFF, "amount": "10"})
        self.assertEqual(sent["code"], 400)
        self.assertEqual(self.ctl_calls, [])


if __name__ == "__main__":
    unittest.main()
