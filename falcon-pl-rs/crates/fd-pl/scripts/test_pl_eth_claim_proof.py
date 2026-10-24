#!/usr/bin/env python3
"""Offline tests for pl-eth-claim-proof.py and pl-walletd.py eth_open_claim (no network).

  python3 crates/fd-pl/scripts/test_pl_eth_claim_proof.py

Fixture: scripts/fixtures/eth-claim-proof/v2-header-353130.json. Every value there is
real and carries its source: FalconQcBridgeV2 headers(353130) read with eth_call on
Sepolia, and the ETH/USDC claim leaves + withdrawals from falcon-pl-ctl status.
The tests re-derive each leaf from its withdrawal and each root from the leaves.
"""
from __future__ import annotations

import copy
import importlib.util
import json
import re
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


cp = _load("claim_proof", "pl-eth-claim-proof.py")
wd = _load("walletd", "pl-walletd.py")
# openClaim gas (F4, #63) is tested in test_pl_walletd_openclaim_gas.py; these cast mocks stay send-only.
wd._open_claim_gas = lambda bridge, args: (850_000, 100_000_000)

FX = json.loads((_HERE / "fixtures/eth-claim-proof/v2-header-353130.json").read_text())
HDR = FX["header"]
ST = FX["status"]
ONCHAIN_ROOT = bytes.fromhex(HDR["claimRoot"][2:])  # 9671942a…, on-chain headers(353130)
LC_ROOT = bytes.fromhex(ST["lc"]["claim_root"])  # 4146c6c6…, current 10-leaf tree
RAILS = {r["asset"]: r for r in ST["rails"]}
ETH = [bytes.fromhex(h) for h in RAILS["ETH"]["claim_leaves"]]
USDC = [bytes.fromhex(h) for h in RAILS["USDC"]["claim_leaves"]]
ALL = ETH + USDC


def _withdrawals_by_index() -> list[tuple[str, dict]]:
    """(asset, withdrawal) for combined leaf 0..9, matched by recomputing withdrawLeaf."""
    out: list[tuple[str, dict] | None] = [None] * len(ALL)
    for asset in ("ETH", "USDC"):
        for w in RAILS[asset]["withdrawals"]:
            leaf = cp.withdraw_leaf(
                bytes.fromhex(w["note_id"]), cp.parse_eth(w["external_to"]), int(w["amount"]), asset == "USDC"
            )
            out[ALL.index(leaf)] = (asset, w)
    assert all(out), "every claim leaf must map to a withdrawal"
    return out  # type: ignore[return-value]


NOTES = _withdrawals_by_index()


def _status() -> dict:
    return copy.deepcopy(ST)


class RealFixture(unittest.TestCase):
    def test_leaves_are_withdraw_leaves(self):
        self.assertEqual(len(ALL), 10)
        self.assertEqual([bytes.fromhex(h) for h in ST["lc"]["claim_leaves"]], ALL)
        self.assertEqual(cp.combined_leaves(_status()), ALL)

    def test_roots(self):
        self.assertEqual(cp.merkle_root(ALL), LC_ROOT)
        self.assertEqual(LC_ROOT.hex()[:8], "4146c6c6")
        self.assertEqual(cp.merkle_root(ALL[:9]), ONCHAIN_ROOT)
        self.assertEqual(ONCHAIN_ROOT.hex()[:8], "9671942a")
        self.assertEqual(cp.merkle_root(ETH).hex(), RAILS["ETH"]["claim_root"])
        self.assertEqual(cp.merkle_root(USDC).hex(), RAILS["USDC"]["claim_root"])

    def test_old_behaviour_reproduces_bug(self):
        # The pre-fix proof came from the current 10-leaf tree; none of 0..8 pass the contract check.
        for i in range(9):
            root, path = cp.build_tree(ALL, i)
            self.assertEqual(root, LC_ROOT.hex())
            proof = [bytes.fromhex(x) for x in path]
            self.assertFalse(cp.merkle_verify(ALL[i], proof, i, ONCHAIN_ROOT), i)


