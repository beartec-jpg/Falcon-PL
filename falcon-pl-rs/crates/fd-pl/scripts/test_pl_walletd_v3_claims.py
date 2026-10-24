#!/usr/bin/env python3
"""Offline tests for walletd V2/V3 claim routing (issue #36). No network, no transactions.

  python3 crates/fd-pl/scripts/test_pl_walletd_v3_claims.py

Every cast call goes through a mocked subprocess.run, so each test sees exactly which
bridge address got the fplTip read, the headers[fplTip] read and the openClaim send.

Data:
- V2: the real #33 fixture (fixtures/eth-claim-proof/v2-header-353130.json): FalconQcBridgeV2
  headers(353130) from Sepolia and the ETH/USDC claim leaves from falcon-pl-ctl status.
- V3: FalconQcBridgeV3 is NOT deployed, so every V3 value below is SYNTHETIC and named
  SYNTHETIC_*. The synthetic V3 header's claimRoot is the fixture's real 10-leaf
  lc.claim_root (what a V3 header past 353130 would commit if no new note was packed);
  its height, hash and the V3 address are made up and are not chain data.
"""
from __future__ import annotations

import copy
import importlib.util
import io
import json
import subprocess
import tempfile
import threading
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent


def _load(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, _HERE / file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


cp = _load("claim_proof_v3t", "pl-eth-claim-proof.py")
wd = _load("walletd_v3t", "pl-walletd.py")
# openClaim gas (F4, #63) is tested in test_pl_walletd_openclaim_gas.py; these cast mocks stay send-only.
wd._open_claim_gas = lambda bridge, args: (850_000, 100_000_000)
inst = _load("bridge_install_v3t", "pl-bridge-install-config.py")

FX = json.loads((_HERE / "fixtures/eth-claim-proof/v2-header-353130.json").read_text())
HDR = FX["header"]  # real V2 header 353130
ST = FX["status"]
RAILS = {r["asset"]: r for r in ST["rails"]}
ETH = [bytes.fromhex(h) for h in RAILS["ETH"]["claim_leaves"]]
USDC = [bytes.fromhex(h) for h in RAILS["USDC"]["claim_leaves"]]
ALL = ETH + USDC
V2 = HDR["bridge"]
V2_TIP = HDR["height"]
V2_ROOT = HDR["claimRoot"]
LC_ROOT = "0x" + ST["lc"]["claim_root"]  # real current 10-leaf root

# SYNTHETIC: V3 is not deployed. Address, height and hash are invented for routing tests.
SYNTHETIC_V3 = "0x" + "3a" * 20
SYNTHETIC_V3_TIP = V2_TIP + 1000
SYNTHETIC_V3_HASH = "0x" + "5e" * 32


def _notes() -> list[tuple[str, dict]]:
    out: list = [None] * len(ALL)
    for asset in ("ETH", "USDC"):
        for w in RAILS[asset]["withdrawals"]:
            leaf = cp.withdraw_leaf(
                bytes.fromhex(w["note_id"]), cp.parse_eth(w["external_to"]), int(w["amount"]), asset == "USDC"
            )
            out[ALL.index(leaf)] = (asset, w)
    return out


NOTES = _notes()


def _hdr_out(hash_: str, root: str) -> str:
    return "\n".join([hash_, HDR["parent"], HDR["innerHash"], root, HDR["nextCommitteeRoot"], "true"]) + "\n"


class FakeChain:
    """Mocked cast: per-bridge fplTip and headers. Records (verb, address, sig) for every call."""

    def __init__(self, v3_root: str = LC_ROOT):
        self.calls: list[tuple[str, str, str]] = []
        self.sends: list[list[str]] = []
        self.tips = {V2.lower(): V2_TIP, SYNTHETIC_V3.lower(): SYNTHETIC_V3_TIP}
        self.headers = {
            (V2.lower(), V2_TIP): _hdr_out(HDR["hash"], V2_ROOT),
            (SYNTHETIC_V3.lower(), SYNTHETIC_V3_TIP): _hdr_out(SYNTHETIC_V3_HASH, v3_root),
        }
        # claims(note).dest per (bridge, note); missing = never opened (zero address).
        self.claim_dests: dict[tuple[str, str], str] = {}
        # (bridge, note) whose claims(note).taken is true (take() already paid).
        self.claim_taken: set[tuple[str, str]] = set()
        # Optional per-bridge list of fplTip answers consumed in order (last one repeats).
        self.tip_seq: dict[str, list[int]] = {}

    def run(self, argv, **kw):
        assert argv[0] == wd.CAST, argv
        verb, addr, sig = argv[1], argv[2], argv[3]
        self.calls.append((verb, addr, sig))
        if verb == "send":
            self.sends.append(argv)
            return subprocess.CompletedProcess(argv, 0, "stub: no broadcast", "")
        if sig == "fplTip()(uint64)":
            seq = self.tip_seq.get(addr.lower())
            tip = (seq.pop(0) if len(seq) > 1 else seq[0]) if seq else self.tips[addr.lower()]
            return subprocess.CompletedProcess(argv, 0, f"{tip}\n", "")
        if sig == wd.CLAIMS_SIG:
            key = (addr.lower(), argv[4].lower())
            dest = self.claim_dests.get(key, wd.ZERO_ADDR)
            taken = "true" if key in self.claim_taken else "false"
            is_open = "false" if taken == "true" or dest == wd.ZERO_ADDR else "true"
            # cast prints the uint256 amount with a scientific hint, as on Sepolia.
            out = f"{dest}\n7000 [7e3]\nfalse\n0\n0\n0x{'00' * 32}\n{is_open}\n{taken}\n"
            return subprocess.CompletedProcess(argv, 0, out, "")
        if sig == wd.HEADER_SIG:
            out = self.headers.get((addr.lower(), int(argv[4])))
            if out is None:
                return subprocess.CompletedProcess(argv, 1, "", "unknown header")
            return subprocess.CompletedProcess(argv, 0, out, "")
        raise AssertionError(f"unexpected cast call {argv}")


class RoutingBase(unittest.TestCase):
    """Mocked chain + real proof tool; helpers only (no tests)."""

    def setUp(self):
        self.chain = FakeChain()
        self.status = copy.deepcopy(ST)
        self.proof_calls: list[tuple[int, str]] = []
        self._patches = [
            patch.object(wd, "QC_V2_BRIDGE", V2),
            patch.object(wd, "QC_V2_LIVE", True),
            patch.object(wd, "QC_V3_BRIDGE", SYNTHETIC_V3),
            # SYNTHETIC: V3 anchored at the real frozen V2 tip (FM ruling: V2 stays at 353130).
            patch.object(wd, "QC_V3_ANCHOR_HEIGHT", str(V2_TIP)),
            patch.object(wd, "claim_proof", side_effect=self._claim_proof),
            # The V3 anchor check (AnchorCheck below) is treated as passed here, so the call
            # sequences in these tests are the per-claim ones.
            patch.object(wd, "_ANCHOR_CHECK", {"state": "ok"}),
            patch.object(wd, "_eth_pk", return_value="0xnot-a-key"),
            patch.object(wd.subprocess, "run", side_effect=self.chain.run),
        ]
        for p in self._patches:
            p.start()

    def tearDown(self):
        for p in reversed(self._patches):
            p.stop()

    def _claim_proof(self, account, asset, dest, amount, note="", fpl_height=0, header_claim_root=""):
        self.proof_calls.append((fpl_height, header_claim_root))
        try:
            return cp.build_record(self.status, "", asset, dest, amount, note, fpl_height, header_claim_root)
        except SystemExit as e:
            raise RuntimeError(str(e.code))

    def _open(self, i: int) -> dict:
        asset, w = NOTES[i]
        return wd.eth_open_claim(w["note_id"], w["external_to"], int(w["amount"]), asset == "USDC", "acct")

    def _open_expect_send(self, i: int) -> list[str]:
        self.chain.sends.clear()
        with self.assertRaisesRegex(RuntimeError, "produced no tx hash"):
            self._open(i)
        self.assertEqual(len(self.chain.sends), 1, i)
        return self.chain.sends[0]

    def _addrs(self, start: int = 0) -> set[str]:
        return {a.lower() for _, a, _ in self.chain.calls[start:]}

    def _note(self, i: int) -> str:
        return "0x" + NOTES[i][1]["note_id"].lower().removeprefix("0x")


class Routing(RoutingBase):
    def test_v2_note_stays_on_v2(self):
        for i in range(9):
            self.chain.calls.clear()
            argv = self._open_expect_send(i)
            # tip + header pick and prove on V2; under the lock: V2 tip again, V3 claims(note)
            # (must be empty), V2 claims(note), then the send. Only the V3 claims read leaves V2.
            self.assertEqual(
                [(c[0], c[1].lower(), c[2]) for c in self.chain.calls],
                [
                    ("call", V2.lower(), "fplTip()(uint64)"),
                    ("call", V2.lower(), wd.HEADER_SIG),
                    ("call", V2.lower(), "fplTip()(uint64)"),
                    ("call", SYNTHETIC_V3.lower(), wd.CLAIMS_SIG),
                    ("call", V2.lower(), wd.CLAIMS_SIG),
                    ("send", V2.lower(), "openClaim(bytes32,address,uint256,bool,bytes32[],uint32,uint64)"),
                ],
                i,
            )
            self.assertEqual(argv[2], V2)
            self.assertEqual(argv[10], str(V2_TIP))
            proof = [bytes.fromhex(x[2:]) for x in argv[8].strip("[]").split(",")]
            self.assertTrue(cp.merkle_verify(ALL[i], proof, int(argv[9]), bytes.fromhex(V2_ROOT[2:])))

    def test_note_past_v2_header_goes_to_v3(self):
        argv = self._open_expect_send(9)
        # V2 tip + header read decide the route; then tip, header and send all hit V3.
        self.assertEqual(self.chain.calls[:2], [("call", V2, "fplTip()(uint64)"), ("call", V2, wd.HEADER_SIG)])
        # V3 tip + header, then under the lock V2 claims(note) (must be empty), V3 claims, send.
        self.assertEqual(
            [(c[0], c[1].lower(), c[2]) for c in self.chain.calls[2:]],
            [
                ("call", SYNTHETIC_V3.lower(), "fplTip()(uint64)"),
                ("call", SYNTHETIC_V3.lower(), wd.HEADER_SIG),
                ("call", V2.lower(), wd.CLAIMS_SIG),
                ("call", SYNTHETIC_V3.lower(), wd.CLAIMS_SIG),
                ("send", SYNTHETIC_V3.lower(), "openClaim(bytes32,address,uint256,bool,bytes32[],uint32,uint64)"),
            ],
        )
        self.assertEqual(argv[2], SYNTHETIC_V3)
        self.assertEqual(argv[10], str(SYNTHETIC_V3_TIP))
        self.assertEqual(self.proof_calls[-1], (SYNTHETIC_V3_TIP, LC_ROOT))
        proof = [bytes.fromhex(x[2:]) for x in argv[8].strip("[]").split(",")]
        self.assertEqual(argv[9], "9")
        self.assertTrue(cp.merkle_verify(ALL[9], proof, 9, bytes.fromhex(LC_ROOT[2:])))

    def test_v3_unset_keeps_409_waiting(self):
        with patch.object(wd, "QC_V3_BRIDGE", ""):
            rec = self._open(9)
        self.assertTrue(rec["waiting"])
        self.assertEqual(rec["status"], "waiting_for_bridge_header")
        self.assertEqual(rec["message"], "waiting for bridge header")
        self.assertEqual(rec["bridge"], V2)
        self.assertEqual(rec["bridgeVersion"], "v2")
        self.assertEqual(self.chain.sends, [])
        self.assertEqual(self._addrs(), {V2.lower()})

    def test_v3_header_not_covering_note_waits_on_v3(self):
        # SYNTHETIC V3 header that still commits only the 9 leaves V2 had.
        self.chain = FakeChain(v3_root=V2_ROOT)
        wd.subprocess.run.side_effect = self.chain.run
        rec = self._open(9)
        self.assertEqual((rec["status"], rec["bridgeVersion"], rec["bridge"]), ("waiting_for_bridge_header", "v3", SYNTHETIC_V3))
        self.assertEqual(self.chain.sends, [])

    def test_v2_unset_routes_everything_to_v3(self):
        with patch.object(wd, "QC_V2_BRIDGE", ""), patch.object(wd, "QC_V2_LIVE", False):
            argv = self._open_expect_send(0)
        self.assertEqual(self._addrs(), {SYNTHETIC_V3.lower()})
        self.assertEqual(argv[2], SYNTHETIC_V3)

    def test_neither_bridge_set(self):
        with patch.object(wd, "QC_V2_BRIDGE", ""), patch.object(wd, "QC_V2_LIVE", False), patch.object(wd, "QC_V3_BRIDGE", ""):
            with self.assertRaisesRegex(RuntimeError, "FALCON_QC_V2_BRIDGE and/or FALCON_QC_V3_BRIDGE"):
                self._open(0)
        self.assertEqual(self.chain.calls, [])

    def test_bad_v3_config_refuses_before_any_call(self):
        for bad in ("0x1234", "not-an-address", V2.lower()):
            with patch.object(wd, "QC_V3_BRIDGE", bad):
                with self.assertRaisesRegex(RuntimeError, "FALCON_QC_V3_BRIDGE"):
                    self._open(0)
        self.assertEqual(self.chain.calls, [])

    def test_zero_address_bridge_refuses_before_any_call(self):
        # Copilot #38 (review on 71420aa): the zero address can never host a bridge.
        for name in ("QC_V2_BRIDGE", "QC_V3_BRIDGE"):
            with patch.object(wd, name, wd.ZERO_ADDR):
                with self.assertRaisesRegex(RuntimeError, "FALCON_" + name + " is not a non-zero"):
                    self._open(0)
        self.assertEqual(self.chain.calls, [])

    def test_node_behind_is_retryable_and_not_routed_to_v3(self):
        # Local node at a tip below the bridge header and missing what the header commits.
        self.status["tip_height"] = V2_TIP - 1
        for r in self.status["rails"]:
            r["claim_leaves"] = r["claim_leaves"][:2]
        self.status["lc"]["claim_leaves"] = (
            [h for r in self.status["rails"] if r["asset"] == "ETH" for h in r["claim_leaves"]]
            + [h for r in self.status["rails"] if r["asset"] == "USDC" for h in r["claim_leaves"]]
        )
        rec = self._open(0)
        self.assertEqual(rec["status"], "node_behind_bridge_header")
        self.assertEqual(rec["message"], "local node behind bridge header")
        self.assertTrue(rec["waiting"] and rec["retryable"])
        self.assertEqual(rec["bridgeVersion"], "v2")
        self.assertEqual(self._addrs(), {V2.lower()})
        self.assertEqual(self.chain.sends, [])

    def test_node_behind_with_split_limit_is_retryable(self):
        # Copilot #38: SplitSearchLimit while behind is node_behind too, not a hard 503.
        self.status["tip_height"] = V2_TIP - 1
        self.chain.headers[(V2.lower(), V2_TIP)] = _hdr_out(HDR["hash"], HDR["hash"])  # no prefix match
        with patch.object(cp, "MAX_SPLIT_PAIRS", 10):
            rec = self._open(0)
        self.assertEqual(rec["status"], "node_behind_bridge_header")
        self.assertEqual(self.chain.sends, [])
        self.status["tip_height"] = V2_TIP + 10  # caught up: the limit stays an error
        with patch.object(cp, "MAX_SPLIT_PAIRS", 10):
            with self.assertRaisesRegex(RuntimeError, "not a proven mismatch"):
                self._open(0)

    def test_busy_before_any_rpc(self):
        # Copilot #38: admission is taken before bridge selection, so a busy walletd starts
        # no cast call and no proof subprocess.
        with patch.object(wd, "PROOF_SLOTS", threading.BoundedSemaphore(1)):
            wd.PROOF_SLOTS.acquire()
            try:
                with self.assertRaises(wd.ProofBusy):
                    self._open(9)
            finally:
                wd.PROOF_SLOTS.release()
            self.assertEqual((self.chain.calls, self.proof_calls), ([], []))
            # One slot covers the whole V2 -> V3 request (no second acquire inside).
            self._open_expect_send(9)
            self.assertTrue(wd.PROOF_SLOTS.acquire(blocking=False))
            wd.PROOF_SLOTS.release()

    def test_mismatch_with_node_caught_up_is_still_an_error(self):
        self.status["tip_height"] = V2_TIP + 10
        self.chain.headers[(V2.lower(), V2_TIP)] = _hdr_out(HDR["hash"], HDR["hash"])
        with self.assertRaisesRegex(RuntimeError, "matches no committed set"):
            self._open(0)
        self.assertEqual(self.chain.sends, [])


class DoublePay(RoutingBase):
    """FM must-fix on #38 at 2ed7526: FalconQcBridgeV2 cannot see V3, so walletd must never
    open on V2 a note V3 holds, and never open on V2 once V2 headers pass the V3 anchor.
    SYNTHETIC: the V3 claim state and V2 tip moves below are made up (V3 is not deployed)."""

    def test_v3_opened_v2_tip_advanced_v2_refused(self):
        # Note 0 is committed by V2 353130 and was opened on V3. V2 headers then move past the
        # anchor (they must not, by FM ruling, but walletd must hold even if they do).
        note = self._note(0)
        self.chain.claim_dests[(SYNTHETIC_V3.lower(), note)] = NOTES[0][1]["external_to"].lower()
        self.chain.tips[V2.lower()] = V2_TIP + 5
        rec = self._open(0)
        self.assertEqual(self.chain.sends, [])
        self.assertNotIn(("send", V2), [(c[0], c[1]) for c in self.chain.calls])
        self.assertTrue(rec["ok"] and rec["alreadyOpen"])
        self.assertEqual((rec["bridgeVersion"], rec["bridge"], rec["tx"]), ("v3", SYNTHETIC_V3, ""))

    def test_v3_opened_v2_at_anchor_v2_refused(self):
        # V2 still at the anchor, so routing picks V2; the V3 claims(note) read refuses it.
        self.chain.claim_dests[(SYNTHETIC_V3.lower(), self._note(0))] = NOTES[0][1]["external_to"].lower()
        with self.assertRaisesRegex(RuntimeError, "already opened on V3"):
            self._open(0)
        self.assertEqual(self.chain.sends, [])

    def test_v2_refused_when_fpltip_above_v3_anchor(self):
        # Nothing opened anywhere; V2 tip is past the anchor: the note goes to V3, not V2.
        self.chain.tips[V2.lower()] = V2_TIP + 1
        argv = self._open_expect_send(0)
        self.assertEqual(argv[2], SYNTHETIC_V3)
        self.assertEqual(argv[10], str(SYNTHETIC_V3_TIP))
        # The V2 header is never read once its tip is past the anchor.
        self.assertNotIn(("call", V2, wd.HEADER_SIG), self.chain.calls)

    def test_v2_tip_moves_past_anchor_before_send(self):
        # Picked V2 at the anchor; V2 tip read under the lock is past it: no send.
        self.chain.tip_seq[V2.lower()] = [V2_TIP, V2_TIP + 1]
        with self.assertRaisesRegex(RuntimeError, "past the V3 anchor"):
            self._open(0)
        self.assertEqual(self.chain.sends, [])

    def test_missing_anchor_with_v3_set_refuses_before_any_call(self):
        for bad in ("", "0", "-1", "abc", "1.5"):
            with patch.object(wd, "QC_V3_ANCHOR_HEIGHT", bad):
                with self.assertRaisesRegex(RuntimeError, "FALCON_QC_V3_ANCHOR_HEIGHT"):
                    self._open(0)
        self.assertEqual(self.chain.calls, [])

    def test_anchor_not_needed_without_v3(self):
        with patch.object(wd, "QC_V3_BRIDGE", ""), patch.object(wd, "QC_V3_ANCHOR_HEIGHT", ""):
            argv = self._open_expect_send(0)
        self.assertEqual(argv[2], V2)
        self.assertEqual(self._addrs(), {V2.lower()})

    def test_already_open_on_same_bridge_is_not_resent(self):
        # Two racing requests: the second finds claims(note) set under the lock and answers
        # ok/alreadyOpen instead of a reverted "exists" send.
        self.chain.claim_dests[(V2.lower(), self._note(0))] = NOTES[0][1]["external_to"].lower()
        rec = self._open(0)
        self.assertEqual(self.chain.sends, [])
        self.assertEqual((rec["ok"], rec["alreadyOpen"], rec["bridgeVersion"], rec["tx"]), (True, True, "v2", ""))

    def test_v3_open_refused_when_v2_holds_note(self):
        self.chain.claim_dests[(V2.lower(), self._note(9))] = NOTES[9][1]["external_to"].lower()
        with self.assertRaisesRegex(RuntimeError, "already opened on V2"):
            self._open(9)
        self.assertEqual(self.chain.sends, [])

    def test_open_guard_runs_under_write_lock(self):
        seen = []
        real = wd._open_guard

        def spy(*a):
            # RLock: owned by this thread iff a non-blocking acquire from another thread fails.
            r = []
            t = threading.Thread(target=lambda: r.append(wd.WRITE_LOCK.acquire(blocking=False)))
            t.start()
            t.join()
            if r[0]:
                wd.WRITE_LOCK.release()
            seen.append(not r[0])
            return real(*a)

        with patch.object(wd, "_open_guard", side_effect=spy):
            self._open_expect_send(0)
        self.assertEqual(seen, [True])

    def test_walletd_ignores_seat_v3_env_name(self):
        src = (_HERE / "pl-walletd.py").read_text()
        self.assertNotIn('os.environ.get("FPL_SEPOLIA_QC_V3_BRIDGE")', src)


# ---- V3 refund notes (F1 from the #40 fork dry run) -------------------------------------
# SYNTHETIC: no V3 deposit has resolved on any live chain. The refund row below has the
# exact shape Node::status_json emits (pinned by the Rust test
# three_nodes_agree_on_one_mint_and_one_refund); deposit ids, sender and amounts are made up.
SYNTHETIC_REFUND_DEP = "02" * 32  # same id as the Rust test's refund_id = [2u8; 32]
SYNTHETIC_MINT_DEP = "01" * 32
SYNTHETIC_SENDER = "ab" * 20  # Rust test SENDER = [0xab; 20]
SYNTHETIC_REFUND_AMT = 7_000
# sha256("refund|" || [2u8; 32]); the Rust test asserts the same constant.
REFUND_NOTE_OF_02 = "a988c126d89667b997adf6da27e96be12d314e3c0f4222bc06970bbade9af593"


def _refund_status() -> tuple[dict, bytes, str]:
    """Real #33 status + one SYNTHETIC ETH refund note appended (as FPL appends it), the
    flat list kept equal to ETH + USDC. Returns (status, refund leaf, new combined root)."""
    st = copy.deepcopy(ST)
    note = cp.v3_refund_note_id(SYNTHETIC_REFUND_DEP)
    leaf = cp.withdraw_leaf(bytes.fromhex(note), bytes.fromhex(SYNTHETIC_SENDER), SYNTHETIC_REFUND_AMT, False)
    rails = {r["asset"]: r for r in st["rails"]}
    rails["ETH"]["claim_leaves"].append(leaf.hex())
    rails["ETH"]["withdrawals"].append({
        "note_id": note, "amount": SYNTHETIC_REFUND_AMT, "external_to": "0x" + SYNTHETIC_SENDER,
        "from": "", "signed_btc_tx": "", "kind": "v3_refund",
        "deposit_id": SYNTHETIC_REFUND_DEP, "eth_block": 50,
    })
    st["lc"]["claim_leaves"] = rails["ETH"]["claim_leaves"] + rails["USDC"]["claim_leaves"]
    st["eth_v3_resolutions"] = [
        {"kind": "mint", "asset": "ETH", "amount": 5_000, "credit_to": "sally", "eth_block": 50,
         "deposit_id": SYNTHETIC_MINT_DEP},
        {"kind": "refund", "asset": "ETH", "amount": SYNTHETIC_REFUND_AMT, "refund_to": SYNTHETIC_SENDER,
         "note_id": note, "eth_block": 50, "deposit_id": SYNTHETIC_REFUND_DEP},
    ]
    combined = [bytes.fromhex(h) for h in st["lc"]["claim_leaves"]]
    return st, leaf, "0x" + cp.merkle_root(combined).hex()


class AlreadyOpenShape(RoutingBase):
    """FM nit 2 on #38 at af367df: alreadyOpen also returns `taken` and `dest` from the target
    bridge's claims(note), so the site (#65) skips take() on a paid note instead of hitting a
    "state" revert. SYNTHETIC: the claim states below are made up (no claim is read from chain)."""

    KEYS = {"ok", "tx", "alreadyOpen", "taken", "dest", "noteId", "fplHeight", "bridge", "bridgeVersion", "mode"}

    def test_open_not_taken(self):
        dest = NOTES[0][1]["external_to"].lower()
        self.chain.claim_dests[(V2.lower(), self._note(0))] = dest
        rec = self._open(0)
        self.assertEqual(self.chain.sends, [])
        self.assertEqual(set(rec), self.KEYS)
        self.assertEqual(
            (rec["ok"], rec["tx"], rec["alreadyOpen"], rec["taken"], rec["dest"], rec["bridgeVersion"], rec["bridge"]),
            (True, "", True, False, dest, "v2", V2),
        )
        self.assertEqual((rec["noteId"], rec["fplHeight"], rec["mode"]), (self._note(0), V2_TIP, "qc-v2-open"))

    def test_open_and_taken_on_v2(self):
        dest = NOTES[0][1]["external_to"].lower()
        self.chain.claim_dests[(V2.lower(), self._note(0))] = dest
        self.chain.claim_taken.add((V2.lower(), self._note(0)))
        rec = self._open(0)
        self.assertEqual(self.chain.sends, [])
        self.assertEqual((rec["alreadyOpen"], rec["taken"], rec["dest"]), (True, True, dest))

    def test_taken_on_v3(self):
        dest = NOTES[9][1]["external_to"].lower()
        self.chain.claim_dests[(SYNTHETIC_V3.lower(), self._note(9))] = dest
        self.chain.claim_taken.add((SYNTHETIC_V3.lower(), self._note(9)))
        rec = self._open(9)
        self.assertEqual(self.chain.sends, [])
        self.assertEqual(
            (rec["alreadyOpen"], rec["taken"], rec["dest"], rec["bridgeVersion"], rec["bridge"], rec["fplHeight"]),
            (True, True, dest, "v3", SYNTHETIC_V3, SYNTHETIC_V3_TIP),
        )

    def test_dest_is_the_on_chain_dest(self):
        # The recorded dest (the only caller take() accepts) is returned, not the request's.
        other = "0x" + "c4" * 20
        self.chain.claim_dests[(V2.lower(), self._note(0))] = other
        rec = self._open(0)
        self.assertEqual((rec["dest"], rec["taken"]), (other, False))

    def test_fresh_open_has_no_taken_or_dest(self):
        # A sent openClaim answers with its tx; taken/dest only come with alreadyOpen.
        real = self.chain.run
        txh = "0x" + "9d" * 32  # SYNTHETIC tx hash from the stubbed send

        def sent(argv, **kw):
            p = real(argv, **kw)
            return subprocess.CompletedProcess(argv, 0, f"transactionHash {txh}\n", "") if argv[1] == "send" else p

        wd.subprocess.run.side_effect = sent
        rec = self._open(0)
        self.assertEqual(len(self.chain.sends), 1)
        self.assertEqual(rec["tx"], txh)
        self.assertFalse({"alreadyOpen", "taken", "dest"} & set(rec))

    def test_malformed_claims_output_refuses(self):
        real = self.chain.run

        def two_bools(argv, **kw):
            if argv[1] == "call" and argv[3] == wd.CLAIMS_SIG:
                self.chain.calls.append(("call", argv[2], argv[3]))
                return subprocess.CompletedProcess(argv, 0, f"{wd.ZERO_ADDR}\n0\nfalse\nfalse\n", "")
            return real(argv, **kw)

        wd.subprocess.run.side_effect = two_bools
        with self.assertRaisesRegex(RuntimeError, "unexpected claims"):
            self._open(0)
        self.assertEqual(self.chain.sends, [])

    def test_claim_state_parser(self):
        out = f"{'0x' + 'AB' * 20}\n1000000000000000000 [1e18]\ntrue\n5\n353130\n0x{'11' * 32}\nfalse\ntrue\n"
        with patch.object(wd.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, out, "")):
            self.assertEqual(wd._claim_state(V2, self._note(0)), {"dest": "0x" + "ab" * 20, "taken": True})
            self.assertEqual(wd._claim_dest(V2, self._note(0)), "0x" + "ab" * 20)


