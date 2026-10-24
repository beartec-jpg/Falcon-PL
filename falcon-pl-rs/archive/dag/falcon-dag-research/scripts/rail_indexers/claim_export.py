#!/usr/bin/env python3
"""
Export withdrawal claim payloads from Falcon public state (no committee).

Anyone can recompute the same root from `withdrawal_notes` RPC — that is the trust
model: Falcon finality + deterministic encoding, not a peg federation.

  # Falcon-native (sha256 pairing — matches fd-exec withdrawal_claim_*)
  python3 scripts/rail_indexers/claim_export.py --rpc http://127.0.0.1:18811 --rail BTC

  # EVM keccak path for FalconRailLock.sol (requires pysha3 or eth-hash)
  python3 scripts/rail_indexers/claim_export.py --rpc http://127.0.0.1:18811 --rail ETH --evm \\
      --note-id <id>
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import urllib.request
from pathlib import Path
from typing import Any

# allow running as scripts/rail_indexers/claim_export.py
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))


def rpc(url: str, method: str, params: dict) -> dict:
    body = json.dumps({"method": method, "params": [params]}).encode()
    req = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode()).get("result", {})


def sha256_hex(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


def keccak256(data: bytes) -> bytes:
    try:
        from eth_hash.auto import keccak  # type: ignore

        return keccak(data)
    except Exception:
        pass
    try:
        import sha3  # type: ignore

        return sha3.keccak_256(data).digest()
    except Exception:
        from rail_indexers.keccak_lite import keccak256 as k  # type: ignore

        return k(data)


def abi_encode_claim(note_id_hex: str, to_addr: str, amount: int) -> bytes:
    """abi.encode(bytes32 noteId, address to, uint256 amount)."""
    nid = bytes.fromhex(note_id_hex.removeprefix("0x"))
    if len(nid) != 32:
        raise ValueError("note_id must be 32-byte hex")
    addr = to_addr.removeprefix("0x").lower()
    if len(addr) != 40:
        # allow non-address external_to for tests — pad/hash into 20 bytes
        addr = hashlib.sha256(to_addr.encode()).hexdigest()[:40]
    to_b = bytes.fromhex(addr)
    amt = amount.to_bytes(32, "big")
    return nid + (b"\x00" * 12) + to_b + amt


def merkle_root_keccak(leaves: list[bytes]) -> bytes:
    if not leaves:
        return keccak256(b"empty")
    layer = list(leaves)
    while len(layer) > 1:
        if len(layer) % 2 == 1:
            layer.append(layer[-1])
        nxt = []
        for i in range(0, len(layer), 2):
            nxt.append(keccak256(layer[i] + layer[i + 1]))
        layer = nxt
    return layer[0]


def merkle_proof_keccak(leaves: list[bytes], index: int) -> list[bytes]:
    layer = list(leaves)
    path: list[bytes] = []
    idx = index
    while len(layer) > 1:
        if len(layer) % 2 == 1:
            layer.append(layer[-1])
        if idx % 2 == 0:
            path.append(layer[idx + 1])
        else:
            path.append(layer[idx - 1])
        nxt = []
        for i in range(0, len(layer), 2):
            nxt.append(keccak256(layer[i] + layer[i + 1]))
        layer = nxt
        idx //= 2
    return path


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default="http://127.0.0.1:18811")
    ap.add_argument("--rail", default="")
    ap.add_argument("--note-id", default="")
    ap.add_argument("--evm", action="store_true", help="keccak codec for FalconRailLock")
    args = ap.parse_args()

    params: dict[str, Any] = {}
    if args.rail:
        params["rail"] = args.rail
    notes_resp = rpc(args.rpc, "withdrawal_notes", params)
    notes = notes_resp.get("notes") or []
    if not notes:
        print(json.dumps({"error": "no open withdrawals", "resp": notes_resp}, indent=2))
        return 1

    if args.evm:
        leaves = [
            keccak256(abi_encode_claim(n["note_id"], n["external_to"], int(n["amount"])))
            for n in notes
        ]
        root = merkle_root_keccak(leaves).hex()
        out: dict[str, Any] = {
            "codec": "evm_keccak_abi_encode",
            "rail": args.rail or notes[0].get("rail"),
            "claim_root": "0x" + root,
            "count": len(notes),
            "committee": None,
            "submitFalconRoot": {
                "root": "0x" + root,
                "height": max(int(n.get("falcon_height") or 0) for n in notes),
            },
            "claims": [],
        }
        for i, n in enumerate(notes):
            if args.note_id and n["note_id"] != args.note_id:
                continue
            path = merkle_proof_keccak(leaves, i)
            out["claims"].append(
                {
                    "note_id": "0x" + n["note_id"],
                    "to": n["external_to"],
                    "amount": n["amount"],
                    "proofLeaf": "0x" + leaves[i].hex(),
                    "proofPath": ["0x" + p.hex() for p in path],
                    "pathIndex": i,
                    "falcon_height": n.get("falcon_height"),
                }
            )
        print(json.dumps(out, indent=2))
        return 0

    # Falcon-native: prefer node claim_export for a single note
    if args.note_id:
        exp = rpc(args.rpc, "claim_export", {"note_id": args.note_id})
        print(json.dumps(exp, indent=2))
        return 0 if "error" not in exp else 1

    print(json.dumps(notes_resp, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
