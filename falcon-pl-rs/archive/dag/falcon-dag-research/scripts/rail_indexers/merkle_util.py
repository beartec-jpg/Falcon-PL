"""Merkle helpers matching fd-exec bridge.rs."""

from __future__ import annotations

import hashlib


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def dsha256(data: bytes) -> bytes:
    return sha256(sha256(data))


def sha256_hex_str(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


def reverse_hex(h: str) -> str:
    """Byte-reverse a hex string (Bitcoin display txid ↔ internal merkle leaf)."""
    t = h.removeprefix("0x")
    b = bytes.fromhex(t)
    return b[::-1].hex()


def note_id_bytes32(s: str) -> bytes:
    t = s.removeprefix("0x")
    try:
        b = bytes.fromhex(t)
        if len(b) == 32:
            return b
    except ValueError:
        pass
    return sha256(s.encode())


def merkle_root_protocol(leaf: str, path: list[str], index: int) -> str:
    h = leaf
    idx = index
    for sib in path:
        if idx % 2 == 0:
            h = sha256_hex_str(f"{h}|{sib}")
        else:
            h = sha256_hex_str(f"{sib}|{h}")
        idx //= 2
    return h


def merkle_root_bitcoin(leaf_hex: str, path: list[str], index: int) -> str:
    h = bytes.fromhex(leaf_hex)
    if len(h) != 32:
        raise ValueError("leaf must be 32 bytes hex")
    idx = index
    for sib_hex in path:
        sib = bytes.fromhex(sib_hex)
        if len(sib) != 32:
            raise ValueError("sibling must be 32 bytes hex")
        if idx % 2 == 0:
            h = dsha256(h + sib)
        else:
            h = dsha256(sib + h)
        idx //= 2
    return h.hex()


def merkle_root_binary(leaf_hex: str, path: list[str], index: int) -> str:
    """sha256(left||right) parents — EVM deposit trees + claim trees."""
    h = bytes.fromhex(leaf_hex)
    if len(h) != 32:
        raise ValueError("leaf must be 32 bytes")
    idx = index
    for sib_hex in path:
        sib = bytes.fromhex(sib_hex)
        if idx % 2 == 0:
            h = sha256(h + sib)
        else:
            h = sha256(sib + h)
        idx //= 2
    return h.hex()


def synth_hash32(label: str) -> str:
    return sha256(label.encode()).hex()


def synthetic_inclusion_protocol(leaf: str, siblings: list[str], index: int = 0):
    root = merkle_root_protocol(leaf, siblings, index)
    return siblings, root


def synthetic_inclusion_bitcoin(leaf_hex: str, siblings_hex: list[str], index: int = 0):
    root = merkle_root_bitcoin(leaf_hex, siblings_hex, index)
    return siblings_hex, root


def evm_deposit_leaf(tx_hash: str, log_index: int, amount: int, lock_id: str) -> str:
    """Match fd_exec::evm_deposit_leaf."""
    buf = (
        note_id_bytes32(tx_hash)
        + int(log_index).to_bytes(8, "big")
        + int(amount).to_bytes(8, "big")
        + note_id_bytes32(lock_id)
    )
    return sha256(buf).hex()


def evm_bind_root(tx_root_hex: str, deposit_root_hex: str) -> str:
    """Match fd_exec::evm_bind_root — sha256(tx_root[32]‖deposit_root[32])."""
    t = note_id_bytes32(tx_root_hex)
    d = note_id_bytes32(deposit_root_hex)
    return sha256(t + d).hex()


def empty_evm_deposit_root() -> str:
    return sha256(b"falcon-evm-deposit-empty-v1").hex()


def empty_xrp_deposit_root() -> str:
    return sha256(b"falcon-xrp-deposit-empty-v1").hex()


def xrp_deposit_leaf(tx_hash: str, amount: int, lock_id: str, destination: str) -> str:
    """Match fd_exec::xrp_deposit_leaf."""
    buf = (
        note_id_bytes32(tx_hash)
        + int(amount).to_bytes(8, "big")
        + note_id_bytes32(lock_id)
        + sha256(destination.encode())
    )
    return sha256(buf).hex()


def build_binary_merkle(leaves: list[str]) -> tuple[str, list[list[str]]]:
    """
    Returns (root, proofs) where proofs[i] is sibling path for leaves[i].
    Leaves are 32-byte hex strings. Odd layers duplicate last (Bitcoin-style).
    """
    if not leaves:
        return sha256(b"falcon-evm-deposit-empty-v1").hex(), []

    n = len(leaves)
    paths: list[list[str]] = [[] for _ in range(n)]
    layers: list[list[str]] = [list(leaves)]

    while len(layers[-1]) > 1:
        layer = list(layers[-1])
        if len(layer) % 2 == 1:
            layer.append(layer[-1])
        nxt: list[str] = []
        for i in range(0, len(layer), 2):
            nxt.append(sha256(bytes.fromhex(layer[i]) + bytes.fromhex(layer[i + 1])).hex())
        layers.append(nxt)

    for li in range(n):
        idx = li
        for layer in layers[:-1]:
            layer_work = list(layer)
            if len(layer_work) % 2 == 1:
                layer_work.append(layer_work[-1])
            if idx % 2 == 0:
                sib = layer_work[idx + 1]
            else:
                sib = layer_work[idx - 1]
            paths[li].append(sib)
            idx //= 2

    return layers[-1][0], paths
