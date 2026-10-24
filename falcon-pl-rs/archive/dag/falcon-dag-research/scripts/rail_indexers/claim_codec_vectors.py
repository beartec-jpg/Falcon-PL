#!/usr/bin/env python3
"""
Claim codec vectors matching fd_exec::withdrawal_claim_* and FalconRailLock.sol.

leaf = sha256( noteId[32] || amount_be8 || height_be8 || sha256(external_to) || sha256(rail) )
parent = sha256( left[32] || right[32] )
empty = sha256(b"falcon-claim-root-empty-v1")

Run offline:  python3 scripts/rail_indexers/claim_codec_vectors.py
"""

from __future__ import annotations

import hashlib
import json
import sys
from typing import Any


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def note_id_bytes32(note_id: str) -> bytes:
    t = note_id.removeprefix("0x")
    try:
        b = bytes.fromhex(t)
        if len(b) == 32:
            return b
    except ValueError:
        pass
    return sha256(note_id.encode())


def claim_leaf(note: dict[str, Any] | str, amount: int | None = None, height: int | None = None,
               external_to: str | None = None, rail: str | None = None) -> str:
    """Accept a note dict (RPC shape) or explicit fields."""
    if isinstance(note, dict):
        nid = note_id_bytes32(note["note_id"])
        amount_i = int(note["amount"])
        height_i = int(note["falcon_height"])
        external_to_s = note["external_to"]
        rail_s = note["rail"]
    else:
        nid = note_id_bytes32(note)
        amount_i = int(amount)  # type: ignore[arg-type]
        height_i = int(height)  # type: ignore[arg-type]
        external_to_s = external_to  # type: ignore[assignment]
        rail_s = rail  # type: ignore[assignment]
    buf = (
        nid
        + amount_i.to_bytes(8, "big")
        + height_i.to_bytes(8, "big")
        + sha256(external_to_s.encode())
        + sha256(rail_s.encode())
    )
    return sha256(buf).hex()


def claim_parent(left_hex: str, right_hex: str) -> str:
    return sha256(bytes.fromhex(left_hex) + bytes.fromhex(right_hex)).hex()


def claim_root(notes: list[dict[str, Any]]) -> str:
    if not notes:
        return empty_claim_root()
    leaves = [claim_leaf(n) for n in sorted(notes, key=lambda x: x["note_id"])]
    return merkle_root_of_leaves(leaves)


def empty_claim_root() -> str:
    return sha256(b"falcon-claim-root-empty-v1").hex()


def merkle_root_of_leaves(leaves: list[str]) -> str:
    if not leaves:
        return empty_claim_root()
    layer = list(leaves)
    while len(layer) > 1:
        if len(layer) % 2 == 1:
            layer.append(layer[-1])
        nxt = []
        for i in range(0, len(layer), 2):
            nxt.append(claim_parent(layer[i], layer[i + 1]))
        layer = nxt
    return layer[0]


def merkle_proof(leaves: list[str], index: int) -> list[str]:
    if not leaves or index < 0 or index >= len(leaves):
        raise ValueError("bad index")
    layer = list(leaves)
    path: list[str] = []
    idx = index
    while len(layer) > 1:
        if len(layer) % 2 == 1:
            layer.append(layer[-1])
        if idx % 2 == 0:
            path.append(layer[idx + 1])
        else:
            path.append(layer[idx - 1])
        idx //= 2
        nxt = []
        for i in range(0, len(layer), 2):
            nxt.append(claim_parent(layer[i], layer[i + 1]))
        layer = nxt
    return path


def verify_proof(leaf_hex: str, path: list[str], index: int, root_hex: str) -> bool:
    h = leaf_hex.removeprefix("0x")
    idx = index
    for sib in path:
        sib = sib.removeprefix("0x")
        if idx % 2 == 0:
            h = claim_parent(h, sib)
        else:
            h = claim_parent(sib, h)
        idx //= 2
    return h == root_hex.removeprefix("0x")


# Golden vectors (must match fd-exec claim_leaf_golden_matches_solidity_spec)
GOLDEN = {
    "empty_root": "55eabc17e925fa0d01fbfcb220fecafce4dab37a12b5a165642a760c0167845c",
    "notes": [
        {
            "note_id": "11" * 32,
            "rail": "BTC",
            "amount": 7000,
            "falcon_height": 9,
            "external_to": "bc1q_restart_out",
            "leaf": "9ae206ffe6f284b3c64883949436e6e21cb6da3ebac76fb09c7b588a8cff4e85",
        },
        {
            "note_id": "22" * 32,
            "rail": "ETH",
            "amount": 1500,
            "falcon_height": 12,
            "external_to": "0xeth_smoke_out",
            "leaf": "939f90ad03104b5c2a1381b4c814352f367f99bd4ab4868596b2101770058132",
        },
        {
            "note_id": "33" * 32,
            "rail": "BNB",
            "amount": 1000,
            "falcon_height": 15,
            "external_to": "0xbnb_smoke_out",
            "leaf": "1a76906c79499f0a65c6b219910493a418e5adaa35041f2c135ae8fd7578d594",
        },
    ],
    "root_1": "9ae206ffe6f284b3c64883949436e6e21cb6da3ebac76fb09c7b588a8cff4e85",
    "root_2": "66b64c675f1a89617e23dfc575832d324a5cd50fdef8ce62c690fd3726ad3823",
    "root_3": "d83931a298ae420ff828525c844f810690464a2a1dae74dd6ddaa11d92be4a8a",
}


def run_offline() -> None:
    assert empty_claim_root() == GOLDEN["empty_root"], empty_claim_root()
    leaves = []
    for g in GOLDEN["notes"]:
        leaf = claim_leaf(g)
        assert leaf == g["leaf"], (leaf, g["leaf"])
        leaves.append(leaf)
    assert merkle_root_of_leaves(leaves[:1]) == GOLDEN["root_1"]
    assert merkle_root_of_leaves(leaves[:2]) == GOLDEN["root_2"]
    assert merkle_root_of_leaves(leaves) == GOLDEN["root_3"]
    # proof for middle leaf under 3-note tree
    path = merkle_proof(leaves, 1)
    assert verify_proof(leaves[1], path, 1, GOLDEN["root_3"])
    print(
        json.dumps(
            {
                "ok": True,
                "codec": "sha256 claim leaf v1",
                "empty_root": GOLDEN["empty_root"][:16] + "…",
                "root_3": GOLDEN["root_3"][:16] + "…",
                "notes": 3,
            }
        )
    )


if __name__ == "__main__":
    try:
        run_offline()
    except AssertionError as e:
        print(f"FAIL claim_codec_vectors: {e}", file=sys.stderr)
        sys.exit(1)