class CommittedSet(unittest.TestCase):
    def test_n_is_nine(self):
        self.assertEqual(cp.committed_split(ETH, USDC, ONCHAIN_ROOT), (5, 4))
        self.assertEqual(cp.committed_split(ETH, USDC, LC_ROOT), (5, 5))

    def test_leaves_0_to_8_verify_against_onchain_root(self):
        for i in range(9):
            got = cp.committed_proof(ETH, USDC, ALL[i], ONCHAIN_ROOT)
            self.assertEqual(got["status"], "ready")
            self.assertEqual(got["n"], 9)
            self.assertEqual(got["index"], i)
            self.assertTrue(cp.merkle_verify(ALL[i], got["proof"], i, ONCHAIN_ROOT), i)

    def test_leaf_9_waits(self):
        got = cp.committed_proof(ETH, USDC, ALL[9], ONCHAIN_ROOT)
        self.assertEqual(got, {"status": "waiting", "n": 9, "split": {"ETH": 5, "USDC": 4}})

    def test_root_matching_nothing_refuses(self):
        # The header's own block hash (a real on-chain word that is not a claim root).
        bogus = bytes.fromhex(HDR["hash"][2:])
        with self.assertRaisesRegex(ValueError, "matches no committed set"):
            cp.committed_proof(ETH, USDC, ALL[0], bogus)

    def test_non_prefix_split(self):
        # An ETH note packed after a USDC note: the header committed ETH[:4] + USDC[:2], which
        # is not a prefix of today's ETH-then-USDC list. Root derived from the real leaves.
        root = cp.merkle_root(ETH[:4] + USDC[:2])
        self.assertEqual(cp.committed_split(ETH, USDC, root), (4, 2))
        got = cp.committed_proof(ETH, USDC, USDC[1], root)
        self.assertEqual((got["status"], got["index"], got["n"]), ("ready", 5, 6))
        self.assertTrue(cp.merkle_verify(USDC[1], got["proof"], 5, root))
        self.assertEqual(cp.committed_proof(ETH, USDC, ETH[4], root)["status"], "waiting")
        self.assertEqual(cp.committed_proof(ETH, USDC, USDC[2], root)["status"], "waiting")

    def test_empty_header_root_waits(self):
        # lc::evm_header_claim_root is [0; 32] for an empty tree.
        got = cp.committed_proof(ETH, USDC, ALL[0], b"\x00" * 32)
        self.assertEqual((got["status"], got["n"]), ("waiting", 0))


class SplitSearch(unittest.TestCase):
    def test_fast_roots_match_reference(self):
        roots = cp._SplitRoots(ETH, USDC)
        for a in range(len(ETH) + 1):
            for b in range(len(USDC) + 1):
                self.assertEqual(roots.root(a, b), cp.merkle_root(ETH[:a] + USDC[:b]), (a, b))

    @staticmethod
    def _synthetic(tag: str, n: int) -> list[bytes]:
        # Scale-only test vectors (not chain data): distinct 32-byte leaves.
        return [cp.sha256(b"scale-test|" + tag.encode() + i.to_bytes(4, "big")) for i in range(n)]

    def test_small_split_found_in_large_tree(self):
        # Copilot #33: with 150 + 150 leaves, ETH[:5] + USDC[:4] sat past the old 20k-candidate
        # cap and came back as "no match". The search is exhaustive now.
        eth, usdc = self._synthetic("E", 150), self._synthetic("U", 150)
        root = cp.merkle_root(eth[:5] + usdc[:4])
        self.assertEqual(cp.committed_split(eth, usdc, root), (5, 4))
        got = cp.committed_proof(eth, usdc, usdc[3], root)
        self.assertEqual((got["status"], got["index"], got["n"]), ("ready", 8, 9))
        self.assertTrue(cp.merkle_verify(usdc[3], got["proof"], 8, root))

    def test_true_mismatch_is_none(self):
        eth, usdc = self._synthetic("E", 150), self._synthetic("U", 150)
        self.assertIsNone(cp.committed_split(eth, usdc, bytes.fromhex(HDR["hash"][2:])))

    def test_limit_is_not_a_mismatch(self):
        root = cp.merkle_root(ETH[:4] + USDC[:2])  # not a combined prefix
        with patch.object(cp, "MAX_SPLIT_PAIRS", 10):
            with self.assertRaises(cp.SplitSearchLimit):
                cp.committed_split(ETH, USDC, root)
            with self.assertRaisesRegex(ValueError, "not a proven mismatch"):
                cp.committed_proof(ETH, USDC, ETH[0], root)
            # Combined prefixes are still tried first under the limit.
            self.assertEqual(cp.committed_split(ETH, USDC, ONCHAIN_ROOT), (5, 4))