class AnchorCheck(RoutingBase):
    """FM nit 4 on #38: V3 headers(anchor).hash must equal V2 headers(anchor).hash before any
    claim opens while both bridges are set. Read-only. SYNTHETIC: the V3 copy of the anchor
    header is the real V2 header 353130 (what the V3 constructor would copy) or a made-up
    mismatch; V3 is not deployed."""

    def setUp(self):
        super().setUp()
        self._ac = patch.object(wd, "_ANCHOR_CHECK", {"state": ""})
        self._ac.start()

    def tearDown(self):
        self._ac.stop()
        super().tearDown()

    def _v3_copy(self, hash_: str = HDR["hash"]):
        self.chain.headers[(SYNTHETIC_V3.lower(), V2_TIP)] = _hdr_out(hash_, V2_ROOT)

    def _anchor_reads(self) -> list:
        return [
            (c[1].lower(), c[2]) for c in self.chain.calls if c[2] == wd.HEADER_SIG
        ]

    def test_match_passes_once_then_cached(self):
        self._v3_copy()
        self._open_expect_send(0)
        self.assertEqual(
            [(c[1].lower(), c[2]) for c in self.chain.calls[:2]],
            [(SYNTHETIC_V3.lower(), wd.HEADER_SIG), (V2.lower(), wd.HEADER_SIG)],
        )
        self.assertEqual(wd._ANCHOR_CHECK["state"], "ok")
        self.chain.calls.clear()
        self._open_expect_send(0)
        # Only the per-claim V2 header read is left.
        self.assertEqual(self._anchor_reads(), [(V2.lower(), wd.HEADER_SIG)])

    def test_mismatch_refuses_and_is_sticky(self):
        self._v3_copy("0x" + "77" * 32)
        with self.assertRaisesRegex(RuntimeError, "mismatch.*not opening claims"):
            self._open(0)
        self.assertEqual(self.chain.sends, [])
        self.assertTrue(wd._ANCHOR_CHECK["state"].startswith("mismatch"))
        self._v3_copy()  # even if the chain answer changes, the config error stands
        self.chain.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "mismatch"):
            self._open(9)
        self.assertEqual((self.chain.calls, self.chain.sends), ([], []))

    def test_zero_hash_is_a_mismatch(self):
        self._v3_copy("0x" + "00" * 32)
        with self.assertRaisesRegex(RuntimeError, "mismatch"):
            self._open(0)
        self.assertEqual(self.chain.sends, [])

    def test_read_error_refuses_then_retries(self):
        # No V3 header at the anchor in the fake chain: cast call fails.
        with self.assertRaisesRegex(RuntimeError, "could not read headers"):
            self._open(0)
        self.assertEqual(self.chain.sends, [])
        self.assertEqual(wd._ANCHOR_CHECK["state"], "")
        self._v3_copy()
        self._open_expect_send(0)
        self.assertEqual(wd._ANCHOR_CHECK["state"], "ok")

    def test_not_run_without_v2(self):
        with patch.object(wd, "QC_V2_BRIDGE", ""), patch.object(wd, "QC_V2_LIVE", False):
            self._open_expect_send(0)
        self.assertNotIn((V2.lower(), wd.HEADER_SIG), self._anchor_reads())
        self.assertEqual(wd._ANCHOR_CHECK["state"], "")

    def test_not_run_without_v3(self):
        with patch.object(wd, "QC_V3_BRIDGE", ""), patch.object(wd, "QC_V3_ANCHOR_HEIGHT", ""):
            self._open_expect_send(0)
        self.assertEqual(self._anchor_reads(), [(V2.lower(), wd.HEADER_SIG)])

    def test_startup_check_logs_and_does_not_raise(self):
        self._v3_copy("0x" + "77" * 32)
        err = io.StringIO()
        with redirect_stderr(err):
            wd._startup_anchor_check()
        self.assertIn("V3 anchor check failed", err.getvalue())
        self.assertEqual(self.chain.sends, [])
        self.assertEqual([c[0] for c in self.chain.calls], ["call", "call"])

    def test_startup_check_ok(self):
        self._v3_copy()
        err = io.StringIO()
        with redirect_stderr(err):
            wd._startup_anchor_check()
        self.assertEqual((err.getvalue(), wd._ANCHOR_CHECK["state"]), ("", "ok"))

    def test_main_runs_startup_check_before_serving(self):
        src = (_HERE / "pl-walletd.py").read_text()
        main = src[src.index('if __name__ == "__main__":'):]
        self.assertLess(main.index("_startup_anchor_check()"), main.index("serve_forever"))


