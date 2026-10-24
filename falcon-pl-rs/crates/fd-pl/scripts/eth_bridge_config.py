"""Sepolia ETH/USDC dest-lock constants — single source of truth for all bridge operators.

Load order (first wins after env):
  1. FALCON_QC_BRIDGE / SEPOLIA_BRIDGE env (explicit override)
  2. $PL_ROOT/run/bridge/sepolia-qc.json  (installed by deploy-eth-sepolia.sh)
  3. $PL_ROOT/run/snark/falcon-qc-bridge-deploy.json (fresh deploy output)
  4. CANONICAL_* constants below (match docs + Rust evm_spv.rs)

All bridge systemd units must use EnvironmentFile=$PL_ROOT/run/bridge/falcon-pl-bridge.env
written by pl-bridge-install-config.py (called from deploy-eth-sepolia.sh).
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

CANONICAL_BRIDGE = "0xf8F1471643792eb1cD5d0C31061629777E55bc48"  # V1 FalconQcBridge (peg-in); 0x933F… test seed is retired
CANONICAL_VERIFIER = "0x9992cD8e45A2b7983E2b7f8fC725308a0E4845EC"
LEGACY_BABYJUB_BRIDGE = "0x7eB72974F2d2a4AaDFabAf0975a29470fcd163E4"
CANONICAL_USDC = "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238"
ETH_NATIVE = "0x0000000000000000000000000000000000000000"


def pl_root() -> Path:
    return Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))


def _read_json(path: Path) -> dict:
    try:
        raw = json.loads(path.read_text())
        return raw if isinstance(raw, dict) else {}
    except (OSError, json.JSONDecodeError):
        return {}


def load_bridge_doc() -> dict:
    """Merge bridge JSON sources; later files fill gaps only."""
    merged: dict = {}
    for path in (
        pl_root() / "run/bridge/sepolia-qc.json",
        pl_root() / "run/snark/falcon-qc-bridge-deploy.json",
    ):
        if path.is_file():
            doc = _read_json(path)
            for k, v in doc.items():
                if v is not None and v != "":
                    merged[k] = v
    return merged


def _pick(env_keys: tuple[str, ...], doc_key: str, doc: dict, default: str) -> str:
    for k in env_keys:
        v = os.environ.get(k, "").strip()
        if v:
            return v
    v = doc.get(doc_key)
    if isinstance(v, str) and v.strip():
        return v.strip()
    return default


_doc = load_bridge_doc()

FALCON_QC_BRIDGE = _pick(
    ("FALCON_QC_BRIDGE", "SEPOLIA_BRIDGE", "FPL_SEPOLIA_QC_BRIDGE"),
    "bridge",
    _doc,
    CANONICAL_BRIDGE,
).strip()

GROTH16_VERIFIER = _pick(("SEPOLIA_VERIFIER",), "verifier", _doc, CANONICAL_VERIFIER).strip()

SEPOLIA_USDC = _pick(("SEPOLIA_USDC",), "usdc", _doc, CANONICAL_USDC).strip()

FPL_NETWORK_ID = os.environ.get("FPL_NETWORK_ID", str(_doc.get("network_id") or "2300"))

ETH_RPC = os.environ.get(
    "ETH_RPC", str(_doc.get("eth_rpc") or "https://ethereum-sepolia-rpc.publicnode.com")
).strip()

# Lodestar serves Sepolia Gloas light-client updates. publicnode's beacon is
# frozen at that fork (sync period 1378) and is not a usable default.
BEACON_RPC = os.environ.get(
    "BEACON_RPC",
    str(_doc.get("beacon_rpc") or "https://lodestar-sepolia.chainsafe.io"),
).strip()

MEMPOOL_API = os.environ.get("MEMPOOL_API", "https://mempool.space/testnet/api").rstrip("/")

_fallbacks_raw = os.environ.get("ETH_RPC_FALLBACKS", "")
if _fallbacks_raw.strip():
    ETH_RPC_FALLBACKS = [u.strip() for u in _fallbacks_raw.split(",") if u.strip()]
elif isinstance(_doc.get("eth_rpc_fallbacks"), list):
    ETH_RPC_FALLBACKS = [str(u).strip() for u in _doc["eth_rpc_fallbacks"] if str(u).strip()]
else:
    ETH_RPC_FALLBACKS = [
        "https://rpc.sepolia.org",
        "https://1rpc.io/sepolia",
    ]


def dest20(account: str) -> bytes:
    return hashlib.sha256(account.strip().lower().encode()).digest()[:20]


def dest20_hex(account: str) -> str:
    return "0x" + dest20(account).hex()


def parse_cast_uint(raw: str) -> int:
    """Parse `cast call` / `cast send` numeric output (may include human suffix)."""
    s = (raw or "").strip()
    if not s:
        return 0
    tok = s.split()[0]
    if tok.startswith("0x"):
        return int(tok, 16)
    return int(tok)


def bridge_config_summary() -> dict:
    return {
        "bridge": FALCON_QC_BRIDGE,
        "verifier": GROTH16_VERIFIER,
        "usdc": SEPOLIA_USDC,
        "network_id": FPL_NETWORK_ID,
        "eth_rpc": ETH_RPC,
        "pl_root": str(pl_root()),
        "doc_paths": [
            str(p)
            for p in (
                pl_root() / "run/bridge/sepolia-qc.json",
                pl_root() / "run/snark/falcon-qc-bridge-deploy.json",
            )
            if p.is_file()
        ],
    }
