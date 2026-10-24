#!/usr/bin/env python3
"""Offline tests for walletd request hardening (#55): strict account names, pay /
vault-activate allowlists, faucet cap, and 400 on bad JSON bodies.

  python3 crates/fd-pl/scripts/test_pl_walletd_hardening.py

Refusals must happen before any falcon-pl-ctl call, key/file open or subprocess:
every one of those is a tripwire in the refusal tests. No key, network or tx is used.
"""
from __future__ import annotations

import importlib.util
import io
import json
import os
import random
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

ENVS = ("FPL_WALLETD_PAY_FROM_ALLOW", "FPL_WALLETD_VAULT_ALLOW", "FPL_FAUCET_MAX", "FPL_BTC_KICKOFF_ENABLED")
CLASSIC = "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe"

BAD_NAMES = [
    "../faucet", "..", "a/../b", "keys/faucet", "/etc/passwd", "faucet/", "a..b", ".hidden",
    "-dash", "_under", "Alice", "ALICE", "a b", "a\x00b", "a\nb", "a\\b", "ａｌｉｃｅ", "alicé",
    "аlice", "a" * 65, "%2e%2e", "a;b", "a$b", "~root", "",
]


def _env(**kv):
    env = {k: v for k, v in os.environ.items() if k not in ENVS}
    env.update({k: v for k, v in kv.items() if v is not None})
    return patch.dict(os.environ, env, clear=True)


def _post_raw(raw: bytes, length=None) -> dict:
    h = wd.H.__new__(wd.H)
    h.headers = {"Content-Length": str(len(raw) if length is None else length)}
    h.rfile = io.BytesIO(raw)
    sent: dict = {}
    h._json = lambda code, obj: sent.update(code=code, obj=obj)
    h.do_POST()
    return sent


def _post(body) -> dict:
    return _post_raw(json.dumps(body).encode())


def _get(query: str) -> dict:
    h = wd.H.__new__(wd.H)
    h.path = "/?" + query
    sent: dict = {}
    h._json = lambda code, obj: sent.update(code=code, obj=obj)
    h.do_GET()
    return sent


class _Trip(AssertionError):
    pass