class RefundNoteTool(unittest.TestCase):
    def test_refund_note_id_matches_rust_and_walletd(self):
        self.assertEqual(cp.v3_refund_note_id(SYNTHETIC_REFUND_DEP), REFUND_NOTE_OF_02)
        self.assertEqual(wd.v3_refund_note("0x" + SYNTHETIC_REFUND_DEP), "0x" + REFUND_NOTE_OF_02)
        with self.assertRaises(ValueError):
            cp.v3_refund_note_id("0x1234")

    def test_found_by_note_without_account(self):
        st, _, _ = _refund_status()
        for acct in ("", "acct", "someone-else"):
            w = cp.find_note(st, acct, "ETH", "", 0, REFUND_NOTE_OF_02)
            self.assertEqual(w["kind"], "v3_refund")

    def test_not_matched_without_note(self):
        st, _, _ = _refund_status()
        with self.assertRaisesRegex(SystemExit, "withdraw note not packed yet"):
            cp.find_note(st, "", "ETH", "0x" + SYNTHETIC_SENDER, SYNTHETIC_REFUND_AMT, "")

    def test_proof_against_v3_header(self):
        st, leaf, root = _refund_status()
        rec = cp.build_record(st, "", "ETH", "", 0, REFUND_NOTE_OF_02, SYNTHETIC_V3_TIP, root)
        self.assertEqual((rec["status"], rec["kind"], rec["depositId"]), ("ready", "v3_refund", "0x" + SYNTHETIC_REFUND_DEP))
        self.assertEqual((rec["dest"], rec["amount"], rec["isUsdc"]), ("0x" + SYNTHETIC_SENDER, SYNTHETIC_REFUND_AMT, False))
        self.assertEqual(rec["index"], 5)  # ETH rail position 5, before the USDC leaves
        proof = [bytes.fromhex(x[2:]) for x in rec["proof"]]
        self.assertTrue(cp.merkle_verify(leaf, proof, 5, bytes.fromhex(root[2:])))
        # The real V2 header 353130 does not commit it: waiting, not an error.
        rec = cp.build_record(st, "", "ETH", "", 0, REFUND_NOTE_OF_02, V2_TIP, V2_ROOT)
        self.assertEqual(rec["status"], "waiting_for_bridge_header")

    def test_withdraw_records_unchanged(self):
        st, _, root = _refund_status()
        asset, w = NOTES[0]
        rec = cp.build_record(st, "", asset, w["external_to"], int(w["amount"]), w["note_id"], SYNTHETIC_V3_TIP, root)
        self.assertNotIn("kind", rec)
        self.assertNotIn("depositId", rec)

    def test_reasons(self):
        st, _, _ = _refund_status()
        mint_note = cp.v3_refund_note_id(SYNTHETIC_MINT_DEP)
        with self.assertRaisesRegex(SystemExit, "resolved as a mint to sally; it has no refund note"):
            cp.find_note(st, "", "ETH", "", 0, mint_note)
        with self.assertRaisesRegex(SystemExit, "V3 refund note is on the ETH rail, not USDC"):
            cp.find_note(st, "", "USDC", "", 0, REFUND_NOTE_OF_02)
        with self.assertRaisesRegex(SystemExit, "^withdraw note not packed yet$"):
            cp.find_note(st, "", "ETH", "", 0, cp.v3_refund_note_id("03" * 32))

    def test_cli_deposit_id(self):
        st, _, root = _refund_status()
        for extra in ([], ["--note", REFUND_NOTE_OF_02]):
            argv = ["pl-eth-claim-proof.py", "--deposit-id", "0x" + SYNTHETIC_REFUND_DEP, "--asset", "ETH",
                    "--fpl-height", str(SYNTHETIC_V3_TIP), "--header-claim-root", root, *extra]
            buf = io.StringIO()
            with patch.object(cp, "ctl_status", return_value=copy.deepcopy(st)), patch("sys.argv", argv), patch("sys.stdout", buf):
                self.assertEqual(cp.main(), 0)
            rec = json.loads(buf.getvalue())
            self.assertEqual((rec["status"], rec["noteId"]), ("ready", "0x" + REFUND_NOTE_OF_02))
        for bad, msg in ((["--deposit-id", "0x12"], "--deposit-id"),
                         (["--deposit-id", SYNTHETIC_REFUND_DEP, "--note", "00" * 32], "not the refund note")):
            with patch.object(cp, "ctl_status", side_effect=AssertionError("must not read the node")):
                with patch("sys.argv", ["pl-eth-claim-proof.py", *bad]):
                    with self.assertRaises(SystemExit) as cm:
                        cp.main()
            self.assertIn(msg, str(cm.exception.code))


