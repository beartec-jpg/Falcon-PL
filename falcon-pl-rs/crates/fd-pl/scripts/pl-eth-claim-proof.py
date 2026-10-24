#!/usr/bin/env python3
"""Merkle proof for a dest-lock ETH/USDC RailWithdraw note vs combined claim_root.

Usage:
  pl-eth-claim-proof.py --account dave --asset ETH --dest 0x... --amount 1000000000000000
  pl-eth-claim-proof.py --note <32-byte hex>
  pl-eth-claim-proof.py --note <hex> --fpl-height H --header-claim-root 0x<headers[H].claimRoot>

With --header-claim-root the proof is built against the leaves that header
committed, not the current tree. FalconQcBridgeV2/V3 FplHeader stores only
claimRoot (no leaf count), and the node does not keep a per-height leaf count,
so the committed set is found by recomputing candidate roots over the local
leaves (ETH rail first, then USDC, as lc::evm_claim_leaves) until one equals the
on-chain root. Both rails are append-only, so the committed set is
ETH[:a] + USDC[:b]; the plain "first N combined leaves" prefixes are tried first,
then every other pair (exhaustive, subtrees cached; see committed_split).
A note outside that set gets status waiting_for_bridge_header. The proof is
checked with the same fold as Solidity _merkleSha256 before it is returned.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = Path(os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl")))
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")


def sha256(b: bytes) -> bytes:
    return hashlib.sha256(b).digest()


def norm_hex(s: str) -> str:
    return (s or "").strip().lower().removeprefix("0x")


def parse_eth(s: str) -> bytes:
    h = norm_hex(s)
    if len(h) != 40:
        raise ValueError("dest must be 20-byte hex")
    return bytes.fromhex(h)


def withdraw_leaf(note32: bytes, dest20: bytes, amount: int, usdc: bool) -> bytes:
    buf = b"evm-wd-v1|" + note32 + dest20 + amount.to_bytes(8, "big") + (b"\x01" if usdc else b"\x00")
    return sha256(buf)


def build_tree(leaves: list[bytes], index: int) -> tuple[str, list[str]]:
    if not leaves:
        raise ValueError("empty claim tree")
    if index < 0 or index >= len(leaves):
        raise ValueError("leaf index")
    layer = list(leaves)
    idx = index
    path: list[str] = []
    while len(layer) > 1:
        if len(layer) % 2 == 1:
            layer.append(layer[-1])
        sib = layer[idx + 1] if idx % 2 == 0 else layer[idx - 1]
        path.append(sib.hex())
        nxt = []
        for i in range(0, len(layer), 2):
            nxt.append(sha256(layer[i] + layer[i + 1]))
        idx //= 2
        layer = nxt
    return layer[0].hex(), path


def merkle_root(leaves: list[bytes]) -> bytes:
    """Root over leaves, same pairing and odd-last padding as build_tree. Empty -> 32 zero bytes
    (lc::evm_header_claim_root returns [0; 32] for an empty tree)."""
    if not leaves:
        return b"\x00" * 32
    layer = list(leaves)
    while len(layer) > 1:
        if len(layer) % 2 == 1:
            layer.append(layer[-1])
        layer = [sha256(layer[i] + layer[i + 1]) for i in range(0, len(layer), 2)]
    return layer[0]


def merkle_verify(leaf: bytes, proof: list[bytes], index: int, root: bytes) -> bool:
    """Exactly FalconQcBridgeV2 _merkleSha256 / FalconQcBridgeV3Checks.merkleRoot(leaf, proof, index) == root.

    V3 also requires len(proof) <= 32 and index < 2**len(proof) ("depth"), and refuses a
    padded copy (a right child equal to its left sibling, "pad"), so each leaf has one index.
    Every proof build_tree returns already satisfies all three, so V2 is unaffected.
    """
    if index < 0 or index >= 2**32:
        return False
    if len(proof) > 32 or index >> len(proof):
        return False
    h = leaf
    idx = index
    for p in proof:
        if idx % 2 == 1 and p == h:
            return False
        h = sha256(h + p) if idx % 2 == 0 else sha256(p + h)
        idx //= 2
    return h == root


# committed_split is exhaustive over every ETH[:a] + USDC[:b]. This only refuses trees
# so large that (len(ETH)+1) * (len(USDC)+1) pairs would take too long, and says so
# (SplitSearchLimit), instead of reporting a false "no match".
MAX_SPLIT_PAIRS = 2_000_000  # ~1M pairs (1000 x 1000) is ~4 s; walletd gives the tool 30 s


class SplitSearchLimit(ValueError):
    pass


class _SplitRoots:
    """merkle_root(eth[:a] + usdc[:b]) for many (a, b) without rehashing shared subtrees.

    Node (k, lo) covers leaves [lo, min(lo + 2**k, n)) and equals
    H(node(k-1, lo) || node(k-1, lo + 2**(k-1)) or itself if that child is past n),
    which is exactly build_tree's odd-last padding. A node's value depends only on k
    and the leaves it covers, so nodes wholly inside ETH or wholly inside USDC are
    cached across all (a, b); nodes that straddle the ETH/USDC boundary are cached
    per a. Each new (a, b) then hashes only O(log n) nodes."""

    def __init__(self, eth: list[bytes], usdc: list[bytes]):
        self.eth = eth
        self.usdc = usdc
        self.shared: dict[tuple, bytes] = {}
        self.per_a: dict[tuple, bytes] = {}
        self.cur_a = -1

    def _node(self, k: int, lo: int, a: int, n: int) -> bytes:
        hi = min(lo + (1 << k), n)
        if hi <= a:
            key, cache = ("E", k, lo, hi), self.shared
        elif lo >= a:
            key, cache = ("U", k, lo - a, hi - a), self.shared
        else:
            key, cache = ("M", k, lo, hi), self.per_a
        got = cache.get(key)
        if got is not None:
            return got
        if k == 0:
            out = self.eth[lo] if lo < a else self.usdc[lo - a]
        else:
            half = 1 << (k - 1)
            left = self._node(k - 1, lo, a, n)
            right = self._node(k - 1, lo + half, a, n) if lo + half < n else left
            out = sha256(left + right)
        cache[key] = out
        return out

    def root(self, a: int, b: int) -> bytes:
        if a != self.cur_a:
            self.per_a.clear()
            self.cur_a = a
        n = a + b
        if n == 0:
            return b"\x00" * 32
        return self._node((n - 1).bit_length(), 0, a, n)


def committed_split(
    eth: list[bytes], usdc: list[bytes], header_root: bytes
) -> tuple[int, int] | None:
    """(a, b) such that merkle_root(eth[:a] + usdc[:b]) == header_root, or None.

    Order: the combined prefixes first (N = len..0, i.e. "first N leaves"), longest
    first, then every other ETH[:a] + USDC[:b] (an ETH note packed after a USDC note
    makes the committed set a non-prefix of the current combined list). Exhaustive:
    None means no such set exists. Raises SplitSearchLimit if the pair space is over
    MAX_SPLIT_PAIRS and no combined prefix matched."""
    roots = _SplitRoots(eth, usdc)
    total = len(eth) + len(usdc)
    prefixes = {(n, 0) if n <= len(eth) else (len(eth), n - len(eth)) for n in range(total + 1)}
    for a, b in sorted(prefixes, key=lambda ab: -(ab[0] + ab[1])):
        if roots.root(a, b) == header_root:
            return a, b
    pairs = (len(eth) + 1) * (len(usdc) + 1)
    if pairs > MAX_SPLIT_PAIRS:
        raise SplitSearchLimit(
            "header claimRoot is not a combined prefix and %d ETH x %d USDC leaves is past the "
            "split search limit (%d pairs); not a proven mismatch" % (len(eth), len(usdc), MAX_SPLIT_PAIRS)
        )
    for a in range(len(eth), -1, -1):
        for b in range(len(usdc), -1, -1):
            if (a, b) not in prefixes and roots.root(a, b) == header_root:
                return a, b
    return None


WAITING_MSG = "waiting for bridge header"
BLANK_ROOT_MSG = (
    "--header-claim-root is blank; pass headers[H].claimRoot as 32-byte hex, or omit the flag"
)


def committed_proof(
    eth: list[bytes], usdc: list[bytes], leaf: bytes, header_root: bytes
) -> dict:
    """Proof for `leaf` against the set `header_root` commits.

    Returns {"status": "ready", index, proof, n, split} or {"status": "waiting", n, split}.
    Raises ValueError when no candidate set matches the root or the built proof does not
    verify (never hand back a proof that openClaim would reject)."""
    split = committed_split(eth, usdc, header_root)
    if split is None:
        raise ValueError(
            "header claimRoot 0x%s matches no committed set of the local claim leaves "
            "(%d ETH + %d USDC); refusing to build a proof" % (header_root.hex(), len(eth), len(usdc))
        )
    a, b = split
    committed = eth[:a] + usdc[:b]
    if leaf in eth:
        i = eth.index(leaf)
        index = i if i < a else -1
    elif leaf in usdc:
        j = usdc.index(leaf)
        index = a + j if j < b else -1
    else:
        raise ValueError("leaf not in combined claim tree yet")
    base = {"n": len(committed), "split": {"ETH": a, "USDC": b}}
    if index < 0:
        return {"status": "waiting", **base}
    root, path = build_tree(committed, index)
    proof = [bytes.fromhex(x) for x in path]
    if bytes.fromhex(root) != header_root or not merkle_verify(leaf, proof, index, header_root):
        raise ValueError("built proof does not verify against header claimRoot; refusing")
    return {"status": "ready", "index": index, "proof": proof, **base}


def rail_leaf_lists(st: dict) -> tuple[list[bytes], list[bytes]] | None:
    """(ETH leaves, USDC leaves) from status rails, in claim order. None if rails carry no
    leaves or they disagree with lc.claim_leaves (then only the flat list is usable)."""
    rails = {str(r.get("asset")): r for r in (st.get("rails") or []) if isinstance(r, dict)}
    out: list[list[bytes]] = []
    for asset in ("ETH", "USDC"):
        row: list[bytes] = []
        for h in (rails.get(asset) or {}).get("claim_leaves") or []:
            b = bytes.fromhex(norm_hex(str(h)))
            if len(b) == 32:
                row.append(b)
        out.append(row)
    eth, usdc = out
    if not eth and not usdc:
        _log("leaf source: flat lc.claim_leaves, prefix-only search (status rails carry no claim_leaves)")
        return None
    flat = ((st.get("lc") or {}).get("claim_leaves")) or []
    if flat and combined_leaves(st) != eth + usdc:
        _log(
            "leaf source: flat lc.claim_leaves, prefix-only search (rails ETH %d + USDC %d disagree "
            "with lc.claim_leaves %d)" % (len(eth), len(usdc), len(flat))
        )
        return None
    return eth, usdc


def _log(msg: str) -> None:
    """One plain line on stderr (no JSON braces, so walletd's JSON scan skips it)."""
    print("pl-eth-claim-proof: " + msg.replace("{", "(").replace("}", ")"), file=sys.stderr, flush=True)


def ctl_status() -> dict:
    raw = subprocess.check_output([str(CTL), "--addr", RPC, "status"], text=True, timeout=30)
    i = raw.find("{")
    body = json.JSONDecoder().raw_decode(raw[i:])[0]
    return body.get("body") or body


def combined_leaves(st: dict) -> list[bytes]:
    lc = st.get("lc") or {}
    raw = lc.get("claim_leaves")
    out: list[bytes] = []
    if isinstance(raw, list) and raw:
        for h in raw:
            b = bytes.fromhex(norm_hex(str(h)))
            if len(b) == 32:
                out.append(b)
        return out
    rails = {str(r.get("asset")): r for r in (st.get("rails") or []) if isinstance(r, dict)}
    for asset in ("ETH", "USDC"):
        r = rails.get(asset) or {}
        for h in r.get("claim_leaves") or []:
            b = bytes.fromhex(norm_hex(str(h)))
            if len(b) == 32:
                out.append(b)
    return out


V3_REFUND_KIND = "v3_refund"


def v3_refund_note_id(deposit_id: str) -> str:
    """sha256("refund|" || depositId), hex without 0x. Same as bridge::v3_refund_note_id and
    FalconQcBridgeV3.refundNoteId."""
    b = bytes.fromhex(norm_hex(deposit_id))
    if len(b) != 32:
        raise ValueError("deposit id must be 32-byte hex")
    return sha256(b"refund|" + b).hex()


def _not_found_reason(st: dict, asset: str, note: str) -> str:
    """Why a note is missing, when node status says more than "not packed yet"."""
    n = norm_hex(note)
    for res in st.get("eth_v3_resolutions") or []:
        if not isinstance(res, dict):
            continue
        kind = str(res.get("kind") or "")
        if kind == "refund" and norm_hex(str(res.get("note_id") or "")) == n:
            got = str(res.get("asset") or "")
            if got.upper() != asset.upper():
                return f"V3 refund note is on the {got} rail, not {asset.upper()}"
            return "V3 refund resolved but its note is not listed in rails[].withdrawals (node older than this walletd?)"
        if kind == "mint" and n:
            dep = str(res.get("deposit_id") or "")
            try:
                if v3_refund_note_id(dep) == n:
                    return (
                        f"V3 deposit 0x{norm_hex(dep)} resolved as a mint to {res.get('credit_to')}; "
                        "it has no refund note"
                    )
            except ValueError:
                continue
    return "withdraw note not packed yet"


def _refund_fields(w: dict) -> dict:
    """Extra record keys for a V3 refund note; none for a RailWithdraw note (unchanged output)."""
    if w.get("kind") != V3_REFUND_KIND:
        return {}
    return {"kind": V3_REFUND_KIND, "depositId": "0x" + norm_hex(str(w.get("deposit_id") or ""))}


def find_note(st: dict, account: str, asset: str, dest: str, amount: int, note: str) -> dict:
    """The withdrawal row for this note. RailWithdraw rows match on account/dest/amount/note.
    V3 refund rows (kind v3_refund, no FPL account) are found only by note id (or the
    deposit id it derives from), never by account/dest/amount alone."""
    rails = {str(r.get("asset")): r for r in (st.get("rails") or []) if isinstance(r, dict)}
    r = rails.get(asset.upper())
    if not r:
        raise SystemExit(f"no rail {asset}")
    dest_n = "0x" + norm_hex(dest) if dest else ""
    hits = []
    for w in r.get("withdrawals") or []:
        if not isinstance(w, dict):
            continue
        nid = norm_hex(str(w.get("note_id") or ""))
        if note and nid != norm_hex(note):
            continue
        refund = w.get("kind") == V3_REFUND_KIND
        if refund and not note:
            continue
        if account and not refund and str(w.get("from") or "").strip().lower() != account.strip().lower():
            continue
        if amount and int(w.get("amount") or 0) != amount:
            continue
        if dest_n:
            got = "0x" + norm_hex(str(w.get("external_to") or ""))
            if got != dest_n:
                continue
        hits.append(w)
    if not hits:
        raise SystemExit(_not_found_reason(st, asset, note) if note else "withdraw note not packed yet")
    return hits[-1]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--account", default="")
    ap.add_argument("--asset", default="ETH")
    ap.add_argument("--dest", default="")
    ap.add_argument("--amount", default="0")
    ap.add_argument("--note", default="")
    ap.add_argument(
        "--deposit-id",
        default="",
        help="V3 depositId (32-byte hex): build the proof for its refund note sha256('refund|' || id)",
    )
    ap.add_argument(
        "--fpl-height",
        type=int,
        default=0,
        help="FalconQcBridge header height for openClaim (on-chain fplTip, not FPL mesh tip)",
    )
    ap.add_argument(
        "--header-claim-root",
        default=None,
        help="headers[--fpl-height].claimRoot read from the bridge; build the proof against the "
        "leaves that root commits (not the current tree) and verify it before printing",
    )
    args = ap.parse_args()
    if args.header_claim_root is not None and not args.header_claim_root.strip():
        # A blank flag (e.g. an unset shell var) must not fall back to the current tree.
        raise SystemExit(BLANK_ROOT_MSG)
    note = args.note
    if args.deposit_id.strip():
        try:
            refund_note = v3_refund_note_id(args.deposit_id)
        except ValueError as e:
            raise SystemExit(f"--deposit-id: {e}")
        if note.strip() and norm_hex(note) != refund_note:
            raise SystemExit("--note is not the refund note of --deposit-id")
        note = refund_note
    st = ctl_status()
    rec = build_record(
        st,
        args.account,
        args.asset,
        args.dest,
        int(args.amount or 0),
        note,
        int(args.fpl_height or 0),
        args.header_claim_root or "",
    )
    print(json.dumps(rec, separators=(",", ":")))
    return 0


NODE_BEHIND_MSG = "local node behind bridge header"


def _is_behind_error(msg: str) -> bool:
    """Failures that can just mean "the local node has not seen what the header commits yet".
    Bad-input errors (blank/short root, missing height, unknown rail) never qualify."""
    if msg in ("withdraw note not packed yet", "leaf not in combined claim tree yet"):
        return True
    if msg.startswith("header claimRoot 0x") and "matches no committed set" in msg:
        return True
    # SplitSearchLimit: too many local pairs to search exhaustively, not a proven mismatch.
    return msg.startswith("header claimRoot is not a combined prefix") and "not a proven mismatch" in msg


def _local_tip(st: dict) -> int | None:
    t = st.get("tip_height")
    return t if isinstance(t, int) and not isinstance(t, bool) else None


def build_record(
    st: dict,
    account: str,
    asset: str,
    dest_in: str,
    amount: int,
    note: str,
    fpl_h: int = 0,
    header_claim_root: str = "",
) -> dict:
    """Proof record. Against a bridge header (header_claim_root set), a local node whose
    tip_height is below the header height gets status node_behind_bridge_header (retryable)
    instead of a hard error when the note, the leaf or the committed set is not local yet."""
    try:
        return _build_record(st, account, asset, dest_in, amount, note, fpl_h, header_claim_root)
    except SystemExit as e:
        msg = str(e.code)
        tip = _local_tip(st)
        if not (header_claim_root and fpl_h > 0 and tip is not None and tip < fpl_h):
            raise
        if not _is_behind_error(msg):
            raise
        return {
            "ok": False,
            "ready": False,
            "waiting": True,
            "retryable": True,
            "status": "node_behind_bridge_header",
            "message": f"{NODE_BEHIND_MSG} (local tip {tip} < header {fpl_h}): {msg}",
            "asset": asset.strip().upper() or "ETH",
            "noteId": ("0x" + norm_hex(note)) if note else "",
            "localTip": tip,
            "fplTip": fpl_h,
        }


def _build_record(
    st: dict,
    account: str,
    asset: str,
    dest_in: str,
    amount: int,
    note: str,
    fpl_h: int = 0,
    header_claim_root: str = "",
) -> dict:
    asset = asset.strip().upper() or "ETH"
    w = find_note(st, account, asset, dest_in, amount, note)
    nid = norm_hex(str(w.get("note_id") or ""))
    dest = str(w.get("external_to") or dest_in)
    amt = int(w.get("amount") or amount)
    usdc = asset == "USDC"
    leaf = withdraw_leaf(bytes.fromhex(nid), parse_eth(dest), amt, usdc)
    leaves = combined_leaves(st)
    if not leaves:
        raise SystemExit("claim_leaves missing from status — restart archive with current binary")
    try:
        idx = leaves.index(leaf)
    except ValueError:
        raise SystemExit("leaf not in combined claim tree yet")
    lc_root = norm_hex(str((st.get("lc") or {}).get("claim_root") or ""))
    # Branch on the raw flag: "--header-claim-root 0x" and a whitespace-only value must be
    # rejected, not fall back to the current tree. Only "" (flag not given) takes the old path.
    if header_claim_root:
        hdr_hex = norm_hex(header_claim_root)
        return _record_vs_header(w, nid, dest, amt, usdc, asset, leaf, leaves, idx, st, lc_root, hdr_hex, fpl_h)
    root, proof = build_tree(leaves, idx)
    # openClaim reads headers[fplHeight]. Mesh tip is not a submitted header.
    ready = root == lc_root and bool(lc_root) and fpl_h > 0
    return {
        "ok": True,
        "noteId": "0x" + nid,
        **_refund_fields(w),
        "asset": asset,
        "account": w.get("from"),
        "dest": "0x" + norm_hex(dest),
        "amount": amt,
        "isUsdc": usdc,
        "leaf": "0x" + leaf.hex(),
        "index": idx,
        "proof": ["0x" + p for p in proof],
        "claimRoot": "0x" + root,
        "lcClaimRoot": "0x" + lc_root,
        "fplTip": fpl_h,
        "ready": ready,
    }


def _record_vs_header(
    w: dict,
    nid: str,
    dest: str,
    amt: int,
    usdc: bool,
    asset: str,
    leaf: bytes,
    leaves: list[bytes],
    idx: int,
    st: dict,
    lc_root: str,
    hdr_hex: str,
    fpl_h: int,
) -> dict:
    if fpl_h < 1:
        raise SystemExit("--header-claim-root needs --fpl-height (the on-chain header height)")
    if not hdr_hex:
        raise SystemExit(BLANK_ROOT_MSG)
    if len(hdr_hex) != 64:
        raise SystemExit("--header-claim-root must be 32-byte hex")
    header_root = bytes.fromhex(hdr_hex)
    lists = rail_leaf_lists(st)
    # Without per-rail lists, the flat combined list can only be cut as a prefix.
    eth, usdc_l = lists if lists is not None else (leaves, [])
    leaf_source = "rails" if lists is not None else "flat-prefix"
    try:
        got = committed_proof(eth, usdc_l, leaf, header_root)
    except ValueError as e:
        raise SystemExit(str(e))
    rec = {
        "ok": True,
        "noteId": "0x" + nid,
        **_refund_fields(w),
        "asset": asset,
        "account": w.get("from"),
        "dest": "0x" + norm_hex(dest),
        "amount": amt,
        "isUsdc": usdc,
        "leaf": "0x" + leaf.hex(),
        "currentIndex": idx,
        "currentLeaves": len(leaves),
        "committedLeaves": got["n"],
        "committedSplit": got["split"],
        "leafSource": leaf_source,
        "headerClaimRoot": "0x" + hdr_hex,
        "lcClaimRoot": "0x" + lc_root,
        "fplTip": fpl_h,
    }
    if got["status"] == "waiting":
        rec.update(
            {
                "ready": False,
                "waiting": True,
                "status": "waiting_for_bridge_header",
                "message": f"{WAITING_MSG} (note is leaf {idx}; header {fpl_h} commits {got['n']} leaves)",
            }
        )
        return rec
    rec.update(
        {
            "index": got["index"],
            "proof": ["0x" + p.hex() for p in got["proof"]],
            "claimRoot": "0x" + hdr_hex,
            "ready": True,
            "waiting": False,
            "status": "ready",
        }
    )
    return rec


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SystemExit as e:
        if isinstance(e.code, str):
            print(json.dumps({"ok": False, "error": e.code}), file=sys.stderr)
            raise SystemExit(1)
        raise
    except Exception as e:
        print(json.dumps({"ok": False, "error": str(e)}), file=sys.stderr)
        raise SystemExit(1)