class BuildRecord(unittest.TestCase):
    def _rec(self, i: int, root: str = HDR["claimRoot"]) -> dict:
        asset, w = NOTES[i]
        return cp.build_record(_status(), "", asset, w["external_to"], int(w["amount"]), w["note_id"], 353130, root)

    def test_ready_records(self):
        for i in range(9):
            rec = self._rec(i)
            self.assertTrue(rec["ready"], i)
            self.assertEqual(rec["status"], "ready")
            self.assertEqual(rec["index"], i)
            self.assertEqual(rec["committedLeaves"], 9)
            self.assertEqual(rec["claimRoot"], HDR["claimRoot"])
            proof = [bytes.fromhex(p[2:]) for p in rec["proof"]]
            self.assertTrue(cp.merkle_verify(ALL[i], proof, i, ONCHAIN_ROOT))

    def test_waiting_record(self):
        rec = self._rec(9)
        self.assertFalse(rec["ready"])
        self.assertTrue(rec["waiting"])
        self.assertEqual(rec["status"], "waiting_for_bridge_header")
        self.assertTrue(rec["message"].startswith("waiting for bridge header"))
        self.assertNotIn("proof", rec)

    def test_mismatch_exits(self):
        with self.assertRaises(SystemExit) as cm:
            self._rec(0, HDR["hash"])
        self.assertIn("matches no committed set", str(cm.exception.code))

    def test_empty_header_root_flag_rejected(self):
        # Copilot #33: "--header-claim-root 0x" must not fall back to the current tree.
        for bad in ("0x", " 0x ", "0x1234"):
            with self.assertRaises(SystemExit) as cm:
                self._rec(0, bad)
            self.assertIn("32-byte hex", str(cm.exception.code))

    def test_without_header_root_unchanged(self):
        asset, w = NOTES[0]
        rec = cp.build_record(_status(), "", asset, w["external_to"], int(w["amount"]), w["note_id"], 353130)
        self.assertEqual(rec["claimRoot"], "0x" + LC_ROOT.hex())
        self.assertEqual(rec["index"], 0)
        self.assertNotIn("status", rec)

    def test_flat_leaves_only_uses_prefix(self):
        st = _status()
        for r in st["rails"]:
            r["claim_leaves"] = []
        self.assertIsNone(cp.rail_leaf_lists(st))
        asset, w = NOTES[3]
        rec = cp.build_record(st, "", asset, w["external_to"], int(w["amount"]), w["note_id"], 353130, HDR["claimRoot"])
        self.assertEqual((rec["status"], rec["index"], rec["committedLeaves"]), ("ready", 3, 9))