class RefundNoteWalletd(RoutingBase):
    """eth-open-claim for a refund note named only by depositId: past the V2 header, so V3."""

    def setUp(self):
        super().setUp()
        self.status, self.refund_leaf, self.v3_root = _refund_status()
        self.chain.headers[(SYNTHETIC_V3.lower(), SYNTHETIC_V3_TIP)] = _hdr_out(SYNTHETIC_V3_HASH, self.v3_root)

    def test_refund_by_deposit_id_goes_to_v3(self):
        with self.assertRaisesRegex(RuntimeError, "produced no tx hash"):
            wd.eth_open_claim(wd.v3_refund_note(SYNTHETIC_REFUND_DEP), "", 0, False, "")
        self.assertEqual(len(self.chain.sends), 1)
        argv = self.chain.sends[0]
        self.assertEqual(argv[2], SYNTHETIC_V3)
        # Past the V2 header: everything after the V2 pick is V3, except the V2 claims(note)
        # double-pay read under the lock.
        self.assertEqual(
            {(a.lower(), sig) for _, a, sig in self.chain.calls[2:] if a.lower() == V2.lower()},
            {(V2.lower(), wd.CLAIMS_SIG)},
        )
        # openClaim(noteId, dest = deposit sender, amount, isUsdc, proof, index, fplHeight)
        self.assertEqual(argv[4:8], ["0x" + REFUND_NOTE_OF_02, "0x" + SYNTHETIC_SENDER, str(SYNTHETIC_REFUND_AMT), "false"])
        proof = [bytes.fromhex(x[2:]) for x in argv[8].strip("[]").split(",")]
        self.assertTrue(cp.merkle_verify(self.refund_leaf, proof, int(argv[9]), bytes.fromhex(self.v3_root[2:])))
        self.assertEqual(argv[10], str(SYNTHETIC_V3_TIP))

    def test_refund_waits_without_v3(self):
        with patch.object(wd, "QC_V3_BRIDGE", ""):
            rec = wd.eth_open_claim(wd.v3_refund_note(SYNTHETIC_REFUND_DEP), "", 0, False, "")
        self.assertEqual((rec["status"], rec["kind"]), ("waiting_for_bridge_header", "v3_refund"))
        self.assertEqual(rec["depositId"], "0x" + SYNTHETIC_REFUND_DEP)
        self.assertEqual(self.chain.sends, [])

    def test_withdraw_note_amount_is_never_backfilled(self):
        # Only a v3_refund record fills dest/amount; a withdraw note with amount 0 does not
        # pick up the row's amount (the local leaf check then refuses).
        asset, w = NOTES[0]
        with self.assertRaisesRegex(RuntimeError, "does not verify"):
            wd.eth_open_claim(w["note_id"], w["external_to"], 0, asset == "USDC", "acct")
        self.assertEqual(self.chain.sends, [])

    def test_wrong_dest_refuses(self):
        # The contract pays the deposit sender only; a different dest never matches the note.
        with self.assertRaises(RuntimeError):
            wd.eth_open_claim(wd.v3_refund_note(SYNTHETIC_REFUND_DEP), "0x" + "cd" * 20, 0, False, "")
        self.assertEqual(self.chain.sends, [])


