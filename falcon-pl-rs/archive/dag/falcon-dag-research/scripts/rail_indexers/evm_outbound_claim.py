#!/usr/bin/env python3
"""
Close EVM outbound claim loop against Falcon auto-committed claim_root.

  1. Read rail_info.claim_root (protocol-committed, no committee)
  2. Read claim_export / withdrawal_notes
  3. Verify merkle proof offline (same sha256 codec as fd-exec + FalconRailLock.sol)
  4. Emit submitFalconRoot + claim calldata JSON for agents

Usage:
  python3 scripts/rail_indexers/evm_outbound_claim.py \\
      --rpc http://127.0.0.1:18811 --rail BTC [--note-id <id>]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def rpc(url: str, method: str, params: dict) -> dict:
    body = json.dumps({"method": method, "params": [params]}).encode()
    req = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode()).get("result", {})


def note_id_bytes32(note_id: str) -> bytes:
    t = note_id.removeprefix("0x")
    try:
        b = bytes.fromhex(t)
        if len(b) == 32:
            return b
    except ValueError:
        pass
    return sha256(note_id.encode())


def claim_leaf(note: dict) -> bytes:
    """Match fd_exec::withdrawal_claim_leaf / FalconRailLock.claimLeaf."""
    nid = note_id_bytes32(note["note_id"])
    amount = int(note["amount"]).to_bytes(8, "big")
    height = int(note["falcon_height"]).to_bytes(8, "big")
    ext = sha256(note["external_to"].encode())
    rail = sha256(note["rail"].encode())
    return sha256(nid + amount + height + ext + rail)


def claim_parent(left: bytes, right: bytes) -> bytes:
    return sha256(left + right)


def verify_proof(leaf: bytes, path: list[str], index: int, root_hex: str) -> bool:
    h = leaf
    idx = index
    for sib_hex in path:
        sib = bytes.fromhex(sib_hex.removeprefix("0x"))
        if idx % 2 == 0:
            h = claim_parent(h, sib)
        else:
            h = claim_parent(sib, h)
        idx //= 2
    return h.hex() == root_hex.removeprefix("0x")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default="http://127.0.0.1:18811")
    ap.add_argument("--rail", default="BTC")
    ap.add_argument("--note-id", default="")
    ap.add_argument("--out", default="", help="write JSON payload path")
    args = ap.parse_args()

    info = rpc(args.rpc, "rail_info", {"rail": args.rail})
    if info.get("error"):
        print(json.dumps(info, indent=2))
        return 1

    bridge = rpc(args.rpc, "bridge_status", {})
    committed = (info.get("claim_root") or "").removeprefix("0x")
    if not committed:
        print(json.dumps({"error": "empty claim_root", "rail_info": info}, indent=2))
        return 1
    if info.get("claim_root_matches") is False:
        print(
            json.dumps(
                {"error": "claim_root_matches is false", "rail_info": info}, indent=2
            ),
            file=sys.stderr,
        )
        return 2

    notes = rpc(args.rpc, "withdrawal_notes", {"rail": args.rail}).get("notes") or []
    if not notes:
        print(json.dumps({"error": "no open withdrawals", "claim_root": committed}))
        return 1

    note_id = args.note_id or notes[0]["note_id"]
    exp = rpc(args.rpc, "claim_export", {"note_id": note_id})
    if exp.get("error"):
        print(json.dumps(exp, indent=2))
        return 1

    leaf = claim_leaf(exp)
    path = exp.get("merkle_path") or []
    idx = int(exp.get("merkle_index") or 0)
    root = (exp.get("claim_root") or committed).removeprefix("0x")

    ok = verify_proof(leaf, path, idx, root)
    if not ok:
        print(
            json.dumps(
                {
                    "error": "proof verification failed",
                    "leaf": leaf.hex(),
                    "path": path,
                    "index": idx,
                    "root": root,
                    "committed": committed,
                },
                indent=2,
            ),
            file=sys.stderr,
        )
        return 3

    if root != committed:
        print(
            json.dumps(
                {
                    "error": "export root != rail committed claim_root",
                    "export_root": root,
                    "committed": committed,
                },
                indent=2,
            ),
            file=sys.stderr,
        )
        return 4

    fin = exp.get("finality") or {}
    if fin.get("ready_for_foreign_claim") is False:
        print(
            json.dumps({"error": "not ready_for_foreign_claim", "finality": fin}, indent=2),
            file=sys.stderr,
        )
        return 5

    # Parse optional EVM address from external_to
    ext = exp.get("external_to") or ""
    to_addr = ext if ext.startswith("0x") and len(ext) == 42 else None
    state_root = exp.get("state_root") or bridge.get("state_root") or ""

    payload = {
        "codec": "falcon_claim_sha256_v1",
        "committee": None,
        "rail": args.rail,
        "verified": True,
        "finality": {
            "state_root": state_root,
            "hard_final_batches": bridge.get("hard_final_batches")
            or exp.get("hard_final_batches"),
            "executed_txs": bridge.get("executed_txs") or exp.get("executed_txs"),
            "applied_user_txs": bridge.get("applied_user_txs")
            or exp.get("applied_user_txs"),
            "ready_for_foreign_claim": True,
            "bound": "claim_root is in Falcon state_root; post only after hard finality",
        },
        "submitFalconRoot": {
            "root": "0x" + committed,
            "height": int(info.get("claim_height") or exp.get("falcon_height") or 0),
            "solidity": f'submitFalconRoot(bytes32(0x{committed}), uint64({int(info.get("claim_height") or 0)}))',
            "falcon_state_root": state_root,
        },
        "claim": {
            "noteId": "0x" + note_id_bytes32(exp["note_id"]).hex(),
            "to": to_addr or ext,
            "amount": exp["amount"],
            "falconHeight": exp["falcon_height"],
            "rail": exp["rail"],
            "externalTo": exp["external_to"],
            "proofPath": ["0x" + p.removeprefix("0x") for p in path],
            "pathIndex": idx,
            "proofLeaf": "0x" + leaf.hex(),
        },
        "lock_id": info.get("lock_id"),
        "claim_note_count": info.get("claim_note_count"),
        "note": "Broadcast submitFalconRoot then claim on FalconRailLock — root bound to Falcon state_root, no committee",
    }
    text = json.dumps(payload, indent=2)
    print(text)
    if args.out:
        Path(args.out).write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