class V3Header(unittest.TestCase):
    """V3 openClaim reads the same FplHeader and _merkleSha256, so the lookup and prefix logic
    are shared. Guard that: if V3's struct or getter changes, these fail."""

    @staticmethod
    def _struct(sol: str) -> list[str]:
        src = (_HERE / sol).read_text()
        body = re.search(r"struct FplHeader \{(.*?)\}", src, re.S).group(1)
        return [" ".join(x.split()) for x in body.split(";") if x.strip()]

    def test_same_struct(self):
        v2 = self._struct("FalconQcBridgeV2.sol")
        self.assertEqual(v2, self._struct("FalconQcBridgeV3.sol"))
        self.assertEqual(
            v2,
            [
                "bytes32 hash",
                "bytes32 parent",
                "bytes32 innerHash",
                "bytes32 claimRoot",
                "bytes32 nextCommitteeRoot",
                "bool finalized",
            ],
        )

    def test_same_merkle_and_open_check(self):
        v2 = (_HERE / "FalconQcBridgeV2.sol").read_text()
        v3 = (_HERE / "FalconQcBridgeV3.sol").read_text()
        ck = (_HERE / "FalconQcBridgeV3Checks.sol").read_text()
        for src in (v2, v3):
            self.assertIn("mapping(uint64 => FplHeader) public headers;", src)
        self.assertIn("require(_merkleSha256(leaf, proof, index) == hdr.claimRoot, \"merkle\");", v2)
        # V3: openClaim passes the header's claimRoot to FalconQcBridgeV3Checks.checkClaim.
        self.assertIn("checks.checkClaim(v2, v1, destLock, noteId, dest, amount, isUsdc, proof, index, hdr.claimRoot, anchorSet)", v3)
        self.assertIn("require(merkleRoot(leaf, proof, index) == claimRoot, \"merkle\");", ck)
        self.assertIn("require(proof.length <= 32 && (uint256(index) >> proof.length) == 0, \"depth\");", ck)
        for src in (v2, ck):
            self.assertIn("h = sha256(abi.encodePacked(h, p));", src)
            self.assertIn("h = sha256(abi.encodePacked(p, h));", src)

    def test_merkle_verify_matches_v3_depth_bound(self):
        leaf, sib = b"\x01" * 32, b"\x02" * 32
        root = cp.sha256(leaf + sib)
        self.assertTrue(cp.merkle_verify(leaf, [sib], 0, root))
        self.assertFalse(cp.merkle_verify(leaf, [sib], 2, root))  # index outside 1 level
        self.assertTrue(cp.merkle_verify(leaf, [], 0, leaf))
        self.assertFalse(cp.merkle_verify(leaf, [], 1, leaf))
        self.assertFalse(cp.merkle_verify(leaf, [sib] * 33, 0, root))

    def test_merkle_verify_refuses_padded_copy_on_real_roots(self):
        # Real trees: V2 headers(353130) commits leaves 0..8 (9, odd -> leaf 8 padded), the
        # 10-leaf LC tree pads layer 1 (leaves 8,9 repeat at 10,11). Without the "pad" rule the
        # V2-committed leaf 8 would also prove at a second (padded) index. V3 refuses it via
        # `pad` here, and refuses every V2-anchor leaf by set membership (`anchor`) at any index.
        r9, p8 = cp.build_tree(ALL[:9], 8)
        self.assertEqual(r9, ONCHAIN_ROOT.hex())
        proof8 = [bytes.fromhex(x) for x in p8]
        self.assertTrue(cp.merkle_verify(ALL[8], proof8, 8, ONCHAIN_ROOT))
        self.assertEqual(proof8[0], ALL[8])  # its sibling is its own padded copy
        self.assertFalse(cp.merkle_verify(ALL[8], proof8, 9, ONCHAIN_ROOT))
        r10, q8 = cp.build_tree(ALL, 8)
        self.assertEqual(r10, LC_ROOT.hex())
        q8 = [bytes.fromhex(x) for x in q8]
        self.assertTrue(cp.merkle_verify(ALL[8], q8, 8, LC_ROOT))
        self.assertFalse(cp.merkle_verify(ALL[8], q8, 10, LC_ROOT))
        # note 9 (leaf index 9) is the only leaf the 10-leaf tree adds; it stays provable.
        _, q9 = cp.build_tree(ALL, 9)
        self.assertTrue(cp.merkle_verify(ALL[9], [bytes.fromhex(x) for x in q9], 9, LC_ROOT))

    def test_parse_header_cast_output(self):
        out = "\n".join(
            [HDR["hash"], HDR["parent"], HDR["innerHash"], HDR["claimRoot"], HDR["nextCommitteeRoot"], "true"]
        )
        got = wd._parse_qc_header(out + "\n")
        self.assertEqual(got["claimRoot"], HDR["claimRoot"])
        self.assertTrue(got["finalized"])
        with self.assertRaises(RuntimeError):
            wd._parse_qc_header("0x01\n")

    def test_header_lookup_takes_any_bridge(self):
        out = "\n".join(
            [HDR["hash"], HDR["parent"], HDR["innerHash"], HDR["claimRoot"], HDR["nextCommitteeRoot"], "true"]
        )
        seen = []

        def run(argv, **kw):
            seen.append(argv)
            return subprocess.CompletedProcess(argv, 0, out, "")

        v3 = "0x" + "ab" * 20  # address only routes the mocked call; no value is read from it
        with patch.object(wd.subprocess, "run", side_effect=run):
            got = wd._qc_header(353130, v3)
        self.assertEqual(got["claimRoot"], HDR["claimRoot"])
        self.assertEqual(seen[0][:5], [wd.CAST, "call", v3, wd.HEADER_SIG, "353130"])