class RefundHttp(unittest.TestCase):
    def _post(self, body: dict, **patches):
        raw = json.dumps(body).encode()
        h = wd.H.__new__(wd.H)
        h.headers = {"Content-Length": str(len(raw))}
        h.rfile = io.BytesIO(raw)
        sent = {}
        h._json = lambda code, obj: sent.update(code=code, obj=obj)
        h.do_POST()
        return sent

    def test_open_claim_by_deposit_id_only(self):
        seen = []
        with patch.object(wd, "eth_open_claim", side_effect=lambda *a: seen.append(a) or {"ok": True}):
            sent = self._post({"action": "eth-open-claim", "depositId": "0x" + SYNTHETIC_REFUND_DEP, "asset": "ETH"})
        self.assertEqual(sent["code"], 200)
        self.assertEqual(seen, [("0x" + REFUND_NOTE_OF_02, "", 0, False, "")])

    def test_claim_proof_by_deposit_id_only(self):
        seen = []
        with patch.object(wd, "claim_proof", side_effect=lambda *a: seen.append(a) or {"ok": True}):
            sent = self._post({"action": "claim-proof", "depositId": SYNTHETIC_REFUND_DEP, "asset": "ETH"})
        self.assertEqual(sent["code"], 200)
        self.assertEqual(seen, [("", "ETH", "", 0, "0x" + REFUND_NOTE_OF_02)])

    def test_bad_deposit_id_or_note_is_400(self):
        for body in ({"depositId": "0x12"}, {"depositId": SYNTHETIC_REFUND_DEP, "noteId": "0x" + "00" * 32}):
            for action in ("eth-open-claim", "claim-proof"):
                with patch.object(wd, "eth_open_claim", side_effect=AssertionError), patch.object(wd, "claim_proof", side_effect=AssertionError):
                    sent = self._post({"action": action, "asset": "ETH", **body})
                self.assertEqual(sent["code"], 400, (action, body))

    def test_withdraw_requests_need_positive_amount(self):
        # Copilot #38: 0 is the refund sentinel only.
        with patch.object(wd, "eth_open_claim", side_effect=AssertionError("must not run")):
            for amt in ("0", "", "-1"):
                sent = self._post({"action": "eth-open-claim", "noteId": "0x" + "00" * 32, "dest": "0x" + "00" * 20,
                                   "amount": amt, "asset": "ETH", "account": "acct"})
                self.assertEqual((sent["code"], sent["obj"]["error"]), (400, "amount must be positive"), amt)

    def test_refund_negative_amount_is_400(self):
        # Copilot #38 (review on 358a4c7): a refund depositId must not let -1 reach the proof tool.
        for action in ("claim-proof", "eth-open-claim"):
            with patch.object(wd, "claim_proof", side_effect=AssertionError("must not run")), \
                    patch.object(wd, "eth_open_claim", side_effect=AssertionError("must not run")):
                sent = self._post({"action": action, "depositId": SYNTHETIC_REFUND_DEP, "asset": "ETH", "amount": "-1"})
            self.assertEqual((sent["code"], sent["obj"]["error"]), (400, "amount must be positive"), action)

    def test_unhashable_action_is_unknown_not_a_crash(self):
        for bad in (["claim-proof"], {"a": 1}):
            sent = self._post({"action": bad})
            self.assertEqual(sent["code"], 400, bad)

    def test_withdraw_requests_still_need_account(self):
        sent = self._post({"action": "eth-open-claim", "noteId": "0x" + "00" * 32, "dest": "0x" + "00" * 20,
                           "amount": "1", "asset": "ETH"})
        self.assertEqual((sent["code"], sent["obj"]["error"]), (400, "account required"))
        sent = self._post({"action": "claim-proof", "asset": "ETH", "dest": "0x" + "00" * 20, "amount": "1"})
        self.assertEqual(sent["code"], 400)