class Tripwired(unittest.TestCase):
    """Base: every side effect raises and is recorded."""

    def setUp(self):
        self.hits: list[str] = []

        def trip(name):
            def f(*a, **k):
                self.hits.append(name)
                raise _Trip(name)
            return f

        self.patches = [
            patch("builtins.open", side_effect=trip("open")),
            patch.object(wd, "ctl", side_effect=trip("ctl")),
            patch.object(wd, "account", side_effect=trip("account")),
            patch.object(wd, "_names_load", side_effect=trip("names_load")),
            patch.object(wd, "_names_save", side_effect=trip("names_save")),
            patch.object(wd.subprocess, "run", side_effect=trip("run")),
            patch.object(wd.subprocess, "Popen", side_effect=trip("Popen")),
            patch.object(wd.subprocess, "check_output", side_effect=trip("check_output")),
            patch.object(urllib.request, "urlopen", side_effect=trip("urlopen")),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()

    def refused(self, body, code, **env):
        with _env(**env):
            sent = _post(body)
        self.assertEqual(sent.get("code"), code, (body, env, sent))
        self.assertFalse(sent["obj"].get("ok", False))
        self.assertEqual(self.hits, [], (body, self.hits))
        return sent


class Names(Tripwired):
    def test_bad_names_rejected_on_every_account_field(self):
        allow = {"FPL_WALLETD_PAY_FROM_ALLOW": "alice", "FPL_WALLETD_VAULT_ALLOW": "alice"}
        for bad in BAD_NAMES:
            self.refused({"action": "pay", "from": bad, "to": "bob", "amount": 1}, 400, **allow)
            self.refused({"from": bad, "to": "bob", "amount": 1}, 400, **allow)  # default action
            self.refused({"action": "pay", "from": "alice", "to": bad, "amount": 1}, 400, **allow)
            self.refused({"action": "vault-activate", "account": bad, "destination": "bob"}, 400, **allow)
            self.refused({"action": "vault-activate", "from": bad, "to": "bob"}, 400, **allow)
            self.refused({"action": "vault-activate", "account": "alice", "destination": bad}, 400, **allow)
            self.refused({"action": "faucet", "to": bad}, 400)
            self.refused({"action": "faucet", "account": bad}, 400)
            if not bad or bad in ("Alice", "ALICE"):
                continue  # these handlers lower-case PL names first
            for action in ("eth-open-claim", "claim-proof", "mint-eth-deposit", "mint-status",
                           "btc-kickoff", "btc-take", "name-status", "name-reserve", "name-activate"):
                key = "name" if action.startswith("name-") else "account"
                self.refused({"action": action, key: bad, "asset": "ETH"}, 400 if not action.startswith("btc-")
                             else 403)  # btc-* are already 403 by the kill switch
            self.refused({"action": "mint-eth-deposit", "to": bad}, 400)

    def test_non_ascii_rejected_before_lowercasing(self):
        # U+212A KELVIN SIGN lower-cases to ASCII "k"; U+0130 lower-cases to "i" + combining dot.
        for bad in ("\u212aey", "\u0130x", "ali\u212ae"):
            self.assertEqual(bad.lower().isascii(), bad.startswith("\u212a") or bad.endswith("\u212ae"))
            for action in ("claim-proof", "mint-status", "eth-open-claim", "name-status"):
                key = "name" if action.startswith("name-") else "account"
                self.refused({"action": action, key: bad, "asset": "ETH"}, 400)

    def test_btc_actions_validate_names_when_switch_on(self):
        for bad in ("../x", "a/b", "ａ"):
            for action in ("btc-kickoff", "btc-take"):
                self.refused({"action": action, "account": bad}, 400, FPL_BTC_KICKOFF_ENABLED="1")
                self.refused({"action": action, "from": bad}, 400, FPL_BTC_KICKOFF_ENABLED="1")

    def test_non_string_fields_rejected(self):
        allow = {"FPL_WALLETD_PAY_FROM_ALLOW": "alice"}
        for v in (["alice"], {"a": 1}, 5, True):
            self.refused({"action": "pay", "from": v, "to": "bob", "amount": 1}, 400, **allow)
            self.refused({"action": "claim-proof", "account": v}, 400)

    def test_get_account_validated(self):
        for bad in ("../faucet", "a/b", "a" * 65, "ａ"):
            sent = _get("account=" + bad)
            self.assertEqual(sent["code"], 400, bad)
        self.assertEqual(self.hits, [])

    def test_fuzz_random_names(self):
        rng = random.Random(55)
        alphabet = "abcXYZ019._-/\\.. \t\n\x00é漢ａ%~$;"
        allow = {"FPL_WALLETD_PAY_FROM_ALLOW": "alice,bob", "FPL_WALLETD_VAULT_ALLOW": "alice"}
        n = 0
        for _ in range(3000):
            name = "".join(rng.choice(alphabet) for _ in range(rng.randint(0, 80)))
            if wd.strict_name(name.strip()):  # handlers strip; a stripped valid name is a name
                self.assertNotIn("/", name)
                self.assertNotIn("..", name)
                self.assertLessEqual(len(name.strip()), 64)
                continue
            n += 1
            self.refused({"action": "pay", "from": name, "to": "bob", "amount": 1}, 400, **allow)
            self.refused({"action": "vault-activate", "account": name, "destination": "bob"}, 400, **allow)
        self.assertGreater(n, 2500)


class Allowlists(Tripwired):
    PAY = {"action": "pay", "from": "alice", "to": "bob", "amount": 5}
    VAULT = {"action": "vault-activate", "account": "alice", "destination": "bob"}

    def test_pay_refused_unless_allowlisted(self):
        for allow in (None, "", " , ", "bob", "alice2", "Alice", "carol, dave", "alic"):
            sent = self.refused(self.PAY, 403, FPL_WALLETD_PAY_FROM_ALLOW=allow)
            self.assertEqual(sent["obj"]["error"], "pay from this account is not enabled")
        # vault allowlist does not open pay
        self.refused(self.PAY, 403, FPL_WALLETD_VAULT_ALLOW="alice")

    def test_vault_refused_unless_allowlisted(self):
        for allow in (None, "", "bob", "alice2"):
            self.refused(self.VAULT, 403, FPL_WALLETD_VAULT_ALLOW=allow)
        self.refused(self.VAULT, 403, FPL_WALLETD_PAY_FROM_ALLOW="alice")

    def test_pay_amount_must_be_positive_int(self):
        for amt in (0, -1, "0", "-5", "1.5", 1.5, "1e3", "", None, True, "abc", " ", "١٢"):
            self.refused({**self.PAY, "amount": amt}, 400, FPL_WALLETD_PAY_FROM_ALLOW="alice")


class AllowedPaths(unittest.TestCase):
    def setUp(self):
        self.calls = []

        def ctl(*args, timeout=30):
            self.calls.append(args)
            return "ok tx_id=" + "ab" * 32

        self.patches = [
            patch.object(wd, "ctl", side_effect=ctl),
            patch.object(wd, "account", return_value={"account_type": "hot", "sequence": 1}),
            patch.object(wd.time, "sleep", return_value=None),
            # These tests are about the allowlists and cap, not the M-1 drip limit.
            patch.object(wd, "FAUCET_LIMITER", wd.FaucetLimiter(None)),
            patch.dict(os.environ, {"FPL_FAUCET_RECIPIENT_MAX": "1000"}),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()

    def test_pay_allowed_on_match(self):
        with _env(FPL_WALLETD_PAY_FROM_ALLOW=" carol , alice "):
            sent = _post({"action": "pay", "from": "alice", "to": CLASSIC, "amount": "7"})
        self.assertEqual(sent["code"], 200, sent)
        self.assertEqual(self.calls[0][:7], ("pay", "--from", "alice", "--to", CLASSIC, "--amount", "7"))

    def test_vault_allowed_on_match(self):
        with _env(FPL_WALLETD_VAULT_ALLOW="alice"):
            sent = _post({"action": "vault-activate", "account": "alice", "destination": "bob"})
        self.assertEqual(sent["code"], 200, sent)
        self.assertEqual([c[0] for c in self.calls], ["vault-open", "vault-lock"])

    def test_allowlist_read_per_request(self):
        body = {"action": "pay", "from": "alice", "to": "bob", "amount": 1}
        with _env(FPL_WALLETD_PAY_FROM_ALLOW="alice"):
            self.assertEqual(_post(body)["code"], 200)
        with _env(FPL_WALLETD_PAY_FROM_ALLOW=""):
            self.assertEqual(_post(body)["code"], 403)

    def test_faucet_default_and_cap(self):
        with _env():
            self.assertEqual(wd.faucet_max(), 2000)
            sent = _post({"action": "faucet", "account": CLASSIC})
            self.assertEqual(sent["code"], 200, sent)
            self.assertEqual(self.calls[-1][self.calls[-1].index("--amount") + 1], "2000")
            sent = _post({"action": "faucet", "to": "bob", "amount": 2000})
            self.assertEqual(sent["code"], 200)
        n = len(self.calls)
        with _env():
            for amt in (2001, "2001", 10**9, 0, -1, "1.5", "x", True):
                sent = _post({"action": "faucet", "to": "bob", "amount": amt})
                self.assertEqual(sent["code"], 400, amt)
        with _env(FPL_FAUCET_MAX="500"):
            self.assertEqual(_post({"action": "faucet", "to": "bob", "amount": 501})["code"], 400)
            self.assertEqual(_post({"action": "faucet", "to": "bob"})["code"], 200)
            self.assertEqual(self.calls[-1][self.calls[-1].index("--amount") + 1], "500")
        for bad in ("abc", "0", "-3", ""):
            with _env(FPL_FAUCET_MAX=bad):
                self.assertEqual(wd.faucet_max(), 2000, bad)
        self.assertEqual(len(self.calls), n + 1)


class BodyParsing(Tripwired):
    def test_non_dict_and_invalid_json_are_400(self):
        for raw in (b"[]", b"[1,2]", b"1", b'"pay"', b"null", b"true", b"{", b"\xff\xfe", b"{'a':1}", b"nan"):
            sent = _post_raw(raw)
            self.assertEqual(sent.get("code"), 400, raw)
        for length in ("x", "-1", str(2 << 20)):
            sent = _post_raw(b"{}", length=length)
            self.assertEqual(sent.get("code"), 400, length)
        self.assertEqual(_post({"action": ["pay"]})["code"], 400)
        self.assertEqual(_post({"action": 5})["code"], 400)
        for v in (False, 0, None, [], {}):
            self.assertEqual(_post({"action": v, "from": "alice", "to": "bob", "amount": 1})["code"], 400, v)
        self.assertEqual(self.hits, [])

    def test_empty_body_defaults_to_pay_and_is_refused(self):
        self.assertEqual(_post_raw(b"")["code"], 400)


if __name__ == "__main__":
    unittest.main()