class WalletdOpenClaim(unittest.TestCase):
    """eth_open_claim with every external call mocked: no cast send reaches a network."""

    def setUp(self):
        self.sends: list[list[str]] = []
        self.header_root = HDR["claimRoot"]
        self.patches = [
            patch.object(wd, "QC_V2_LIVE", True),
            patch.object(wd, "QC_V2_BRIDGE", HDR["bridge"]),
            patch.object(wd, "_cast_uint", return_value=353130),
            patch.object(wd, "_qc_header", side_effect=self._hdr),
            # claims(note) pre-send read (#38 double-pay guard): never opened.
            patch.object(wd, "_claim_dest", return_value=wd.ZERO_ADDR),
            patch.object(wd, "_claim_state", return_value={"dest": wd.ZERO_ADDR, "taken": False}),
            patch.object(wd, "QC_V3_BRIDGE", ""),
            patch.object(wd, "claim_proof", side_effect=self._claim_proof),
            patch.object(wd, "_eth_pk", return_value="0xnot-a-key"),
            patch.object(wd.subprocess, "run", side_effect=self._run),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()

    def _hdr(self, height, bridge=""):
        self.assertEqual(height, 353130)
        return {**{k: HDR[k] for k in ("hash", "parent", "innerHash", "nextCommitteeRoot")},
                "claimRoot": self.header_root, "finalized": True}

    def _claim_proof(self, account, asset, dest, amount, note="", fpl_height=0, header_claim_root=""):
        try:
            return cp.build_record(_status(), "", asset, dest, amount, note, fpl_height, header_claim_root)
        except SystemExit as e:
            raise RuntimeError(str(e.code))

    def _run(self, argv, **kw):
        self.assertEqual(argv[1], "send")
        self.sends.append(argv)
        # No tx hash in the stub output, so eth_open_claim stops at "produced no tx hash".
        return subprocess.CompletedProcess(argv, 0, "stub: no broadcast", "")

    def _open(self, i):
        asset, w = NOTES[i]
        return wd.eth_open_claim(w["note_id"], w["external_to"], int(w["amount"]), asset == "USDC", "acct")

    def test_leaf_9_waits_and_sends_nothing(self):
        rec = self._open(9)
        self.assertEqual(rec["status"], "waiting_for_bridge_header")
        self.assertEqual(rec["message"], "waiting for bridge header")
        self.assertTrue(rec["waiting"])
        self.assertNotIn("openClaim failed", json.dumps(rec))
        self.assertEqual(self.sends, [])

    def test_leaves_0_to_8_send_verified_proof(self):
        for i in range(9):
            self.sends.clear()
            with self.assertRaisesRegex(RuntimeError, "produced no tx hash"):
                self._open(i)
            self.assertEqual(len(self.sends), 1, i)
            argv = self.sends[0]
            asset, w = NOTES[i]
            self.assertEqual(argv[2], HDR["bridge"])
            self.assertEqual(argv[4], "0x" + w["note_id"])
            proof = [bytes.fromhex(x[2:]) for x in argv[8].strip("[]").split(",")]
            self.assertEqual(argv[9], str(i))
            self.assertEqual(argv[10], "353130")
            self.assertTrue(cp.merkle_verify(ALL[i], proof, i, ONCHAIN_ROOT))

    def test_mismatch_refuses_without_send(self):
        self.header_root = HDR["hash"]
        with self.assertRaisesRegex(RuntimeError, "matches no committed set"):
            self._open(0)
        self.assertEqual(self.sends, [])

    def test_local_verify_blocks_bad_proof(self):
        # If the proof tool ever returned a proof that does not fold to the header root,
        # walletd must refuse before cast send.
        real = self._claim_proof

        def bad(*a, **k):
            rec = real(*a, **k)
            rec["proof"] = list(reversed(rec["proof"]))
            return rec

        with patch.object(wd, "claim_proof", side_effect=bad):
            with self.assertRaisesRegex(RuntimeError, "does not verify against headers\\[353130\\]"):
                self._open(0)
        self.assertEqual(self.sends, [])


class WalletdHttp(unittest.TestCase):
    def test_waiting_is_409(self):
        import io

        body = json.dumps({"action": "eth-open-claim", "noteId": "0x" + "00" * 32,
                           "dest": "0x" + "00" * 20, "amount": "1", "asset": "ETH", "account": "acct"}).encode()
        h = wd.H.__new__(wd.H)
        h.headers = {"Content-Length": str(len(body))}
        h.rfile = io.BytesIO(body)
        sent = {}
        h._json = lambda code, obj: sent.update(code=code, obj=obj)
        waiting = {"ok": False, "waiting": True, "status": "waiting_for_bridge_header", "message": "waiting for bridge header"}
        with patch.object(wd, "eth_open_claim", return_value=waiting):
            h.do_POST()
        self.assertEqual(sent["code"], 409)
        self.assertEqual(sent["obj"]["status"], "waiting_for_bridge_header")


if __name__ == "__main__":
    unittest.main()