class WhitespaceRoot(unittest.TestCase):
    """FM nit 1 on #33: a whitespace-only --header-claim-root is rejected."""

    def _rec(self, root: str) -> dict:
        asset, w = NOTES[0]
        return cp.build_record(copy.deepcopy(ST), "", asset, w["external_to"], int(w["amount"]), w["note_id"], V2_TIP, root)

    def test_build_record_rejects_blank(self):
        for bad in (" ", "   ", "\t", "\n", " \t "):
            with self.assertRaises(SystemExit) as cm:
                self._rec(bad)
            self.assertIn("is blank", str(cm.exception.code), repr(bad))

    def test_cli_rejects_blank_before_reading_node(self):
        for bad in ("   ", ""):
            with patch.object(cp, "ctl_status", side_effect=AssertionError("must not read the node")):
                with patch("sys.argv", ["pl-eth-claim-proof.py", "--note", "00" * 32, "--fpl-height", "1", "--header-claim-root", bad]):
                    with self.assertRaises(SystemExit) as cm:
                        cp.main()
            self.assertIn("is blank", str(cm.exception.code), repr(bad))

    def test_flag_omitted_keeps_old_path(self):
        asset, w = NOTES[0]
        with patch.object(cp, "ctl_status", return_value=copy.deepcopy(ST)):
            with patch("sys.argv", ["pl-eth-claim-proof.py", "--note", w["note_id"], "--asset", asset]):
                buf = io.StringIO()
                with patch("sys.stdout", buf):
                    self.assertEqual(cp.main(), 0)
        rec = json.loads(buf.getvalue())
        self.assertEqual(rec["claimRoot"], LC_ROOT)
        self.assertNotIn("status", rec)


