#!/usr/bin/env python3
"""Offline tests for pl-eth-lc-relayer.py (no network).

  python3 crates/fd-pl/scripts/test_pl_eth_lc_relayer.py

Amsterdam and Gloas fixtures live in scripts/fixtures/gloas (repo-relative),
so a clean checkout runs them. FPL_GLOAS_DIR / FPL_GLOAS_EXEC_HEADERS override.
"""
from __future__ import annotations

import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("relayer", _HERE / "pl-eth-lc-relayer.py")
rel = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rel)

# Captured from encode_exec_header before the Amsterdam fields existed.
# A pre-Amsterdam block must still produce these exact bytes.
_PRE_BLOCK = {
    "parentHash": "0x" + "11" * 32,
    "sha3Uncles": "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347",
    "miner": "0x" + "22" * 20,
    "stateRoot": "0x" + "33" * 32,
    "transactionsRoot": "0x" + "44" * 32,
    "receiptsRoot": "0x" + "55" * 32,
    "logsBloom": "0x" + "00" * 256,
    "difficulty": "0x0",
    "number": "0x10",
    "gasLimit": "0x1c9c380",
    "gasUsed": "0x5208",
    "timestamp": "0x66000000",
    "extraData": "0x",
    "mixHash": "0x" + "66" * 32,
    "nonce": "0x0000000000000000",
    "baseFeePerGas": "0x7",
    "withdrawalsRoot": "0x" + "77" * 32,
    "blobGasUsed": "0x0",
    "excessBlobGas": "0x0",
    "parentBeaconBlockRoot": "0x" + "88" * 32,
    "requestsHash": "0x" + "99" * 32,
}
_PRE_RLP = (
    "f9025da01111111111111111111111111111111111111111111111111111111111111111"
    "a01dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347"
    "942222222222222222222222222222222222222222"
    "a03333333333333333333333333333333333333333333333333333333333333333"
    "a04444444444444444444444444444444444444444444444444444444444444444"
    "a05555555555555555555555555555555555555555555555555555555555555555"
    "b90100" + "00" * 256 +
    "80108401c9c380825208846600000080"
    "a06666666666666666666666666666666666666666666666666666666666666666"
    "88000000000000000007"
    "a07777777777777777777777777777777777777777777777777777777777777777"
    "8080"
    "a08888888888888888888888888888888888888888888888888888888888888888"
    "a09999999999999999999999999999999999999999999999999999999999999999"
)
_PRE_KECCAK = "cec01490984a80831b046b218af2116f8008d2ab6cb2b2f92286dc91a22650ca"

_HEADER_FIELDS = (
    ("parentHash", "b"),
    ("sha3Uncles", "b"),
    ("miner", "b"),
    ("stateRoot", "b"),
    ("transactionsRoot", "b"),
    ("receiptsRoot", "b"),
    ("logsBloom", "b"),
    ("difficulty", "u"),
    ("number", "u"),
    ("gasLimit", "u"),
    ("gasUsed", "u"),
    ("timestamp", "u"),
    ("extraData", "b"),
    ("mixHash", "b"),
    ("nonce", "b"),
    ("baseFeePerGas", "u"),
    ("withdrawalsRoot", "b"),
    ("blobGasUsed", "u"),
    ("excessBlobGas", "u"),
    ("parentBeaconBlockRoot", "b"),
    ("requestsHash", "b"),
    ("blockAccessListHash", "b"),
    ("slotNumber", "u"),
)

def gloas_dir() -> Path | None:
    env = os.environ.get("FPL_GLOAS_DIR", "").strip()
    cands = []
    if env:
        cands.append(Path(env))
    cands.append(_HERE / "fixtures" / "gloas")
    cands.append(_HERE.parent / "data" / "gloas")
    for c in cands:
        if (c / "exec-headers-amsterdam.json").is_file():
            return c
    return None


def amsterdam_path() -> Path | None:
    env = os.environ.get("FPL_GLOAS_EXEC_HEADERS", "").strip()
    if env:
        p = Path(env)
        return p if p.is_file() else None
    d = gloas_dir()
    if d is None:
        return None
    p = d / "exec-headers-amsterdam.json"
    return p if p.is_file() else None


def rlp_payload(buf: bytes):
    if not buf:
        raise ValueError("eof rlp")
    b = buf[0]
    if b < 0x80:
        return False, buf[:1], 1
    if b <= 0xB7:
        n = b - 0x80
        return False, buf[1 : 1 + n], 1 + n
    if b <= 0xBF:
        ln = b - 0xB7
        n = int.from_bytes(buf[1 : 1 + ln], "big")
        return False, buf[1 + ln : 1 + ln + n], 1 + ln + n
    if b <= 0xF7:
        n = b - 0xC0
        return True, buf[1 : 1 + n], 1 + n
    ln = b - 0xF7
    n = int.from_bytes(buf[1 : 1 + ln], "big")
    return True, buf[1 + ln : 1 + ln + n], 1 + ln + n


def rlp_items(buf: bytes) -> list[bytes]:
    is_list, payload, total = rlp_payload(buf)
    if not is_list or total != len(buf):
        raise ValueError("want one rlp list")
    out = []
    i = 0
    while i < len(payload):
        _, _, n = rlp_payload(payload[i:])
        out.append(payload[i : i + n])
        i += n
    return out


def rlp_body(item: bytes) -> bytes:
    _, payload, total = rlp_payload(item)
    if total != len(item):
        raise ValueError("truncated rlp item")
    return payload


def block_from_rlp(raw: bytes) -> dict:
    items = rlp_items(raw)
    if len(items) < 21:
        raise ValueError(f"header has {len(items)} fields")
    block = {}
    for (name, kind), item in zip(_HEADER_FIELDS, items):
        payload = rlp_body(item)
        if kind == "u":
            n = int.from_bytes(payload, "big") if payload else 0
            block[name] = hex(n)
        else:
            block[name] = "0x" + payload.hex()
    return block


def _update_body(period: int) -> dict:
    slot = period * rel.SLOTS_PER_PERIOD + 10
    return {
        "attested_header": {"beacon": {"slot": str(slot)}},
        "finalized_header": {
            "beacon": {"slot": str(slot - 5)},
            "execution_block_hash": "0x" + "cd" * 32,
        },
        "signature_slot": str(slot + 1),
        "sync_aggregate": {},
    }


def _update_body_numbered(period: int, number: int = 50) -> dict:
    """Pre-Gloas update: execution payload carries a block number."""
    body = _update_body(period)
    header = body["finalized_header"]
    header.pop("execution_block_hash", None)
    header["execution"] = {"block_number": number, "block_hash": "0x" + "ab" * 32}
    return body


def _finality(slot: int, sig: int) -> dict:
    return {
        "version": "gloas",
        "data": {
            "attested_header": {"beacon": {"slot": str(sig - 1)}},
            "finalized_header": {
                "beacon": {"slot": str(slot)},
                "execution": {"block_number": "10", "block_hash": "0x" + "ab" * 32},
            },
            "signature_slot": str(sig),
            "sync_aggregate": {"sync_committee_bits": "0xff"},
        },
    }


class BeaconAndPlanner(unittest.TestCase):
    def test_operator_sentence_and_lodestar_default(self):
        self.assertIn(
            "The deployed host1 copy of this script differs from the repo; the operator must diff and reconcile before replacing it.",
            rel.__doc__ or "",
        )
        src = (_HERE / "eth_bridge_config.py").read_text()
        self.assertIn('or "https://lodestar-sepolia.chainsafe.io"', src)
        self.assertNotIn("ethereum-sepolia-beacon-api.publicnode.com", src)
        rel_src = Path(rel.__file__).read_text()
        self.assertIn('os.environ.get("SEPOLIA_BEACON", BEACON_RPC)', rel_src)
        doc = rel.__doc__ or ""
        self.assertIn("before waiting for inclusion", doc)
        self.assertIn("before any mint", doc)
        cfg = json.loads((_HERE.parent / "config" / "sepolia-qc.json").read_text())
        self.assertEqual(cfg["beacon_rpc"], "https://lodestar-sepolia.chainsafe.io")

    def test_publicnode_beacon_warns_without_failing(self):
        frozen = "https://ethereum-sepolia-beacon-api.publicnode.com"
        buf = io.StringIO()
        with patch.object(rel, "BEACON", frozen), redirect_stdout(buf):
            rel.announce_beacon()
        text = buf.getvalue()
        self.assertIn(f"beacon {frozen}", text)
        self.assertIn("WARNING", text)
        self.assertIn("1378", text)
        self.assertIn("SEPOLIA_BEACON", text)
        self.assertIn("run/bridge/sepolia-qc.json", text)

        buf = io.StringIO()
        with patch.object(rel, "BEACON", "https://lodestar-sepolia.chainsafe.io"), redirect_stdout(buf):
            rel.announce_beacon()
        text = buf.getvalue()
        self.assertIn("beacon https://lodestar-sepolia.chainsafe.io", text)
        self.assertNotIn("WARNING", text)

        calls = []
        with patch.object(rel, "announce_beacon", lambda: calls.append("announce")), patch.object(
            rel, "cmd_update", lambda soft=False: calls.append("update")
        ), patch.object(rel, "cmd_daemon", lambda: calls.append("daemon")), patch.object(
            sys, "argv", ["pl-eth-lc-relayer.py", "update"]
        ):
            self.assertEqual(rel.main(), 0)
        self.assertEqual(calls, ["announce", "update"])
        calls.clear()
        with patch.object(rel, "announce_beacon", lambda: calls.append("announce")), patch.object(
            rel, "cmd_daemon", lambda: calls.append("daemon")
        ), patch.object(sys, "argv", ["pl-eth-lc-relayer.py", "daemon"]):
            self.assertEqual(rel.main(), 0)
        self.assertEqual(calls, ["announce", "daemon"])

    def test_catchup_periods(self):
        # LC at 1378, beacon finality signed in 1380 → updates 1379 then 1380.
        self.assertEqual(rel.catchup_periods(1378, 1380), [1379, 1380])
        self.assertEqual(rel.catchup_periods(1378, 1379), [1379])
        self.assertEqual(rel.catchup_periods(1380, 1380), [])
        self.assertEqual(rel.catchup_periods(1381, 1380), [])
        self.assertEqual(rel.catchup_periods("1378", "1382"), [1379, 1380, 1381, 1382])

    def test_unwrap_list_data_and_versioned(self):
        body = _update_body(1379)
        self.assertEqual(rel.unwrap_lc_update(body)["data"]["signature_slot"], body["signature_slot"])
        self.assertEqual(rel.unwrap_lc_update({"data": body})["data"], body)
        wrapped = {"version": "gloas", "data": body}
        self.assertEqual(rel.unwrap_lc_update(wrapped)["data"]["attested_header"], body["attested_header"])
        self.assertNotIn("version", rel.unwrap_lc_update(wrapped))
        listed = [{"version": "gloas", "data": body}]
        got = rel.unwrap_lc_update(listed)
        self.assertEqual(got["data"]["finalized_header"]["execution_block_hash"], "0x" + "cd" * 32)
        # Versioned LC headers (beacon nested under data) are peeled.
        nested = {
            "version": "gloas",
            "data": {
                "attested_header": {
                    "version": "gloas",
                    "data": {"beacon": {"slot": "1"}, "execution_block_hash": "0xaa"},
                },
                "finalized_header": {
                    "version": "gloas",
                    "data": {"beacon": {"slot": "9"}, "execution_block_hash": "0xbb"},
                },
                "signature_slot": "10",
                "sync_aggregate": {},
            },
        }
        peeled = rel.unwrap_lc_update(nested)
        self.assertEqual(peeled["data"]["finalized_header"]["beacon"]["slot"], "9")
        self.assertNotIn("version", peeled["data"]["finalized_header"])
        self.assertEqual(rel._finalized_slot(peeled), 9)
        with self.assertRaises(ValueError):
            rel.unwrap_lc_update([])
        with self.assertRaises(ValueError):
            rel.unwrap_lc_update({"version": "gloas", "data": {}})

    def test_hash_only_vs_execution_payload(self):
        gloas = {"data": {"finalized_header": {"execution_block_hash": "0xCD"}}}
        self.assertEqual(rel.finalized_exec_ref(gloas), (0, "cd"))
        self.assertEqual(rel.hash_only_head(None, gloas), "cd")
        full = {
            "data": {
                "finalized_header": {
                    "execution": {"block_number": "0x10", "block_hash": "0xAB"}
                }
            }
        }
        self.assertEqual(rel.finalized_exec_ref(full), (16, "ab"))
        self.assertEqual(rel.hash_only_head({"execution_number": 16, "execution_block_hash": "ab"}, gloas), "")
        self.assertEqual(
            rel.hash_only_head({"execution_number": 0, "execution_block_hash": "0xEE"}, full),
            "ee",
        )
        self.assertFalse(rel.mint_head_ready({"execution_number": 0, "execution_block_hash": "ee"}, 5))
        self.assertTrue(rel.mint_head_ready({"execution_number": 5, "execution_block_hash": "ee"}, 5))
        self.assertFalse(rel.mint_head_ready({"execution_number": 4, "execution_block_hash": "ee"}, 5))
        # No hash and number 0 is the old "head at genesis" comparison, not hash-only.
        self.assertTrue(rel.mint_head_ready({"execution_number": 0, "execution_block_hash": ""}, 0))
        # Number alone is not a resolved head: the stored chain must reach it.
        self.assertFalse(
            rel.lc_head_resolved({"execution_number": 5, "execution_block_hash": "ee", "eth_exec_tip": 4})
        )
        self.assertFalse(
            rel.lc_head_resolved({"execution_number": 0, "execution_block_hash": "ee", "eth_exec_tip": 5})
        )
        self.assertTrue(
            rel.lc_head_resolved({"execution_number": 5, "execution_block_hash": "ee", "eth_exec_tip": 5})
        )