class LeafSourceLog(unittest.TestCase):
    """RO nit 2 on #33: say when rail_leaf_lists falls back to the flat prefix search."""

    def test_rails_path(self):
        asset, w = NOTES[0]
        err = io.StringIO()
        with redirect_stderr(err):
            rec = cp.build_record(copy.deepcopy(ST), "", asset, w["external_to"], int(w["amount"]), w["note_id"], V2_TIP, V2_ROOT)
        self.assertEqual(rec["leafSource"], "rails")
        self.assertEqual(err.getvalue(), "")

    def test_disagreeing_rails_logged(self):
        st = copy.deepcopy(ST)
        st["rails"][0]["claim_leaves"] = list(reversed(st["rails"][0]["claim_leaves"]))
        err = io.StringIO()
        with redirect_stderr(err):
            self.assertIsNone(cp.rail_leaf_lists(st))
        self.assertIn("prefix-only search (rails ETH 5 + USDC 5 disagree", err.getvalue())
        self.assertNotIn("{", err.getvalue())

    def test_no_rail_leaves_logged(self):
        st = copy.deepcopy(ST)
        for r in st["rails"]:
            r["claim_leaves"] = []
        asset, w = NOTES[3]
        err = io.StringIO()
        with redirect_stderr(err):
            rec = cp.build_record(st, "", asset, w["external_to"], int(w["amount"]), w["note_id"], V2_TIP, V2_ROOT)
        self.assertEqual(rec["leafSource"], "flat-prefix")
        self.assertIn("rails carry no claim_leaves", err.getvalue())


class Concurrency(unittest.TestCase):
    """RO nit 1 on #33: a long proof search must not block other requests."""

    def _handler(self, body: dict):
        raw = json.dumps(body).encode()
        h = wd.H.__new__(wd.H)
        h.headers = {"Content-Length": str(len(raw))}
        h.rfile = io.BytesIO(raw)
        h.sent = {}
        h._json = lambda code, obj: h.sent.update(code=code, obj=obj)
        return h

    def test_server_is_threading(self):
        src = (_HERE / "pl-walletd.py").read_text()
        self.assertIn('BoundedThreadingHTTPServer((WALLETD_BIND, WALLETD_PORT), H).serve_forever()', src)
        self.assertNotIn("    HTTPServer(", src)
        self.assertTrue(issubclass(wd.BoundedThreadingHTTPServer, wd.ThreadingHTTPServer))

    def test_handler_threads_are_bounded(self):
        # Copilot #38: no more than max_threads handler threads; extra connections wait in
        # the backlog. Real loopback server on an ephemeral port, slow handler.
        import socket

        gate = threading.Event()
        live, peak = [0], [0]
        lock = threading.Lock()

        class Slow(wd.H):
            def do_GET(self):
                with lock:
                    live[0] += 1
                    peak[0] = max(peak[0], live[0])
                gate.wait(5)
                with lock:
                    live[0] -= 1
                self._json(200, {"ok": True})

        srv = wd.BoundedThreadingHTTPServer(("127.0.0.1", 0), Slow, max_threads=2)
        t = threading.Thread(target=srv.serve_forever, daemon=True)
        t.start()
        socks = []
        try:
            for _ in range(5):
                c = socket.create_connection(srv.server_address, timeout=5)
                c.sendall(b"GET /x HTTP/1.0\r\n\r\n")
                socks.append(c)
            import time as _t
            _t.sleep(0.5)
            self.assertEqual(peak[0], 2)
            gate.set()
            for c in socks:
                self.assertIn(b"200", c.recv(64))
        finally:
            gate.set()
            for c in socks:
                c.close()
            srv.shutdown()
            srv.server_close()
        self.assertEqual(peak[0], 2)

    def test_read_only_action_runs_while_write_lock_held(self):
        h = self._handler({"action": "claim-proof", "account": "acct", "asset": "ETH",
                           "dest": "0x" + "00" * 20, "amount": "1"})
        with patch.object(wd, "claim_proof", return_value={"ok": True}):
            with wd.WRITE_LOCK:
                t = threading.Thread(target=h.do_POST)
                t.start()
                t.join(2)
                self.assertFalse(t.is_alive())
        self.assertEqual(h.sent["code"], 200)

    def test_write_action_waits_for_lock(self):
        h = self._handler({"action": "name-status", "name": "zz-test"})
        held = threading.Event()
        release = threading.Event()

        def hold():
            with wd.WRITE_LOCK:
                held.set()
                release.wait(5)

        holder = threading.Thread(target=hold)
        holder.start()
        held.wait(2)
        with patch.object(wd, "_names_load", return_value=[]):
            t = threading.Thread(target=h.do_POST)
            t.start()
            t.join(0.3)
            self.assertTrue(t.is_alive())  # blocked behind the other writer
            release.set()
            t.join(2)
        holder.join(2)
        self.assertFalse(t.is_alive())
        self.assertEqual(h.sent["code"], 200)

    def test_proof_slots_bounded(self):
        # Copilot #38: proof subprocesses are capped; past the cap the answer is "busy" at once.
        ran = []
        with patch.object(wd, "PROOF_SLOTS", threading.BoundedSemaphore(1)):
            wd.PROOF_SLOTS.acquire()  # another request holds the only slot
            try:
                with patch.object(wd.subprocess, "run", side_effect=lambda *a, **k: ran.append(a)):
                    with self.assertRaises(wd.ProofBusy):
                        wd.claim_proof("acct", "ETH", "0x" + "00" * 20, 1)
                    h = self._handler({"action": "eth-open-claim", "noteId": "0x" + "00" * 32,
                                       "dest": "0x" + "00" * 20, "amount": "1", "asset": "ETH", "account": "acct"})
                    with patch.object(wd, "_pick_claim_bridge", side_effect=wd.ProofBusy(wd.PROOF_BUSY)):
                        h.do_POST()
                    self.assertEqual((h.sent["code"], h.sent["obj"]["status"]), (409, "proof_busy"))
                    self.assertTrue(h.sent["obj"]["waiting"] and h.sent["obj"]["retryable"])
                    h = self._handler({"action": "claim-proof", "account": "acct", "asset": "ETH",
                                       "dest": "0x" + "00" * 20, "amount": "1"})
                    h.do_POST()
                    self.assertEqual((h.sent["code"], h.sent["obj"]["status"]), (429, "proof_busy"))
            finally:
                wd.PROOF_SLOTS.release()
            self.assertEqual(ran, [])  # no subprocess started while busy
            # Slot is returned after a run, including a failing one.
            fail = subprocess.CompletedProcess([], 1, "", '{"ok":false,"error":"x"}')
            with patch.object(wd.subprocess, "run", return_value=fail):
                for _ in range(3):
                    with self.assertRaisesRegex(RuntimeError, "^x$"):
                        wd.claim_proof("acct", "ETH", "0x" + "00" * 20, 1)

    def test_claim_proof_inside_admission_takes_no_second_slot(self):
        ok = subprocess.CompletedProcess([], 0, '{"ok":true}', "")
        with patch.object(wd, "PROOF_SLOTS", threading.BoundedSemaphore(1)):
            with patch.object(wd.subprocess, "run", return_value=ok):
                with wd._proof_slot():
                    self.assertEqual(wd.claim_proof("acct", "ETH", "0x" + "00" * 20, 1), {"ok": True})
                    self.assertEqual(wd.claim_proof("acct", "ETH", "0x" + "00" * 20, 1), {"ok": True})
            self.assertTrue(wd.PROOF_SLOTS.acquire(blocking=False))  # released on exit
            wd.PROOF_SLOTS.release()

    def test_node_behind_is_http_409(self):
        h = self._handler({"action": "eth-open-claim", "noteId": "0x" + "00" * 32,
                           "dest": "0x" + "00" * 20, "amount": "1", "asset": "ETH", "account": "acct"})
        behind = {"ok": False, "waiting": True, "retryable": True,
                  "status": "node_behind_bridge_header", "message": "local node behind bridge header"}
        with patch.object(wd, "eth_open_claim", return_value=behind):
            h.do_POST()
        self.assertEqual(h.sent["code"], 409)
        self.assertEqual(h.sent["obj"]["status"], "node_behind_bridge_header")


class InstallConfig(unittest.TestCase):
    """QC_V3 entry in falcon-pl-bridge.env: placeholder, unset by default."""

    def _write(self, v3_doc=None) -> str:
        with tempfile.TemporaryDirectory() as d:
            bd = Path(d)
            if v3_doc is not None:
                (bd / "sepolia-qc-v3.json").write_text(json.dumps(v3_doc))
            env = bd / "falcon-pl-bridge.env"
            with patch.object(inst, "BRIDGE_DIR", bd), patch.object(inst, "ENV_OUT", env):
                inst.write_env({"bridge": "0x" + "11" * 20})
            return env.read_text()

    def _env(self, text: str) -> dict:
        out = {}
        for line in text.splitlines():
            if line.strip() and not line.startswith("#"):
                k, _, v = line.partition("=")
                out[k] = v
        return out

    def test_unset_by_default_with_placeholder_label(self):
        text = self._write()
        env = self._env(text)
        self.assertEqual(env["FALCON_QC_V3_BRIDGE"], "")
        self.assertIn("# FalconQcBridgeV3: PLACEHOLDER, unset until V3 is deployed", text)
        # Seat/consensus name is not written by the installer.
        self.assertNotIn("FPL_SEPOLIA_QC_V3_BRIDGE", env)

    def test_reads_deploy_record(self):
        # SYNTHETIC address standing in for a future deploy record.
        env = self._env(self._write({"bridge": SYNTHETIC_V3, "anchor_height": V2_TIP}))
        self.assertEqual(env["FALCON_QC_V3_BRIDGE"], SYNTHETIC_V3)
        self.assertEqual(env["FALCON_QC_V3_ANCHOR_HEIGHT"], str(V2_TIP))

    def test_anchor_unset_by_default(self):
        self.assertEqual(self._env(self._write())["FALCON_QC_V3_ANCHOR_HEIGHT"], "")

    def test_record_without_valid_anchor_stops_install(self):
        # FM must-fix on #38: walletd needs the anchor to keep V2 closed past it.
        for bad in (None, 0, -1, "353130", 1.5, True):
            doc = {"bridge": SYNTHETIC_V3}
            if bad is not None:
                doc["anchor_height"] = bad
            with self.assertRaisesRegex(SystemExit, "anchor_height", msg=repr(bad)):
                self._write(doc)

    def test_malformed_record_stops_install(self):
        # Copilot #38: only a missing file means "not deployed"; a present but empty or
        # truncated record fails closed.
        for bad in ({"bridge": "0x1234"}, {}, {"bridge": ""}, {"bridge": "  "}, [], "x"):
            with self.assertRaises(SystemExit, msg=repr(bad)):
                self._write(bad)

    def test_zero_address_record_stops_install(self):
        # Copilot #38 (review on 71420aa): a zero-address deploy record is not a bridge.
        for z in ("0x" + "00" * 20, "0X" + "00" * 20):
            with self.assertRaisesRegex(SystemExit, "zero address"):
                self._write({"bridge": z, "anchor_height": V2_TIP})

    def test_record_path_is_a_directory_stops_install(self):
        with tempfile.TemporaryDirectory() as d:
            bd = Path(d)
            (bd / "sepolia-qc-v3.json").mkdir()
            with patch.object(inst, "BRIDGE_DIR", bd), patch.object(inst, "ENV_OUT", bd / "e.env"):
                with self.assertRaisesRegex(SystemExit, "unreadable"):
                    inst.write_env({"bridge": "0x" + "11" * 20})

    def test_bad_record_writes_neither_output(self):
        # Copilot #38: validate V3 before writing sepolia-qc.json or the env file.
        with tempfile.TemporaryDirectory() as d:
            bd = Path(d)
            (bd / "sepolia-qc-v3.json").write_text(json.dumps({"bridge": "0x1234"}))
            json_out, env_out = bd / "sepolia-qc.json", bd / "falcon-pl-bridge.env"
            with patch.object(inst, "BRIDGE_DIR", bd), patch.object(inst, "ENV_OUT", env_out), \
                    patch.object(inst, "JSON_OUT", json_out), \
                    patch.object(inst, "load_template", return_value={"bridge": "0x" + "11" * 20}), \
                    patch.object(inst, "merge_deploy", side_effect=lambda d: d):
                with self.assertRaises(SystemExit):
                    inst.main()
            self.assertFalse(json_out.exists())
            self.assertFalse(env_out.exists())

    def test_unreadable_record_stops_install(self):
        with tempfile.TemporaryDirectory() as d:
            bd = Path(d)
            (bd / "sepolia-qc-v3.json").write_text('{"bridge": "0x')  # truncated
            with patch.object(inst, "BRIDGE_DIR", bd), patch.object(inst, "ENV_OUT", bd / "e.env"):
                with self.assertRaisesRegex(SystemExit, "unreadable"):
                    inst.write_env({"bridge": "0x" + "11" * 20})


if __name__ == "__main__":
    unittest.main()