class HeaderRlp(unittest.TestCase):
    def test_pre_amsterdam_byte_identical(self):
        raw = rel.encode_exec_header(_PRE_BLOCK)
        self.assertEqual(raw.hex(), _PRE_RLP)
        self.assertEqual(rel.keccak256(raw).hex(), _PRE_KECCAK)
        self.assertEqual(len(rlp_items(raw)), 21)
        absent = dict(_PRE_BLOCK)
        absent["blockAccessListHash"] = None
        absent["slotNumber"] = ""
        self.assertEqual(rel.encode_exec_header(absent), raw)
        # Both new fields, in order: bytes32 then uint.
        ext = dict(_PRE_BLOCK)
        ext["blockAccessListHash"] = "0x" + "ab" * 32
        ext["slotNumber"] = "0xac619f"
        got = rel.encode_exec_header(ext)
        self.assertNotEqual(got, raw)
        items = rlp_items(got)
        self.assertEqual(len(items), 23)
        self.assertEqual(rlp_body(items[21]), bytes.fromhex("ab" * 32))
        self.assertEqual(int.from_bytes(rlp_body(items[22]), "big"), 0xAC619F)
        # Re-encoding the decoded form is stable, and keccak matches header_obj.
        again = rel.encode_exec_header(block_from_rlp(got))
        self.assertEqual(again, got)

    def test_amsterdam_fixture_keccak(self):
        path = amsterdam_path()
        if path is None:
            self.skipTest("exec-headers-amsterdam.json not present")
        rows = json.loads(path.read_text())
        self.assertGreaterEqual(len(rows), 1)
        for row in rows:
            raw = bytes.fromhex(row["rlp_hex"])
            self.assertEqual(rel.keccak256(raw).hex(), row["hash"], row["number"])
            block = block_from_rlp(raw)
            # The fixture is the encoded header, not eth_getBlock JSON. Rebuild
            # that JSON from the RLP field order and re-encode.
            encoded = rel.encode_exec_header(block)
            self.assertEqual(encoded.hex(), row["rlp_hex"], row["number"])
            self.assertEqual(rel.keccak256(encoded).hex(), row["hash"])
            obj = rel.header_obj(dict(block, hash="0x" + row["hash"]))
            self.assertEqual(obj["hash"], row["hash"])
            self.assertEqual(obj["number"], row["number"])
            self.assertEqual(obj["parent_hash"], row["parent_hash"])
            self.assertEqual(obj["receipts_root"], row["receipts_root"])
            items = rlp_items(raw)
            self.assertEqual(len(items), 23, row["number"])
            self.assertEqual(len(rlp_body(items[21])), 32)
            self.assertGreater(int.from_bytes(rlp_body(items[22]), "big"), 0)

    def test_gloas_fixtures_are_hash_only_and_periods_match_planner(self):
        d = gloas_dir()
        if d is None or not (d / "finality-update-gloas-11305567.json").is_file():
            self.skipTest("gloas LC fixtures not present")
        fin = json.loads((d / "finality-update-gloas-11305567.json").read_text())
        unwrapped = rel.unwrap_lc_update([fin])  # updates endpoint is a list
        self.assertEqual(rel.unwrap_lc_update(fin), unwrapped)
        num, eh = rel.finalized_exec_ref(unwrapped)
        self.assertEqual(num, 0)
        self.assertEqual(eh, fin["data"]["finalized_header"]["execution_block_hash"].lower().removeprefix("0x"))
        beacon_period = rel._sig_slot(unwrapped) // rel.SLOTS_PER_PERIOD
        self.assertEqual(beacon_period, 1380)
        # Live LC sits at 1378; the fixture's finality is signed in 1380.
        self.assertEqual(rel.catchup_periods(1378, beacon_period), [1379, 1380])
        upd = json.loads((d / "update-gloas-period-1379.json").read_text())
        one = rel.unwrap_lc_update([{"version": upd.get("version", "gloas"), "data": upd["data"]}])
        unum, uhash = rel.finalized_exec_ref(one)
        self.assertEqual(unum, 0)
        self.assertEqual(
            uhash,
            upd["data"]["finalized_header"]["execution_block_hash"].lower().removeprefix("0x"),
        )
        self.assertEqual(rel._sig_slot(one) // rel.SLOTS_PER_PERIOD, 1379)
        # The 1379 header hash is the first Amsterdam exec header.
        headers = json.loads((d / "exec-headers-amsterdam.json").read_text())
        self.assertEqual(headers[0]["hash"], uhash)


class CatchupHttp(unittest.TestCase):
    def _run(self, state, http_get, wait_lc, eth_rpc=None, events=None):
        submits = []

        # Committed sequence, separate from the in-process floor. alloc_fresh
        # bumps the floor before the confirm re-read; returning the floor
        # there looks like N committed mid-flight. This fake node packs when
        # submit() returns, which is when the committed sequence catches up.
        committed = {"n": 0}

        def submit(kind, path, sequence=None):
            if events is not None:
                events.append(("submit", kind))
            submits.append((kind, Path(path).name, json.loads(Path(path).read_text())))
            committed["n"] = rel._seq_floor
            return "ok"

        def account_sequence():
            return committed["n"]

        def status():
            return {"eth_lc": dict(state)}

        if eth_rpc is None:
            def eth_rpc(method, params):
                raise AssertionError(f"unexpected EL {method}")

        def wrapped_wait(pred, tries=60, label=""):
            if events is not None:
                events.append(("wait", label))
            return wait_lc(pred, tries=tries, label=label)

        rel._seq_floor = 0
        with patch.object(rel, "http_get", http_get), patch.object(rel, "status", status), patch.object(
            rel, "submit", submit
        ), patch.object(rel, "account_sequence", account_sequence), patch.object(
            rel, "wait_lc", wrapped_wait
        ), patch.object(rel, "eth_rpc", eth_rpc), patch.object(
            rel.time, "sleep", lambda *_a, **_k: None
        ):
            out = rel.cmd_update(soft=True)
        return out, submits

    def test_periods_then_finality_with_mocked_http(self):
        # signature slot 11305633 is period 1380; finalized slot is later in that period.
        sig = 11_305_633
        fin_slot = 11_305_567
        calls = []
        state = {
            "period": 1378,
            "next_pks": 512,
            "finalized_slot": 11_290_112,
            "execution_number": 50,
            "execution_block_hash": "ab" * 32,
            "eth_exec_tip": 40,
        }

        def http_get(url):
            calls.append(url)
            if url.endswith("/finality_update"):
                return _finality(fin_slot, sig)
            self.assertIn("/updates?", url)
            period = int(url.split("start_period=")[1].split("&")[0])
            self.assertIn("count=1", url)
            return [{"version": "gloas", "data": _update_body_numbered(period)}]

        def wait_lc(pred, tries=60, label=""):
            if label.startswith("period>="):
                state["period"] = int(label.split(">=")[1])
                state["finalized_slot"] = state["period"] * rel.SLOTS_PER_PERIOD
            elif label == "finalized_slot":
                state["finalized_slot"] = fin_slot
            else:
                raise AssertionError(label)
            lc = dict(state)
            self.assertTrue(pred(lc), label)
            return lc

        out, submits = self._run(state, http_get, wait_lc)
        periods = [
            int(u.split("start_period=")[1].split("&")[0]) for u in calls if "start_period=" in u
        ]
        self.assertEqual(periods, [1379, 1380])
        self.assertEqual(sum(1 for u in calls if u.endswith("/finality_update")), 1)
        self.assertEqual([s[1] for s in submits], [
            "sepolia-lc-period.json",
            "sepolia-lc-period.json",
            "sepolia-lc-finality.json",
        ])
        # Submitted JSON is the unwrapped {"data": ...} shape, not the list.
        self.assertIn("attested_header", submits[0][2]["data"])
        self.assertNotIn("version", submits[0][2])
        self.assertEqual(out["period"], 1380)
        self.assertEqual(out["finalized_slot"], fin_slot)
        # finality GET happens first; period updates are submitted before finality.
        self.assertLess(calls.index(next(u for u in calls if "start_period=1379" in u)), len(calls))
        self.assertTrue(calls[0].endswith("/finality_update"))

    def test_missing_next_committee_fetches_current_period_first(self):
        sig = 11_305_633
        calls = []
        state = {
            "period": 1378,
            "next_pks": 0,
            "finalized_slot": 100,
            "execution_number": 50,
            "execution_block_hash": "ab" * 32,
        }

        def http_get(url):
            calls.append(url)
            if url.endswith("/finality_update"):
                return _finality(11_305_567, sig)
            period = int(url.split("start_period=")[1].split("&")[0])
            return [{"version": "fulu", "data": _update_body_numbered(period)}]

        def wait_lc(pred, tries=60, label=""):
            if label == "next_pks":
                state["next_pks"] = 512
            elif label.startswith("period>="):
                state["period"] = int(label.split(">=")[1])
            elif label == "finalized_slot":
                state["finalized_slot"] = 11_305_567
            lc = dict(state)
            self.assertTrue(pred(lc), label)
            return lc

        _out, submits = self._run(state, http_get, wait_lc)
        periods = [
            int(u.split("start_period=")[1].split("&")[0]) for u in calls if "start_period=" in u
        ]
        self.assertEqual(periods, [1378, 1379, 1380])
        self.assertEqual(submits[0][1], "sepolia-lc-next.json")
        self.assertEqual(submits[-1][1], "sepolia-lc-finality.json")

    def test_soft_period_timeout_does_not_submit_finality(self):
        calls = []
        state = {
            "period": 1378,
            "next_pks": 512,
            "finalized_slot": 100,
            "execution_number": 50,
            "execution_block_hash": "ab" * 32,
        }

        def http_get(url):
            calls.append(url)
            if url.endswith("/finality_update"):
                return _finality(11_305_567, 11_305_633)
            return [{"version": "gloas", "data": _update_body_numbered(1379)}]

        def wait_lc(pred, tries=60, label=""):
            raise SystemExit(f"timeout {label}")

        _out, submits = self._run(state, http_get, wait_lc)
        self.assertEqual(len(submits), 1)
        self.assertEqual(submits[0][1], "sepolia-lc-period.json")
        self.assertNotIn("sepolia-lc-finality.json", [s[1] for s in submits])

    def test_hash_only_head_resolved_by_block_hash(self):
        parent = "11" * 32
        mid = _sealed(99, parent)
        end = _sealed(
            100,
            mid["hash"].removeprefix("0x"),
            blockAccessListHash="0x" + "ab" * 32,
            slotNumber="0xac619f",
        )
        end_hash = end["hash"].lower().removeprefix("0x")
        state = {
            "period": 1380,
            "next_pks": 512,
            "finalized_slot": 11_305_567,
            "execution_number": 0,
            "execution_block_hash": end_hash,
            "eth_exec_tip": 98,
        }
        rpc_calls = []

        def eth_rpc(method, params):
            rpc_calls.append((method, params))
            if method == "eth_getBlockByHash":
                self.assertEqual(params, ["0x" + end_hash, False])
                return end
            if method == "eth_getBlockByNumber":
                n = int(params[0], 16)
                self.assertEqual(n, 99)
                self.assertIs(params[1], False)
                return mid
            raise AssertionError(method)

        def http_get(url):
            self.assertTrue(url.endswith("/finality_update"))
            return _finality(11_305_567, 11_305_633)

        def wait_lc(pred, tries=60, label=""):
            self.assertTrue(label.startswith("exec-number>="), label)
            state["execution_number"] = 100
            lc = dict(state)
            self.assertTrue(pred(lc), label)
            return lc

        out, submits = self._run(state, http_get, wait_lc, eth_rpc=eth_rpc)
        self.assertEqual(out["execution_number"], 100)
        exec_submits = [s for s in submits if s[0] == "submit-eth-exec-headers"]
        self.assertEqual(len(exec_submits), 1)
        batch = exec_submits[0][2]
        self.assertEqual([h["number"] for h in batch], [99, 100])
        self.assertEqual(batch[-1]["hash"], end_hash)
        self.assertEqual(batch[-1]["parent_hash"], mid["hash"].removeprefix("0x"))
        # Amsterdam fields survived the re-encode (keccak checked inside header_obj).
        self.assertIn("ac619f", bytes.fromhex(batch[-1]["rlp_hex"]).hex())
        self.assertEqual(rpc_calls[0][0], "eth_getBlockByHash")
        self.assertTrue(all(s[0] != "submit-eth-lc-update" for s in submits))

    def test_exec_headers_follow_lc_update_before_wait_and_mint(self):
        # LC at 1378, beacon signature in 1379: one period update, then finality.
        # Both heads are hash-only. Headers must be submitted after each LC
        # update and before the inclusion wait, with no mint in between.
        period = 1379
        sig = period * rel.SLOTS_PER_PERIOD + 100
        fin_slot = period * rel.SLOTS_PER_PERIOD + 50
        first = _sealed(100, "11" * 32)
        second = _sealed(100, "22" * 32)
        h1 = first["hash"].lower().removeprefix("0x")
        h2 = second["hash"].lower().removeprefix("0x")
        state = {
            "period": 1378,
            "next_pks": 512,
            "finalized_slot": 100,
            "execution_number": 40,
            "execution_block_hash": "ab" * 32,
            "eth_exec_tip": 0,
        }
        events = []

        def http_get(url):
            events.append(("get", url))
            if url.endswith("/finality_update"):
                body = _update_body(period)
                body["signature_slot"] = str(sig)
                body["finalized_header"]["beacon"]["slot"] = str(fin_slot)
                body["finalized_header"]["execution_block_hash"] = "0x" + h2
                return {"version": "gloas", "data": body}
            got = int(url.split("start_period=")[1].split("&")[0])
            self.assertEqual(got, period)
            body = _update_body(got)
            body["finalized_header"]["execution_block_hash"] = "0x" + h1
            return [{"version": "gloas", "data": body}]

        def eth_rpc(method, params):
            events.append(("rpc", method))
            self.assertEqual(method, "eth_getBlockByHash")
            h = params[0].lower().removeprefix("0x")
            if h == h1:
                return first
            if h == h2:
                return second
            raise AssertionError(h)

        def wait_lc(pred, tries=60, label=""):
            if label.startswith("period>="):
                state["period"] = int(label.split(">=")[1])
                state["finalized_slot"] = state["period"] * rel.SLOTS_PER_PERIOD
            elif label == "finalized_slot":
                state["finalized_slot"] = fin_slot
            elif label.startswith("exec-number>="):
                state["execution_number"] = int(label.split(">=")[1])
                state["eth_exec_tip"] = state["execution_number"]
            else:
                raise AssertionError(label)
            lc = dict(state)
            self.assertTrue(pred(lc), label)
            return lc

        _out, submits = self._run(state, http_get, wait_lc, eth_rpc=eth_rpc, events=events)
        seq = [(k, v) for k, v in events if k in ("submit", "wait")]
        self.assertEqual(
            seq,
            [
                ("submit", "submit-eth-lc-update"),
                ("submit", "submit-eth-exec-headers"),
                ("wait", "period>=1379"),
                ("wait", "exec-number>=100"),
                ("submit", "submit-eth-lc-update"),
                ("submit", "submit-eth-exec-headers"),
                ("wait", "finalized_slot"),
                ("wait", "exec-number>=100"),
            ],
        )
        self.assertNotIn("submit-rail-deposit", [s[0] for s in submits])
        # Head batch is fetched before the LC tx is submitted.
        rpc_at = [i for i, e in enumerate(events) if e[0] == "rpc"]
        lc_at = [i for i, e in enumerate(events) if e == ("submit", "submit-eth-lc-update")]
        self.assertEqual(len(rpc_at), 2)
        self.assertLess(rpc_at[0], lc_at[0])
        self.assertLess(rpc_at[1], lc_at[1])
        for i, (kind, _name, blob) in enumerate(submits):
            if kind != "submit-eth-lc-update":
                continue
            num, _h = rel.finalized_exec_ref(blob)
            self.assertEqual(num, 0)
            self.assertEqual(submits[i + 1][0], "submit-eth-exec-headers")
            self.assertEqual(submits[i + 1][2][-1]["number"], 100)

    def test_hash_only_soft_timeout_does_not_mint_or_submit_finality(self):
        block = _sealed(100, "33" * 32)
        h = block["hash"].lower().removeprefix("0x")
        state = {
            "period": 1378,
            "next_pks": 512,
            "finalized_slot": 100,
            "execution_number": 40,
            "execution_block_hash": "ab" * 32,
            "eth_exec_tip": 0,
        }
        events = []

        def http_get(url):
            if url.endswith("/finality_update"):
                return _finality(11_305_567, 11_305_633)
            body = _update_body(1379)
            body["finalized_header"]["execution_block_hash"] = "0x" + h
            return [{"version": "gloas", "data": body}]

        def eth_rpc(method, params):
            self.assertEqual(method, "eth_getBlockByHash")
            return block

        def wait_lc(pred, tries=60, label=""):
            raise SystemExit(f"timeout {label}")

        _out, submits = self._run(state, http_get, wait_lc, eth_rpc=eth_rpc, events=events)
        seq = [(k, v) for k, v in events if k in ("submit", "wait")]
        self.assertEqual(
            seq,
            [
                ("submit", "submit-eth-lc-update"),
                ("submit", "submit-eth-exec-headers"),
                ("wait", "period>=1379"),
            ],
        )
        self.assertEqual([s[1] for s in submits], ["sepolia-lc-period.json", "sepolia-exec-headers.json"])
        self.assertNotIn("submit-rail-deposit", [s[0] for s in submits])
        self.assertNotIn("sepolia-lc-finality.json", [s[1] for s in submits])

    def test_unbuildable_hash_only_head_skips_the_lc_update(self):
        state = {
            "period": 1378,
            "next_pks": 512,
            "finalized_slot": 100,
            "execution_number": 40,
            "execution_block_hash": "ab" * 32,
            "eth_exec_tip": 0,
        }

        def http_get(url):
            if url.endswith("/finality_update"):
                return _finality(11_305_567, 11_305_633)
            return [{"version": "gloas", "data": _update_body(1379)}]

        def eth_rpc(method, params):
            raise RuntimeError("el down")

        def wait_lc(pred, tries=60, label=""):
            raise AssertionError(f"waited {label}")

        _out, submits = self._run(state, http_get, wait_lc, eth_rpc=eth_rpc)
        self.assertEqual(submits, [])

    def test_bootstrap_hash_only_headers_follow_without_mint(self):
        block = _sealed(7, "44" * 32)
        h = block["hash"].lower().removeprefix("0x")
        events = []

        def eth_rpc(method, params):
            self.assertEqual(method, "eth_getBlockByHash")
            self.assertEqual(params[0].lower(), "0x" + h)
            return block

        def status():
            return {}

        committed = {"n": 0}

        def submit(kind, path, sequence=None):
            events.append(kind)
            committed["n"] = rel._seq_floor
            return "ok"

        def account_sequence():
            return committed["n"]

        doc = {
            "data": {
                "header": {
                    "beacon": {"slot": "11305567"},
                    "execution_block_hash": "0x" + h,
                }
            }
        }
        rel._seq_floor = 0
        with tempfile.TemporaryDirectory() as td:
            boot = Path(td) / "boot.json"
            boot.write_text(json.dumps(doc))
            with patch.object(rel, "BOOT", boot), patch.object(rel, "status", status), patch.object(
                rel, "submit", submit
            ), patch.object(rel, "account_sequence", account_sequence), patch.object(
                rel, "eth_rpc", eth_rpc
            ), patch.object(rel.time, "sleep", lambda *_a, **_k: None):
                rel.cmd_bootstrap()
        self.assertEqual(events, ["submit-eth-lc-bootstrap", "submit-eth-exec-headers"])

    def test_bootstrap_with_execution_payload_does_not_fetch_headers(self):
        events = []

        def eth_rpc(method, params):
            raise AssertionError(method)

        def status():
            return {}

        committed = {"n": 0}

        def submit(kind, path, sequence=None):
            events.append(kind)
            committed["n"] = rel._seq_floor
            return "ok"

        def account_sequence():
            return committed["n"]

        doc = {
            "data": {
                "header": {
                    "beacon": {"slot": "1"},
                    "execution": {"block_number": 10, "block_hash": "0x" + "ab" * 32},
                }
            }
        }
        rel._seq_floor = 0
        with tempfile.TemporaryDirectory() as td:
            boot = Path(td) / "boot.json"
            boot.write_text(json.dumps(doc))
            with patch.object(rel, "BOOT", boot), patch.object(rel, "status", status), patch.object(
                rel, "submit", submit
            ), patch.object(rel, "account_sequence", account_sequence), patch.object(
                rel, "eth_rpc", eth_rpc
            ):
                rel.cmd_bootstrap()
        self.assertEqual(events, ["submit-eth-lc-bootstrap"])

    def test_bootstrap_fixtures_split_hash_only_from_payload(self):
        d = gloas_dir()
        if d is None:
            self.skipTest("gloas fixtures not present")
        gloas_p = d / "bootstrap-gloas-11305567.json"
        fulu_p = d / "bootstrap-fulu-11280384.json"
        if not gloas_p.is_file() or not fulu_p.is_file():
            self.skipTest("bootstrap fixtures not present")
        num, h = rel.bootstrap_exec_ref(json.loads(gloas_p.read_text()))
        self.assertEqual(num, 0)
        self.assertEqual(len(h), 64)
        num, h = rel.bootstrap_exec_ref(json.loads(fulu_p.read_text()))
        self.assertEqual(num, 11840768)
        self.assertEqual(len(h), 64)


class SequenceChain(unittest.TestCase):
    """Real submit() / alloc path. ctl is mocked; nothing is sent."""

    def setUp(self):
        rel._seq_floor = 0
        rel._seq_stuck_since = None
        rel._stuck_watch_committed = None

    def _ctl(self, state, calls, fail_at=None, commit=False):
        def ctl(*args, timeout=180):
            calls.append(tuple(args))
            if args[0] == "account":
                body = {"type": "account", "body": {"account": "watcher-eth-lc", "sequence": state["n"]}}
                return json.dumps(body)
            if args[0] == "submit-eth-exec-headers" and fail_at is not None and "--sequence" in args:
                seq = int(args[args.index("--sequence") + 1])
                if seq == fail_at:
                    raise subprocess.CalledProcessError(1, list(args), output="rejected")
            if commit and str(args[0]).startswith("submit-"):
                if "--sequence" in args:
                    seq = int(args[args.index("--sequence") + 1])
                    state["n"] = max(int(state["n"]), seq + 1)
                else:
                    state["n"] = int(state["n"]) + 1
            return "ok tx"
        return ctl

    def _seqs(self, calls):
        """(kind, sequence or None) for submit commands, in order."""
        out = []
        for args in calls:
            if not args or not str(args[0]).startswith("submit-"):
                continue
            if "--sequence" in args:
                seq = int(args[args.index("--sequence") + 1])
            else:
                seq = None
            out.append((args[0], seq))
        return out

    def test_update_and_chunks_get_distinct_increasing_sequences(self):
        import subprocess
        calls = []
        state = {"n": 5}
        headers = [{"number": 10, "hash": "aa"}, {"number": 11, "hash": "bb"}]
        with patch.object(rel, "ctl", self._ctl(state, calls, commit=True)), patch.object(rel, "HEADER_CHUNK", 1):
            rel.submit_lc_then_headers("submit-eth-lc-update", Path("/tmp/lc.json"), headers)
        self.assertEqual(
            self._seqs(calls),
            [
                ("submit-eth-lc-update", None),
                ("submit-eth-exec-headers", 6),
                ("submit-eth-exec-headers", 7),
            ],
        )
        # The LC command has no --sequence flag (ctl would ignore it). The
        # account query immediately before it is the N that resolve_seq(0) uses.
        kinds = [c[0] for c in calls]
        self.assertEqual(kinds[0], "account")
        self.assertLess(kinds.index("account"), kinds.index("submit-eth-lc-update"))
        self.assertNotIn("--sequence", calls[kinds.index("submit-eth-lc-update")])
        # alloc_fresh reads N, then alloc_confirmed_lc_sequence reads it
        # again before the submit. Nothing re-reads between the update and
        # the header chunks (that re-read would evict the LC tx).
        upd = kinds.index("submit-eth-lc-update")
        last_submit = max(i for i, k in enumerate(kinds) if str(k).startswith("submit-"))
        self.assertEqual(kinds[:upd].count("account"), 2)
        # No re-read between the LC tx and the header chunks of this wave.
        # The account query after the last submit is the wave commit wait.
        self.assertNotIn("account", kinds[upd:last_submit])
        self.assertGreaterEqual(kinds[last_submit + 1 :].count("account"), 1)

    def test_sequence_zero_is_not_passed_as_flag(self):
        calls = []
        state = {"n": 0}
        headers = [{"number": 1, "hash": "aa"}, {"number": 2, "hash": "bb"}]
        with patch.object(rel, "ctl", self._ctl(state, calls, commit=True)), patch.object(rel, "HEADER_CHUNK", 1):
            rel.submit_header_chain(headers)
        self.assertEqual(
            self._seqs(calls),
            [
                ("submit-eth-exec-headers", None),
                ("submit-eth-exec-headers", 1),
            ],
        )

    def test_mid_chain_failure_stops_and_does_not_reuse_inflight(self):
        import subprocess
        calls = []
        state = {"n": 5}
        headers = [{"number": 10, "hash": "aa"}, {"number": 11, "hash": "bb"}]
        with patch.object(rel, "ctl", self._ctl(state, calls, fail_at=7)), patch.object(
            rel, "HEADER_CHUNK", 1
        ):
            with self.assertRaises(subprocess.CalledProcessError):
                rel.submit_lc_then_headers("submit-eth-lc-update", Path("/tmp/lc.json"), headers)
        self.assertEqual(
            self._seqs(calls),
            [
                ("submit-eth-lc-update", None),
                ("submit-eth-exec-headers", 6),
                ("submit-eth-exec-headers", 7),
            ],
        )
        # 5 and 6 were accepted. 7 was not. Floor holds 7 until the account
        # query reaches it, so a new chain must not submit at 5 again.
        calls.clear()
        with patch.object(rel, "ctl", self._ctl(state, calls)), patch.object(rel, "HEADER_CHUNK", 1):
            with self.assertRaises(rel.SequenceInFlight):
                rel.submit_lc_then_headers("submit-eth-lc-update", Path("/tmp/lc.json"), headers)
        self.assertEqual(self._seqs(calls), [])
        # Committed next is 7: the accepted prefix committed, 7 was not in
        # flight, so the next chain may use 7 and must not use 5 or 6.
        state["n"] = 7
        calls.clear()
        with patch.object(rel, "ctl", self._ctl(state, calls, commit=True)), patch.object(rel, "HEADER_CHUNK", 1):
            rel.submit_lc_then_headers(
                "submit-eth-lc-update", Path("/tmp/lc.json"), [{"number": 12, "hash": "cc"}]
            )
        seqs = self._seqs(calls)
        self.assertEqual(seqs[0][0], "submit-eth-lc-update")
        header_seqs = [seq for kind, seq in seqs if kind == "submit-eth-exec-headers"]
        self.assertEqual(header_seqs, [8])
        self.assertNotIn(5, [seq for _, seq in seqs if seq is not None])
        self.assertNotIn(6, [seq for _, seq in seqs if seq is not None])

    def test_dropped_tx_resets_after_timeout_and_not_before(self):
        """Fake clock. A packer drop leaves the floor above committed N."""
        calls = []
        state = {"n": 5}
        clock = {"t": 10_000.0}

        def fake_now():
            return clock["t"]

        ctl = self._ctl(state, calls)
        with patch.object(rel, "ctl", ctl), patch.object(rel, "_now", fake_now):
            rel.submit_lc_then_headers("submit-eth-lc-update", Path("/tmp/lc.json"), [])
            self.assertEqual(rel._seq_floor, 6)
            self.assertEqual(rel._seq_stuck_since, 10_000.0)
            # Committed stays at 5: the accepted tx was dropped, not packed.
            calls.clear()
            clock["t"] = 10_000.0 + rel.SEQ_STUCK_SEC - 1
            with self.assertRaises(rel.SequenceInFlight):
                rel.submit_lc_then_headers("submit-eth-lc-update", Path("/tmp/lc.json"), [])
            self.assertEqual(self._seqs(calls), [])
            self.assertEqual(rel._seq_floor, 6)
            calls.clear()
            clock["t"] = 10_000.0 + rel.SEQ_STUCK_SEC
            rel.submit_lc_then_headers("submit-eth-lc-update", Path("/tmp/lc.json"), [])
        self.assertEqual(self._seqs(calls), [("submit-eth-lc-update", None)])
        self.assertEqual(rel._seq_floor, 6)
        self.assertEqual(rel._seq_stuck_since, clock["t"])

    def test_mempool_hit_is_not_reused_even_after_timeout(self):
        """A view that still lists the sequence blocks the timeout reset."""
        calls = []
        state = {"n": 5}
        clock = {"t": 50_000.0}

        def fake_now():
            return clock["t"]

        def still_there():
            return {5}

        rel._seq_floor = 6
        rel._seq_stuck_since = 0.0
        with patch.object(rel, "ctl", self._ctl(state, calls)), patch.object(
            rel, "_now", fake_now
        ), patch.object(rel, "account_mempool_sequences", still_there):
            with self.assertRaises(rel.SequenceInFlight) as ctx:
                rel.alloc_fresh(1)
        self.assertIn("still in the mempool", str(ctx.exception))
        self.assertEqual(rel._seq_floor, 6)
        self.assertEqual(self._seqs(calls), [])

    def test_mempool_miss_resets_without_waiting_out_the_timeout(self):
        calls = []
        state = {"n": 5}
        clock = {"t": 1_000.0}

        def fake_now():
            return clock["t"]

        def gone():
            return set()

        rel._seq_floor = 6
        rel._seq_stuck_since = clock["t"]  # just became stuck
        with patch.object(rel, "ctl", self._ctl(state, calls)), patch.object(
            rel, "_now", fake_now
        ), patch.object(rel, "account_mempool_sequences", gone):
            seqs = rel.alloc_fresh(1)
        self.assertEqual(seqs, [5])
        self.assertEqual(rel._seq_floor, 6)

    def test_stuck_timer_restarts_when_committed_advances(self):
        """A header chain that is packing must not hit the 180s reset."""
        calls = []
        state = {"n": 5}
        clock = {"t": 0.0}

        def fake_now():
            return clock["t"]

        headers = [{"number": i, "hash": "aa"} for i in range(3)]
        ctl = self._ctl(state, calls)
        with patch.object(rel, "ctl", ctl), patch.object(rel, "_now", fake_now), patch.object(
            rel, "HEADER_CHUNK", 1
        ), patch.object(rel, "wait_wave_committed", lambda *_a, **_k: None):
            rel.submit_header_chain(headers)
        self.assertEqual(rel._seq_floor, 8)
        self.assertEqual(rel._seq_stuck_since, 0.0)
        # Committed moves 5 -> 6 while the floor is still 8, just before the
        # original deadline. The clock restarts, so t=180 must not reset.
        state["n"] = 6
        clock["t"] = rel.SEQ_STUCK_SEC - 1
        with patch.object(rel, "ctl", ctl), patch.object(rel, "_now", fake_now):
            with self.assertRaises(rel.SequenceInFlight):
                rel.alloc_fresh(1)
        self.assertEqual(rel._seq_stuck_since, clock["t"])
        self.assertEqual(rel._seq_floor, 8)
        clock["t"] = rel.SEQ_STUCK_SEC
        with patch.object(rel, "ctl", ctl), patch.object(rel, "_now", fake_now):
            with self.assertRaises(rel.SequenceInFlight):
                rel.alloc_fresh(1)
        self.assertEqual(rel._seq_floor, 8)
        # A full window with no further progress does reset, to the new N.
        restarted = rel.SEQ_STUCK_SEC - 1
        clock["t"] = restarted + rel.SEQ_STUCK_SEC
        with patch.object(rel, "ctl", ctl), patch.object(rel, "_now", fake_now):
            seqs = rel.alloc_fresh(1)
        self.assertEqual(seqs, [6])
        self.assertEqual(rel._seq_floor, 7)

    def test_lc_reread_skips_a_sequence_that_committed_mid_flight(self):
        """Old N committing between reads must not pair an LC at N+1 with a header at N+1."""
        calls = []
        reads = {"n": 0}

        def ctl(*args, timeout=180):
            calls.append(tuple(args))
            if args[0] == "account":
                reads["n"] += 1
                # First alloc sees 5. The confirm read sees 6 (N committed).
                # The retry alloc and its confirm both see 6. After the
                # submits, the wave wait must see the header's sequence commit.
                if any(str(c[0]).startswith("submit-") for c in calls):
                    seq = 8
                else:
                    seq = 5 if reads["n"] == 1 else 6
                body = {"type": "account", "body": {"account": "watcher-eth-lc", "sequence": seq}}
                return json.dumps(body)
            return "ok tx"

        headers = [{"number": 10, "hash": "aa"}]
        with patch.object(rel, "ctl", ctl), patch.object(rel, "HEADER_CHUNK", 1):
            rel.submit_lc_then_headers("submit-eth-lc-update", Path("/tmp/lc.json"), headers)
        self.assertEqual(
            self._seqs(calls),
            [
                ("submit-eth-lc-update", None),
                ("submit-eth-exec-headers", 7),
            ],
        )
        self.assertNotIn(6, [seq for _kind, seq in self._seqs(calls) if seq is not None])


    def test_waves_stay_under_byte_cap_and_wait_for_commit(self):
        """A long catch-up is split into waves. The next wave waits for commit."""
        calls = []
        state = {"n": 10}
        # ~36 KB each, so two fit the wave cap and it is above the 64 KiB floor.
        headers = [{"number": i, "hash": "ab", "rlp_hex": "ff" * 18_000} for i in range(6)]
        one = len(json.dumps([headers[0]]).encode())
        wave_cap = one * 2
        self.assertGreater(wave_cap, rel.HEADER_WAVE_MIN)
        self.assertLess(one * 3, wave_cap * 2)
        sizes = []

        def ctl(*args, timeout=180):
            calls.append(tuple(args))
            if args[0] == "account":
                body = {"type": "account", "body": {"sequence": state["n"]}}
                return json.dumps(body)
            if str(args[0]).startswith("submit-"):
                if "--json-file" in args:
                    raw = Path(args[args.index("--json-file") + 1]).read_bytes()
                    sizes.append(len(raw))
                if "--sequence" in args:
                    seq = int(args[args.index("--sequence") + 1])
                    state["n"] = max(int(state["n"]), seq + 1)
                else:
                    state["n"] = int(state["n"]) + 1
            return "ok tx"

        env = {"ETH_HEADER_WAVE_BYTES": str(wave_cap), "ETH_HEADER_CHUNK_BYTES": str(one + 10)}
        with patch.dict(os.environ, env), patch.object(rel, "ctl", ctl), patch.object(rel, "HEADER_CHUNK", 1):
            rel.submit_header_chain(headers)
        seqs = [seq for _k, seq in self._seqs(calls)]
        self.assertEqual(seqs, [10, 11, 12, 13, 14, 15])
        self.assertEqual(len(sizes), 6)
        for i in range(0, 6, 2):
            self.assertLessEqual(sizes[i] + sizes[i + 1], wave_cap)
        kinds = [c[0] for c in calls]
        submits = [i for i, k in enumerate(kinds) if k == "submit-eth-exec-headers"]
        between = kinds[submits[1] + 1 : submits[2]]
        self.assertIn("account", between)

    def test_stuck_reset_does_not_start_the_next_wave(self):
        calls = []
        state = {"n": 10}
        clock = {"t": 1_000.0}
        headers = [{"number": i, "hash": "ab", "rlp_hex": "ff" * 18_000} for i in range(4)]
        one = len(json.dumps([headers[0]]).encode())
        self.assertGreater(one * 2, rel.HEADER_WAVE_MIN)

        def fake_now():
            return clock["t"]

        def fake_sleep(sec):
            clock["t"] += sec

        ctl = self._ctl(state, calls)
        env = {"ETH_HEADER_WAVE_BYTES": str(one * 2), "ETH_WAVE_POLL_SEC": "30"}
        with patch.dict(os.environ, env), patch.object(rel, "ctl", ctl), patch.object(
            rel, "_now", fake_now
        ), patch.object(rel.time, "sleep", fake_sleep), patch.object(rel, "HEADER_CHUNK", 1):
            with self.assertRaises(rel.SequenceInFlight):
                rel.submit_header_chain(headers)
        seqs = [seq for _k, seq in self._seqs(calls)]
        self.assertEqual(seqs, [10, 11])
        self.assertNotIn(12, seqs)

    def test_oversize_header_refused_before_any_submit(self):
        """RO nit 3: refuse with ALERT. No LC tx, no header, no sequence reserved."""
        small = [{"number": i, "hash": "aa", "rlp_hex": "ab" * 500} for i in range(3)]
        fat = {"number": 3, "hash": "bb", "rlp_hex": "cd" * 40_000}
        headers = small + [fat]
        self.assertGreater(len(json.dumps([fat]).encode()), rel.HEADER_WAVE_MIN)
        for env_val in ("1", str(rel.HEADER_WAVE_MIN)):
            for call in ("chain", "lc"):
                calls = []
                state = {"n": 7}
                rel._seq_floor = 0
                rel._env_warned.clear()
                with patch.dict(os.environ, {"ETH_HEADER_CHUNK_BYTES": env_val}), patch.object(
                    rel, "ctl", self._ctl(state, calls, commit=True)
                ), patch.object(rel, "HEADER_CHUNK", 32), redirect_stdout(io.StringIO()) as buf:
                    with self.assertRaises(rel.OversizeHeader):
                        if call == "chain":
                            rel.submit_header_chain(headers)
                        else:
                            rel.submit_lc_then_headers("submit-eth-lc-update", Path("/tmp/lc.json"), headers)
                self.assertEqual(calls, [], (env_val, call))
                self.assertEqual(rel._seq_floor, 0)
                self.assertIn("ALERT exec header 3", buf.getvalue())
        # The 64 KiB floor means a junk-small cap never refuses real headers.
        with patch.dict(os.environ, {"ETH_HEADER_CHUNK_BYTES": "1"}), patch.object(
            rel, "HEADER_CHUNK", 32
        ), redirect_stdout(io.StringIO()):
            chunks = rel.header_chunks(small)
        self.assertEqual(chunks, [small])

    def test_wave_wait_is_bounded_by_time_not_tries(self):
        """RO nit 2: a slow ctl cannot stretch the wave wait past STUCK+30s (+1 query)."""
        clock = {"t": 5_000.0}
        queries = {"n": 0}
        per_query = 60.0

        def ctl(*args, timeout=180):
            if args[0] == "account":
                queries["n"] += 1
                clock["t"] += per_query
                return json.dumps({"type": "account", "body": {"sequence": 5}})
            raise AssertionError(args)

        def fake_sleep(sec):
            self.assertGreater(sec, 0)
            clock["t"] += sec

        rel._seq_floor = 0  # nothing above committed, so no stuck reset path
        with patch.dict(os.environ, {"ETH_WAVE_POLL_SEC": "2"}), patch.object(rel, "ctl", ctl), patch.object(
            rel, "_now", lambda: clock["t"]
        ), patch.object(rel.time, "sleep", fake_sleep):
            t_start = clock["t"]
            with self.assertRaises(rel.SequenceInFlight) as ctx:
                rel.wait_wave_committed(10)
        spent = clock["t"] - t_start
        self.assertLessEqual(spent, rel.SEQ_STUCK_SEC + rel.WAVE_WAIT_SLACK_SEC + per_query + 2)
        self.assertLessEqual(queries["n"], int((rel.SEQ_STUCK_SEC + 30) // per_query) + 2)
        self.assertIn("still not committed", str(ctx.exception))

    def test_submit_files_are_per_process_not_shared_tmp(self):
        """Copilot r4225239490: daemon and minter must not share /tmp file names."""
        seen = []

        def ctl(*args, timeout=180):
            if "--json-file" in args:
                path = Path(args[args.index("--json-file") + 1])
                seen.append(path)
                self.assertTrue(path.is_file())
            return "ok tx"

        with patch.object(rel, "ctl", ctl), redirect_stdout(io.StringIO()):
            rel.submit_headers([{"number": 1, "hash": "aa"}], sequence=3)
            upd = rel._write_update({"data": {}}, "sepolia-lc-period.json")
        self.assertEqual(len(seen), 1)
        for path in (seen[0], upd):
            self.assertNotEqual(path.parent, Path("/tmp"))
            self.assertTrue(path.parent.name.startswith("fpl-eth-lc-"))
            self.assertEqual(path.parent.stat().st_mode & 0o077, 0)
        self.assertEqual(seen[0].name, "sepolia-exec-headers.json")

    def test_wave_poll_floor_and_cap(self):
        cases = {"": 2.0, "1": 1.0, "0": 0.5, "0.1": 0.5, "-1": 2.0, "nan": 2.0, "inf": 2.0, "x": 2.0, "100": 30.0}
        for raw, want in cases.items():
            rel._env_warned.clear()
            with patch.dict(os.environ, {"ETH_WAVE_POLL_SEC": raw}), redirect_stdout(io.StringIO()):
                self.assertEqual(rel.wave_poll_sec(), want, raw)


class HeaderByteKnobs(unittest.TestCase):
    def setUp(self):
        rel._env_warned.clear()

    def tearDown(self):
        rel._env_warned.clear()

    def test_wave_bytes_junk_uses_default_and_small_is_floored(self):
        d = rel.HEADER_WAVE_DEFAULT
        cases = {"": d, "0": d, "-5": d, "nan": d, "inf": d, "junk": d, "1": rel.HEADER_WAVE_MIN,
                 "1000": rel.HEADER_WAVE_MIN, "100000": 100_000, "999999999": d}
        for raw, want in cases.items():
            with patch.dict(os.environ, {"ETH_HEADER_WAVE_BYTES": raw}), redirect_stdout(io.StringIO()):
                self.assertEqual(rel.header_wave_bytes(), want, raw)

    def test_chunk_bytes_junk_uses_default_and_stays_within_wave(self):
        base = {"ETH_HEADER_WAVE_BYTES": "100000"}
        cases = {"": 100_000, "0": 100_000, "-1": 100_000, "nan": 100_000, "1": rel.HEADER_WAVE_MIN,
                 "70000": 70_000, "200000": 100_000}
        for raw, want in cases.items():
            env = dict(base, ETH_HEADER_CHUNK_BYTES=raw)
            with patch.dict(os.environ, env), redirect_stdout(io.StringIO()):
                self.assertEqual(rel.header_chunk_bytes(), want, raw)
        with patch.dict(os.environ, {"ETH_HEADER_CHUNK_BYTES": "0"}), redirect_stdout(io.StringIO()):
            os.environ.pop("ETH_HEADER_WAVE_BYTES", None)
            self.assertEqual(rel.header_chunk_bytes(), 256 * 1024)

    def test_bad_wave_env_warns_once_per_process(self):
        """RO nit 6: one WARNING, not 2-3 per submission."""
        headers = [{"number": i, "hash": "aa", "rlp_hex": "ab" * 50} for i in range(4)]
        with patch.dict(os.environ, {"ETH_HEADER_WAVE_BYTES": "1"}), patch.object(
            rel, "HEADER_CHUNK", 1
        ), redirect_stdout(io.StringIO()) as buf:
            for _ in range(3):
                rel.header_wave_bytes()
                chunks = rel.header_chunks(headers)
                rel.pack_header_waves(chunks, with_lc=True)
        self.assertEqual(buf.getvalue().count("WARNING ETH_HEADER_WAVE_BYTES"), 1)


class SeqStuckSecEnv(unittest.TestCase):
    def test_non_finite_falls_back_to_default(self):
        for bad in ("nan", "inf", "-inf", "NaN"):
            with patch.dict(os.environ, {"ETH_SEQ_STUCK_SEC": bad}), redirect_stdout(io.StringIO()):
                self.assertEqual(rel._seq_stuck_sec(), 180.0, bad)

    def test_out_of_range_is_clamped(self):
        with patch.dict(os.environ, {"ETH_SEQ_STUCK_SEC": "5"}), redirect_stdout(io.StringIO()):
            self.assertEqual(rel._seq_stuck_sec(), 120.0)
        with patch.dict(os.environ, {"ETH_SEQ_STUCK_SEC": "9999"}), redirect_stdout(io.StringIO()):
            self.assertEqual(rel._seq_stuck_sec(), 300.0)


class Alerts(unittest.TestCase):
    def setUp(self):
        rel._exec_watch["number"] = None
        rel._exec_watch["since"] = None
        rel._last_deposit_scan = 0.0
        rel._last_stale_alert = 0.0
        rel._scan_resume["addr"] = ""
        rel._scan_resume["from_block"] = -1
        rel._block_ts_cache.clear()
        rel._block_ts_failed.clear()

    def test_stale_exec_head_alerts_after_threshold(self):
        self.assertIsNone(rel.note_exec_head({"execution_number": 10}, now=1_000))
        self.assertIsNone(rel.note_exec_head({"execution_number": 10}, now=1_000 + rel.STALE_EXEC_SEC - 1))
        line = rel.note_exec_head({"execution_number": 10}, now=1_000 + rel.STALE_EXEC_SEC)
        self.assertIsNotNone(line)
        self.assertTrue(line.startswith("ALERT stale LC exec head"))
        # Same hour, next poll: throttled by ETH_ALERT_EVERY_SEC.
        self.assertIsNone(
            rel.note_exec_head({"execution_number": 10}, now=1_000 + rel.STALE_EXEC_SEC + 1)
        )
        again = rel.note_exec_head(
            {"execution_number": 10},
            now=1_000 + rel.STALE_EXEC_SEC + rel.alert_every_sec(),
        )
        self.assertIsNotNone(again)
        # A move resets the window.
        self.assertIsNone(rel.note_exec_head({"execution_number": 11}, now=9_000))
        self.assertIsNone(rel.note_exec_head({"execution_number": 11}, now=9_000 + 10))

    def test_unminted_deposit_alert_and_spent_skip(self):
        now = 1_000_000
        old = now - rel.UNMINTED_DEPOSIT_SEC
        rows = [
            {"txid": "aa" * 32, "timestamp": old, "block": 1},
            {"txid": "bb" * 32, "timestamp": old, "block": 2},
            {"txid": "cc" * 32, "timestamp": now - 10, "block": 3},
        ]
        spent = {"bb" * 32}
        lines = rel.unminted_deposit_alerts(rows, spent, now)
        self.assertEqual(len(lines), 1)
        self.assertIn("aa" * 32, lines[0])
        self.assertTrue(lines[0].startswith("ALERT unminted deposit"))
        st = {"rails": [{"asset": "ETH", "spent_deposits": ["0x" + "bb" * 32]}, {"asset": "BTC", "spent_deposits": ["zz"]}]}
        self.assertEqual(rel.spent_txids(st), {"bb" * 32})

    def test_refuses_btc_submitter_account(self):
        with patch.object(rel, "ACCOUNT", "watcher-browser"):
            with self.assertRaises(SystemExit) as ctx:
                rel.assert_dedicated_account()
        self.assertIn("watcher-browser", str(ctx.exception))
        self.assertEqual(ctx.exception.code, rel.ACCOUNT_REFUSAL_EXIT)
        self.assertEqual(rel.BTC_WATCHER_ACCOUNT, "watcher-browser")
        for name in ("Watcher-Browser", " watcher-browser ", "WATCHER-BROWSER"):
            with patch.object(rel, "ACCOUNT", name):
                with self.assertRaises(SystemExit):
                    rel.assert_dedicated_account()

    def test_daemon_refuses_mint_account(self):
        for name in ("watcher-eth", "Watcher-Eth", " watcher-eth "):
            with patch.object(rel, "ACCOUNT", name):
                with self.assertRaises(SystemExit) as ctx:
                    rel.assert_dedicated_account()
            msg = str(ctx.exception)
            self.assertIn("watcher-eth-lc", msg)
            self.assertIn("account.conf", msg)
            self.assertEqual(ctx.exception.code, 78)

    def test_mint_defaults_to_watcher_eth_and_refuses_relayer(self):
        env = {k: v for k, v in os.environ.items() if k != "FPL_WATCHER_ACCOUNT"}
        with patch.dict(os.environ, env, clear=True):
            self.assertEqual(rel.resolve_account("mint"), "watcher-eth")
            self.assertEqual(rel.resolve_account("daemon"), "watcher-eth-lc")
            self.assertEqual(rel.resolve_account("update"), "watcher-eth-lc")
        with patch.dict(os.environ, {"FPL_WATCHER_ACCOUNT": "watcher-eth"}):
            self.assertEqual(rel.resolve_account("mint"), "watcher-eth")
        for name in ("watcher-eth-lc", " Watcher-Eth-LC "):
            with patch.object(rel, "ACCOUNT", name):
                with self.assertRaises(SystemExit) as ctx:
                    rel.assert_mint_account()
            self.assertIn("watcher-eth", str(ctx.exception))

    def test_mint_refuses_btc_submitter(self):
        for name in ("watcher-browser", " Watcher-Browser "):
            with patch.object(rel, "ACCOUNT", name):
                with self.assertRaises(SystemExit) as ctx:
                    rel.assert_mint_account()
            self.assertIn("watcher-browser", str(ctx.exception))
        with patch.object(rel, "ACCOUNT", "watcher-eth"):
            rel.assert_mint_account()

    def test_default_account_is_dedicated(self):
        self.assertEqual(rel.ACCOUNT, "watcher-eth-lc")
        self.assertNotEqual(rel.ACCOUNT, rel.BTC_WATCHER_ACCOUNT)

    def test_deposit_scan_covers_v1_and_v2(self):
        addrs = rel.deposit_bridges()
        self.assertIn(rel.QC_BRIDGE_V1.lower(), addrs)
        self.assertIn(rel.QC_BRIDGE_V2.lower(), addrs)
        self.assertEqual(
            rel.QC_BRIDGE_V1.lower(), "0xf8f1471643792eb1cd5d0c31061629777e55bc48"
        )
        self.assertEqual(
            rel.QC_BRIDGE_V2.lower(), "0x811854827627024b38926ea9dcc0f88acd5fb23e"
        )
        seen = []

        def eth_rpc(method, params):
            if method == "eth_blockNumber":
                return "0x10"
            if method == "eth_getLogs":
                seen.append(params[0]["address"])
                return []
            raise AssertionError(method)

        rel._last_deposit_scan = 0.0
        with patch.object(rel, "eth_rpc", eth_rpc):
            rel.scan_unminted_deposits({}, now=5_000)
        self.assertEqual(seen, addrs)

    def _scan_env(self):
        env = {
            k: v
            for k, v in os.environ.items()
            if k not in ("ETH_DEPOSIT_SCAN_BLOCKS", "ETH_LOG_CHUNK", "ETH_UNMINTED_DEPOSIT_SEC", "ETH_ALERT_EVERY_SEC")
        }
        return patch.dict(os.environ, env, clear=True)

    def test_default_window_keeps_a_pre_prune_unminted_deposit(self):
        """Older than the threshold, younger than 7 days, inside the default window.

        The old 7200-block default dropped this deposit. Alerts repeat once
        ETH_ALERT_EVERY_SEC elapses, and not before.
        """
        now = 2_000_000
        head = 200_000
        # Six days back: outside 7200 blocks, inside the 50_400 prune window,
        # and younger than 7 days.
        dep_block = head - (6 * 86400 // rel.BLOCK_SEC)
        age = rel.UNMINTED_DEPOSIT_SEC + 3600
        self.assertLess(age, 7 * 86400)
        self.assertGreater(head - dep_block, 7200)
        self.assertLess(head - dep_block, rel.EXEC_PRUNE_BLOCKS)
        txid = "ab" * 32
        v2 = rel.QC_BRIDGE_V2.lower()

        def eth_rpc(method, params):
            if method == "eth_blockNumber":
                return hex(head)
            if method == "eth_getLogs":
                frm = int(params[0]["fromBlock"], 16)
                to = int(params[0]["toBlock"], 16)
                self.assertLessEqual(to - frm + 1, 2000)
                if params[0]["address"] == v2 and frm <= dep_block <= to:
                    return [{"transactionHash": "0x" + txid, "blockNumber": hex(dep_block)}]
                return []
            if method == "eth_getBlockByNumber":
                n = int(params[0], 16)
                self.assertEqual(n, dep_block)
                return {"timestamp": hex(now - age)}
            raise AssertionError(method)

        rel._last_deposit_scan = 0.0
        with self._scan_env(), patch.object(rel, "eth_rpc", eth_rpc):
            self.assertGreaterEqual(rel.deposit_scan_window_blocks(), rel.EXEC_PRUNE_BLOCKS)
            self.assertGreaterEqual(rel.deposit_scan_window_blocks(), head - dep_block)
            first = rel.scan_unminted_deposits({}, now=now)
        self.assertEqual(len(first), 1)
        self.assertIn(txid, first[0])
        self.assertTrue(first[0].startswith("ALERT unminted deposit"))
        with self._scan_env(), patch.object(rel, "eth_rpc", eth_rpc):
            self.assertEqual(rel.scan_unminted_deposits({}, now=now + 1), [])
        with self._scan_env(), patch.object(rel, "eth_rpc", eth_rpc):
            again = rel.scan_unminted_deposits({}, now=now + rel.alert_every_sec())
        self.assertEqual(len(again), 1)
        self.assertIn(txid, again[0])

    def test_block_number_and_one_log_failure_skip_only_that_call(self):
        now = 3_000_000
        head = 80_000
        good_block = head - 100
        bad_block = head - 50
        good = "cd" * 32
        v1 = rel.QC_BRIDGE_V1.lower()
        v2 = rel.QC_BRIDGE_V2.lower()

        def eth_rpc(method, params):
            if method == "eth_blockNumber":
                raise RuntimeError("head rpc down")
            raise AssertionError(method)

        rel._last_deposit_scan = 0.0
        head_calls = {"n": 0}

        def eth_rpc_count(method, params):
            if method == "eth_blockNumber":
                head_calls["n"] += 1
                raise RuntimeError("head rpc down")
            raise AssertionError(method)

        with self._scan_env(), patch.object(rel, "eth_rpc", eth_rpc_count), redirect_stdout(io.StringIO()):
            self.assertEqual(rel.scan_unminted_deposits({}, now=now), [])
            # A failed head read must not arm the 600s throttle.
            self.assertEqual(rel._last_deposit_scan, 0.0)
            self.assertEqual(rel.scan_unminted_deposits({}, now=now + 1), [])
        self.assertEqual(head_calls["n"], 2)

        v1_calls = {"n": 0}

        def eth_rpc_logs(method, params):
            if method == "eth_blockNumber":
                return hex(head)
            if method == "eth_getLogs":
                addr = params[0]["address"]
                frm = int(params[0]["fromBlock"], 16)
                to = int(params[0]["toBlock"], 16)
                if addr == v1:
                    v1_calls["n"] += 1
                    raise RuntimeError("v1 bridge rpc down")
                rows = []
                if addr == v2 and frm <= good_block <= to:
                    rows.append({"transactionHash": "0x" + good, "blockNumber": hex(good_block)})
                if addr == v2 and frm <= bad_block <= to:
                    rows.append({"transactionHash": "0x" + "ef" * 32, "blockNumber": hex(bad_block)})
                return rows
            if method == "eth_getBlockByNumber":
                n = int(params[0], 16)
                if n == bad_block:
                    raise RuntimeError("block timestamp rpc down")
                if n == good_block:
                    return {"timestamp": hex(now - rel.UNMINTED_DEPOSIT_SEC - 5)}
                raise AssertionError(n)
            raise AssertionError(method)

        rel._last_deposit_scan = 0.0
        with self._scan_env(), patch.object(rel, "eth_rpc", eth_rpc_logs), redirect_stdout(io.StringIO()) as buf:
            lines = rel.scan_unminted_deposits({}, now=now + 10_000)
        self.assertEqual(len(lines), 1)
        self.assertIn(good, lines[0])
        self.assertNotIn("ef" * 32, lines[0])
        text_out = buf.getvalue()
        self.assertIn("v1 bridge rpc down", text_out)
        self.assertIn("block timestamp rpc down", text_out)
        # One getLogs failure skips the rest of that bridge, not later chunks.
        self.assertEqual(v1_calls["n"], 1)

    def test_non_v1_v2_bridge_is_not_scanned(self):
        third = "0x" + "ab" * 20
        rel._skipped_bridges.clear()
        with patch.object(rel, "BRIDGE", third), redirect_stdout(io.StringIO()) as buf:
            addrs = rel.deposit_bridges()
        self.assertEqual(addrs, [rel.QC_BRIDGE_V1.lower(), rel.QC_BRIDGE_V2.lower()])
        self.assertNotIn(third, addrs)
        self.assertIn("#29", buf.getvalue())

    def test_hash_only_walk_capped_at_prune_window(self):
        end_n = 100_000
        end = _sealed(end_n, "22" * 32)
        parent = end["parentHash"].removeprefix("0x")
        fetched = []

        def eth_rpc(method, params):
            if method == "eth_getBlockByHash":
                return end
            raise AssertionError(method)

        def fetch_header(n):
            fetched.append(n)
            return {
                "number": n,
                "hash": parent if n == end_n - 1 else "33" * 32,
                "parent_hash": "44" * 32,
                "receipts_root": "55" * 32,
                "rlp_hex": "00",
            }

        tip = end_n - rel.EXEC_PRUNE_BLOCKS - 100
        with patch.object(rel, "eth_rpc", eth_rpc), patch.object(rel, "fetch_header", fetch_header), redirect_stdout(
            io.StringIO()
        ):
            headers, got_end = rel.prepare_hash_only_headers({"eth_exec_tip": tip}, end["hash"])
        self.assertEqual(got_end, end_n)
        floor = end_n - rel.EXEC_PRUNE_BLOCKS
        self.assertEqual(headers[0]["number"], floor)
        self.assertEqual(headers[-1]["number"], end_n)
        self.assertEqual(len(headers), rel.EXEC_PRUNE_BLOCKS + 1)
        self.assertEqual(fetched[0], floor)
        self.assertTrue(all(n >= floor for n in fetched))
        self.assertNotIn(floor - 1, fetched)
        # tip == floor-1: that header is the connects_down link and must be fetched.
        fetched.clear()
        with patch.object(rel, "eth_rpc", eth_rpc), patch.object(rel, "fetch_header", fetch_header), redirect_stdout(
            io.StringIO()
        ):
            headers, got_end = rel.prepare_hash_only_headers({"eth_exec_tip": floor - 1}, end["hash"])
        self.assertEqual(got_end, end_n)
        self.assertEqual(headers[0]["number"], floor)
        self.assertEqual(len(headers), rel.EXEC_PRUNE_BLOCKS + 1)
        self.assertEqual(min(fetched), floor)
        self.assertNotIn(floor - 1, fetched)
        self.assertTrue(all(n >= floor for n in fetched))


class AutoMintAccount(unittest.TestCase):
    def test_default_is_watcher_eth_and_refuses_shared_accounts(self):
        env = {k: v for k, v in os.environ.items() if k != "FPL_WATCHER_ACCOUNT"}
        spec = importlib.util.spec_from_file_location("auto_mint_default", _HERE / "pl-eth-auto-mint.py")
        mod = importlib.util.module_from_spec(spec)
        with patch.dict(os.environ, env, clear=True):
            spec.loader.exec_module(mod)
        self.assertEqual(mod.WATCHER, "watcher-eth")
        self.assertEqual(mod.RELAYER_ACCOUNT, "watcher-eth-lc")
        for bad in ("watcher-browser", "Watcher-Browser", " watcher-eth-lc ", "WATCHER-ETH-LC"):
            with patch.object(mod, "WATCHER", bad):
                with self.assertRaises(SystemExit) as ctx:
                    mod.assert_mint_account()
            self.assertEqual(ctx.exception.code, 78)
        with patch.object(mod, "WATCHER", "watcher-eth"):
            mod.assert_mint_account()


class ScanLimits(unittest.TestCase):
    def setUp(self):
        rel._scan_resume["addr"] = ""
        rel._scan_resume["from_block"] = -1
        rel._block_ts_cache.clear()
        rel._block_ts_failed.clear()
        rel._last_deposit_scan = 0.0

    def test_log_chunk_clamped(self):
        cases = {
            "2000": 2000,
            "1": 1,
            "2001": 2000,
            "0": 1,
            "-5": 1,
            "nan": 2000,
            "inf": 2000,
            "-inf": 2000,
            "nope": 2000,
            "": 2000,
        }
        for raw, want in cases.items():
            with patch.dict(os.environ, {"ETH_LOG_CHUNK": raw}), redirect_stdout(io.StringIO()):
                self.assertEqual(rel._log_chunk_blocks(), want, raw)

    def test_scan_budget_stops_and_warns(self):
        clock = {"t": 1_000.0}
        calls = {"logs": 0}

        def mono():
            return clock["t"]

        def eth_rpc(method, params):
            if method == "eth_blockNumber":
                return hex(10_000)
            if method == "eth_getLogs":
                calls["logs"] += 1
                clock["t"] += 10_000
                return []
            raise AssertionError(method)

        rel._last_deposit_scan = 0.0
        env = {k: v for k, v in os.environ.items() if k not in ("ETH_DEPOSIT_SCAN_BLOCKS", "ETH_SCAN_BUDGET_SEC", "ETH_LOG_CHUNK")}
        env["ETH_SCAN_BUDGET_SEC"] = "120"
        with patch.dict(os.environ, env, clear=True), patch.object(rel, "eth_rpc", eth_rpc), patch.object(
            rel.time, "monotonic", mono
        ), redirect_stdout(io.StringIO()) as buf:
            self.assertEqual(rel.scan_unminted_deposits({}, now=50_000), [])
        self.assertEqual(calls["logs"], 1)
        self.assertIn("ETH_SCAN_BUDGET_SEC", buf.getvalue())
        self.assertIn("resuming next cycle", buf.getvalue())

    def test_failed_head_does_not_throttle(self):
        rel._last_deposit_scan = 0.0

        def eth_rpc(method, params):
            if method == "eth_blockNumber":
                raise RuntimeError("down")
            raise AssertionError(method)

        with patch.object(rel, "eth_rpc", eth_rpc), redirect_stdout(io.StringIO()):
            rel.scan_unminted_deposits({}, now=10)
            rel.scan_unminted_deposits({}, now=11)
        self.assertEqual(rel._last_deposit_scan, 0.0)

    def test_tiny_budget_covers_both_bridges_across_cycles(self):
        rel._scan_resume["addr"] = ""
        rel._scan_resume["from_block"] = -1
        head = 5_000
        seen = []
        clock = {"t": 0.0}

        def mono():
            return clock["t"]

        def eth_rpc(method, params):
            if method == "eth_blockNumber":
                return hex(head)
            if method == "eth_getLogs":
                seen.append((params[0]["address"], int(params[0]["fromBlock"], 16)))
                clock["t"] += 10_000
                return []
            raise AssertionError(method)

        env = {
            k: v
            for k, v in os.environ.items()
            if k not in ("ETH_DEPOSIT_SCAN_BLOCKS", "ETH_LOG_CHUNK", "ETH_SCAN_BUDGET_SEC", "ETH_ALERT_EVERY_SEC")
        }
        env["ETH_LOG_CHUNK"] = "2000"
        env["ETH_SCAN_BUDGET_SEC"] = "120"
        v1 = rel.QC_BRIDGE_V1.lower()
        v2 = rel.QC_BRIDGE_V2.lower()
        expect = [(b, frm) for b in (v1, v2) for frm in (0, 2000, 4000)]
        with patch.dict(os.environ, env, clear=True), patch.object(rel, "eth_rpc", eth_rpc), patch.object(
            rel.time, "monotonic", mono
        ), redirect_stdout(io.StringIO()):
            for i in range(len(expect)):
                clock["t"] = 0.0
                rel._last_deposit_scan = 0.0
                rel.scan_unminted_deposits({}, now=1_000_000 + i)
        self.assertEqual(seen, expect)

    def _scan_env(self, budget: str):
        env = {
            k: v
            for k, v in os.environ.items()
            if k
            not in (
                "ETH_DEPOSIT_SCAN_BLOCKS",
                "ETH_LOG_CHUNK",
                "ETH_SCAN_BUDGET_SEC",
                "ETH_ALERT_EVERY_SEC",
                "ETH_UNMINTED_DEPOSIT_SEC",
            )
        }
        env["ETH_SCAN_BUDGET_SEC"] = budget
        return patch.dict(os.environ, env, clear=True)

    def test_slow_rpc_aged_v2_deposit_alerts_within_one_rotation(self):
        """FM blocker. 5 s per RPC call, one 4-day-old unminted V2 deposit.

        Before the fix, getLogs spent the whole budget and the timestamp pass
        dropped every log: all 52 spans were visited and no ALERT ever fired.
        """
        head = 200_000
        dep_block = head - 4 * 86400 // rel.BLOCK_SEC
        txid = "5a" * 32
        v2 = rel.QC_BRIDGE_V2.lower()
        per_call = 5.0
        for budget, per_cycle in (("10", 2), ("120", 18)):
            with self.subTest(budget=budget):
                rel._scan_resume["addr"] = ""
                rel._scan_resume["from_block"] = -1
                rel._block_ts_cache.clear()
                rel._block_ts_failed.clear()
                clock = {"t": 0.0}
                visited = []

                def eth_rpc(method, params):
                    clock["t"] += per_call
                    if method == "eth_blockNumber":
                        return hex(head)
                    if method == "eth_getLogs":
                        q = params[0]
                        frm, to = int(q["fromBlock"], 16), int(q["toBlock"], 16)
                        visited.append((q["address"], frm))
                        if q["address"] == v2 and frm <= dep_block <= to:
                            return [{"transactionHash": "0x" + txid, "blockNumber": hex(dep_block)}]
                        return []
                    if method == "eth_getBlockByNumber":
                        self.assertEqual(int(params[0], 16), dep_block)
                        return {"timestamp": hex(1_000_000 - 4 * 86400)}
                    raise AssertionError(method)

                alerts_at = []
                with self._scan_env(budget), patch.object(rel, "eth_rpc", eth_rpc), patch.object(
                    rel.time, "monotonic", lambda: clock["t"]
                ), redirect_stdout(io.StringIO()):
                    n_spans = 2 * len(list(rel._block_chunks(head - rel.deposit_scan_window_blocks(), head, 2000)))
                    self.assertEqual(n_spans, 52)
                    rotation = -(-n_spans // per_cycle)
                    for cycle in range(50):
                        rel._last_deposit_scan = 0.0
                        before = len(visited)
                        lines = rel.scan_unminted_deposits({"rails": []}, now=1_000_000 + cycle)
                        if cycle == 0:
                            self.assertEqual(len(visited) - before, per_cycle)
                        if any(txid in ln for ln in lines):
                            alerts_at.append(cycle)
                self.assertTrue(alerts_at, budget)
                self.assertLess(alerts_at[0], rotation, (budget, alerts_at[0], rotation))
                # Keeps alerting on later rotations, not just once.
                self.assertGreaterEqual(len(alerts_at), 50 // rotation)
                self.assertEqual(len(set(visited)), n_spans)

    def test_failing_block_lookup_does_not_pin_the_cursor(self):
        """A block whose timestamp lookup keeps failing on a slow RPC is not retried every cycle."""
        head = 200_000
        start = head - rel.deposit_scan_window_blocks()
        bad = start + 10
        good = start + 20
        v1 = rel.QC_BRIDGE_V1.lower()
        clock = {"t": 0.0}
        tries = {"bad": 0}

        def eth_rpc(method, params):
            clock["t"] += 5.0
            if method == "eth_blockNumber":
                return hex(head)
            if method == "eth_getLogs":
                q = params[0]
                frm, to = int(q["fromBlock"], 16), int(q["toBlock"], 16)
                if q["address"] == v1 and frm <= bad <= to:
                    return [
                        {"transactionHash": "0x" + "01" * 32, "blockNumber": hex(bad)},
                        {"transactionHash": "0x" + "02" * 32, "blockNumber": hex(good)},
                    ]
                return []
            if method == "eth_getBlockByNumber":
                if int(params[0], 16) == bad:
                    tries["bad"] += 1
                    raise RuntimeError("timestamp rpc down")
                return {"timestamp": hex(1_000_000 - 2 * 86400)}
            raise AssertionError(method)

        hits = []
        with self._scan_env("10"), patch.object(rel, "eth_rpc", eth_rpc), patch.object(
            rel.time, "monotonic", lambda: clock["t"]
        ), redirect_stdout(io.StringIO()):
            for cycle in range(4):
                rel._last_deposit_scan = 0.0
                lines = rel.scan_unminted_deposits({}, now=1_000_000 + cycle)
                hits += [ln for ln in lines if "02" * 32 in ln]
        self.assertEqual(tries["bad"], 1)
        self.assertTrue(hits)
        # The cursor moved on past the first V1 span.
        self.assertGreater(rel._scan_resume["from_block"], start)

    def test_failed_bridge_does_not_eat_the_next_bridges_turn(self):
        """Copilot r4225239553: a V1 getLogs that fails after eating the budget moves the cursor to V2."""
        head = 200_000
        v1 = rel.QC_BRIDGE_V1.lower()
        v2 = rel.QC_BRIDGE_V2.lower()
        clock = {"t": 0.0}
        seen = []

        def eth_rpc(method, params):
            if method == "eth_blockNumber":
                return hex(head)
            if method == "eth_getLogs":
                q = params[0]
                seen.append((q["address"], int(q["fromBlock"], 16)))
                clock["t"] += 60.0
                if q["address"] == v1:
                    raise RuntimeError("v1 timeout")
                return []
            raise AssertionError(method)

        with self._scan_env("10"), patch.object(rel, "eth_rpc", eth_rpc), patch.object(
            rel.time, "monotonic", lambda: clock["t"]
        ), redirect_stdout(io.StringIO()):
            rel.scan_unminted_deposits({}, now=1_000_000)
            self.assertEqual(rel._scan_resume["addr"], v2)
            rel._last_deposit_scan = 0.0
            rel.scan_unminted_deposits({}, now=1_000_001)
        self.assertEqual([a for a, _f in seen], [v1, v2])

    def test_timestamp_cap_rewinds_and_finishes_next_cycle(self):
        """More than 64 distinct blocks: rewind to that span, finish next cycle, no re-lookups."""
        head = 100_000
        v1 = rel.QC_BRIDGE_V1.lower()
        now = 5_000_000
        lookups = []
        start = head - rel.EXEC_PRUNE_BLOCKS
        aged = {start + k: ("%064x" % (k + 1)) for k in range(100)}
        minted = "ee" * 32

        def eth_rpc(method, params):
            if method == "eth_blockNumber":
                return hex(head)
            if method == "eth_getLogs":
                q = params[0]
                frm, to = int(q["fromBlock"], 16), int(q["toBlock"], 16)
                if q["address"] != v1:
                    return []
                rows = [
                    {"transactionHash": "0x" + tx, "blockNumber": hex(bn)}
                    for bn, tx in aged.items()
                    if frm <= bn <= to
                ]
                if frm <= start + 500 <= to:
                    rows.append({"transactionHash": "0x" + minted, "blockNumber": hex(start + 500)})
                return rows
            if method == "eth_getBlockByNumber":
                n = int(params[0], 16)
                self.assertNotEqual(n, start + 500, "spent deposit needs no timestamp")
                lookups.append(n)
                return {"timestamp": hex(now - 2 * 86400)}
            raise AssertionError(method)

        st = {"rails": [{"asset": "ETH", "spent_deposits": ["0x" + minted]}]}
        with self._scan_env("120"), patch.object(rel, "eth_rpc", eth_rpc), redirect_stdout(io.StringIO()) as buf:
            first = rel.scan_unminted_deposits(st, now=now)
            self.assertEqual(len(first), rel.SCAN_TS_LOOKUPS_MAX)
            self.assertEqual(rel._scan_resume["addr"], v1)
            self.assertEqual(rel._scan_resume["from_block"], start)
            rel._last_deposit_scan = 0.0
            second = rel.scan_unminted_deposits(st, now=now + 1)
        self.assertEqual(len(second), 100)
        self.assertEqual(sorted(lookups), sorted(aged))
        self.assertEqual(len(lookups), len(set(lookups)))
        self.assertIn("rewinding to", buf.getvalue())
        # Second pass finished: cursor back at the top.
        self.assertEqual(rel._scan_resume["from_block"], start)
        self.assertEqual(rel._scan_resume["addr"], v1)


def _sealed(number: int, parent_hex: str, **extra) -> dict:
    block = dict(_PRE_BLOCK)
    block["number"] = hex(number)
    block["parentHash"] = "0x" + parent_hex
    block.update(extra)
    raw = rel.encode_exec_header(block)
    block["hash"] = "0x" + rel.keccak256(raw).hex()
    return block


class TransientRetryAndBackfillTests(unittest.TestCase):
    """9 Oct 2026: a transient reject must not leave a permanent exec-header hole."""

    def setUp(self):
        rel._seq_floor = 0
        rel._seq_stuck_since = None
        rel._stuck_watch_committed = None

    def _ctl(self, state, calls, fail=None):
        """fail: {seq: [CalledProcessError, ...]} raised in order on that seq's submits."""
        fail = fail or {}

        def ctl(*args, timeout=180):
            calls.append(tuple(args))
            if args[0] == "account":
                return json.dumps({"type": "account", "body": {"sequence": state["n"]}})
            if args[0] == "submit-eth-exec-headers" and "--sequence" in args:
                seq = int(args[args.index("--sequence") + 1])
                q = fail.get(seq)
                if q:
                    raise q.pop(0)
                state["n"] = max(int(state["n"]), seq + 1)
            return "ok tx"

        return ctl

    @staticmethod
    def _seqs(calls):
        return [int(a[a.index("--sequence") + 1]) for a in calls if a and a[0] == "submit-eth-exec-headers"]

    @staticmethod
    def _deferred():
        return subprocess.CalledProcessError(
            1, ["ctl"], output="", stderr="Error: deferred-until-catchup (local 409795 < peer 409796)\n"
        )

    def test_transient_reject_retries_same_sequence_then_continues(self):
        calls, state = [], {"n": 5}
        headers = [{"number": n, "hash": "%02x" % n} for n in (10, 11, 12)]
        sleeps = []
        with patch.object(rel, "ctl", self._ctl(state, calls, {6: [self._deferred()]})), patch.object(
            rel, "HEADER_CHUNK", 1
        ), patch.object(rel, "_sleep", sleeps.append), redirect_stdout(io.StringIO()) as out:
            rel.submit_header_chain(headers)
        # Newest-first chunks at 5, 6, 7; 6 is resubmitted once at the same sequence.
        self.assertEqual(self._seqs(calls), [5, 6, 6, 7])
        self.assertEqual(sleeps, [3.0])
        self.assertIn("transient reject; retry 1/5", out.getvalue())

    def test_non_transient_reject_is_not_retried(self):
        calls, state = [], {"n": 5}
        headers = [{"number": n, "hash": "%02x" % n} for n in (10, 11)]
        err = subprocess.CalledProcessError(1, ["ctl"], output="", stderr="Error: execution parent mismatch at 11")
        with patch.object(rel, "ctl", self._ctl(state, calls, {6: [err]})), patch.object(
            rel, "HEADER_CHUNK", 1
        ), patch.object(rel, "_sleep", lambda s: None), redirect_stdout(io.StringIO()):
            with self.assertRaises(subprocess.CalledProcessError):
                rel.submit_header_chain(headers)
        self.assertEqual(self._seqs(calls), [5, 6])
        self.assertEqual(rel._seq_floor, 6)

    def test_transient_retries_are_bounded(self):
        calls, state = [], {"n": 5}
        headers = [{"number": 10, "hash": "0a"}]
        with patch.dict(os.environ, {"ETH_SUBMIT_RETRIES": "2", "ETH_SUBMIT_RETRY_SEC": "1"}), patch.object(
            rel, "ctl", self._ctl(state, calls, {5: [self._deferred() for _ in range(5)]})
        ), patch.object(rel, "_sleep", lambda s: None), redirect_stdout(io.StringIO()):
            with self.assertRaises(subprocess.CalledProcessError):
                rel.submit_header_chain(headers)
        self.assertEqual(self._seqs(calls), [5, 5, 5])

    def test_is_transient_classifier(self):
        self.assertTrue(rel.is_transient_submit_error(self._deferred()))
        self.assertTrue(
            rel.is_transient_submit_error(subprocess.CalledProcessError(1, ["c"], output="Connection refused (os error 111)"))
        )
        self.assertFalse(rel.is_transient_submit_error(subprocess.CalledProcessError(1, ["c"], output="rejected")))
        self.assertFalse(rel.is_transient_submit_error(RuntimeError("deferred-until-catchup")))

    def test_ctl_keeps_stderr_on_failure(self):
        class R:
            returncode = 1
            stdout = ""
            stderr = "Error: deferred-until-catchup (local 1 < peer 2)\n"

        with patch.object(rel.subprocess, "run", lambda *a, **k: R()), patch.object(
            rel.sys, "stderr", io.StringIO()
        ) as err:
            with self.assertRaises(subprocess.CalledProcessError) as cm:
                rel.ctl("submit-eth-exec-headers")
        self.assertIn("deferred-until-catchup", cm.exception.stderr)
        self.assertIn("deferred-until-catchup", err.getvalue())
        self.assertTrue(rel.is_transient_submit_error(cm.exception))

    def _chain(self, lo, hi):
        out = {}
        for n in range(lo, hi + 1):
            out[n] = {"number": n, "hash": "%064x" % n, "parent_hash": "%064x" % (n - 1)}
        return out

    def _st(self, conn, tip, exec_n, prune):
        return {
            "eth_exec_prune_floor": prune,
            "eth_lc": {"eth_exec_floor": conn, "eth_exec_tip": tip, "execution_number": exec_n},
        }

    def test_backfill_fills_slice_below_connected_floor_newest_first(self):
        chain = self._chain(100, 400)
        submitted = []
        st = self._st(conn=300, tip=400, exec_n=400 + rel.EXEC_PRUNE_BLOCKS - 200, prune=0)
        # target = exec_n - window = 200
        with patch.object(rel, "fetch_header", lambda n: chain[n]), patch.object(
            rel, "submit_header_chain", submitted.append
        ), patch.dict(os.environ, {"ETH_BACKFILL_MAX_HEADERS": "64"}), redirect_stdout(io.StringIO()) as out:
            n = rel.backfill_exec_hole(st)
        self.assertEqual(n, 64)
        self.assertEqual([h["number"] for h in submitted[0]], list(range(236, 300)))
        self.assertIn("backfill exec headers 236..299 (connected floor 300, target 200", out.getvalue())
        # Last slice stops at the target, never below it.
        submitted.clear()
        st = self._st(conn=230, tip=400, exec_n=400 + rel.EXEC_PRUNE_BLOCKS - 200, prune=0)
        with patch.object(rel, "fetch_header", lambda n: chain[n]), patch.object(
            rel, "submit_header_chain", submitted.append
        ), patch.dict(os.environ, {"ETH_BACKFILL_MAX_HEADERS": "64"}), redirect_stdout(io.StringIO()):
            rel.backfill_exec_hole(st)
        self.assertEqual(submitted[0][0]["number"], 200)
        self.assertEqual(submitted[0][-1]["number"], 229)

    def test_backfill_respects_prune_floor_and_noops(self):
        calls = []
        sub = calls.append
        with patch.object(rel, "fetch_header", lambda n: self.fail("no fetch")), patch.object(
            rel, "submit_header_chain", sub
        ):
            # Connected range already reaches the prune line.
            self.assertEqual(rel.backfill_exec_hole(self._st(conn=500, tip=900, exec_n=900, prune=500)), 0)
            # No LC / nothing stored.
            self.assertEqual(rel.backfill_exec_hole({"eth_lc": {}}), 0)
            # Disabled.
            with patch.dict(os.environ, {"ETH_BACKFILL_MAX_HEADERS": "0"}):
                self.assertEqual(rel.backfill_exec_hole(self._st(conn=800, tip=900, exec_n=900, prune=500)), 0)
        self.assertEqual(calls, [])
        # The prune floor wins over a lower window floor.
        self.assertEqual(rel.backfill_target(self._st(conn=1, tip=2, exec_n=60000, prune=11000)), 11000)
        self.assertEqual(rel.backfill_target(self._st(conn=1, tip=2, exec_n=60000, prune=0)), 60000 - rel.EXEC_PRUNE_BLOCKS)

    def test_backfill_refuses_parent_break(self):
        chain = self._chain(100, 200)
        chain[150] = dict(chain[150], parent_hash="ff" * 32)
        with patch.object(rel, "fetch_header", lambda n: chain[n]), patch.object(
            rel, "submit_header_chain", lambda h: self.fail("must not submit")
        ), redirect_stdout(io.StringIO()):
            with self.assertRaises(RuntimeError):
                rel.backfill_exec_hole(self._st(conn=200, tip=300, exec_n=100 + rel.EXEC_PRUNE_BLOCKS, prune=0))


if __name__ == "__main__":
    unittest.main()
