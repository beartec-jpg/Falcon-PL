#!/usr/bin/env python3
"""DEPRECATED: legacy watcher path without Sepolia sync-committee LC.

Use pl-eth-lc-relayer.py (peg-in) and pl-falcon-qc-prover.py (peg-out).
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time
import warnings
from pathlib import Path

warnings.warn(
    "pl-eth-deposit-watcher.py is deprecated — use pl-eth-lc-relayer.py",
    DeprecationWarning,
    stacklevel=1,
)

_SCRIPT = Path(__file__).resolve()
if str(_SCRIPT.parent) not in sys.path:
    sys.path.insert(0, str(_SCRIPT.parent))
from eth_bridge_config import ETH_RPC, FALCON_QC_BRIDGE, SEPOLIA_USDC  # noqa: E402

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = Path(os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl")))
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
KEYS = os.environ.get("FPL_KEYS", str(ROOT / "keys"))
ACCOUNT = os.environ.get("FPL_WATCHER_ACCOUNT", "watcher-browser")
SCHEME = os.environ.get("FPL_SCHEME", "falcon-512")
NETWORK_ID = os.environ.get("FPL_NETWORK_ID", "2300")
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
ETH_RPC = os.environ.get("ETH_RPC", ETH_RPC)
BRIDGE = os.environ.get("SEPOLIA_BRIDGE", FALCON_QC_BRIDGE)
USDC = SEPOLIA_USDC
ETH_LOCK = os.environ.get(
    "ETH_LOCK", "c1113d2059c5d73f45968910fadfff7d88a3c4a4d8a0f754f15faa495d2c467b"
)
USDC_LOCK = os.environ.get(
    "USDC_LOCK", "c8927e1f8b56b080b015ccf240c5d27c6f1525d52a6545a1c03c84326e483c1c"
)
STATE = Path(os.environ.get("STATE_PATH", str(ROOT / "run/eth-deposits.json")))
POLL = int(os.environ.get("POLL_SEC", "20"))
# Deposit(bytes32 dest20, address token, uint256 amount, bytes32 lockId)
TOPIC0 = "0x" + hashlib.sha256()  # placeholder, replaced below


def keccak_topic() -> str:
    out = subprocess.check_output(
        [str(CAST), "sig-event", "Deposit(bytes32,address,uint256,bytes32)"],
        text=True,
    ).strip()
    if out.startswith("0x") and len(out) >= 66:
        return out[:66]
    # fallback via cast keccak of the canonical event
    k = subprocess.check_output(
        [str(CAST), "keccak", "Deposit(bytes32,address,uint256,bytes32)"],
        text=True,
    ).strip()
    return k if k.startswith("0x") else "0x" + k


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def dest20(account: str) -> str:
    return hashlib.sha256(account.strip().lower().encode()).hexdigest()[:40]


def leaf(txid: str, log_index: int, token: str, dest_account: str, amount: int, lock_id: str) -> str:
    s = f"evm-dep-v1|{txid.lower().removeprefix('0x')}|{log_index}|{token.lower().removeprefix('0x')}|{dest20(dest_account)}|{amount}|{lock_id.lower().removeprefix('0x')}"
    return sha256_hex(s.encode())


def load_state() -> dict:
    try:
        return json.loads(STATE.read_text())
    except (OSError, json.JSONDecodeError):
        return {"last_block": 0, "minted": []}


def save_state(st: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(st, indent=2) + "\n")
    tmp.replace(STATE)


def ctl(*args: str) -> str:
    cmd = [str(CTL), "--addr", RPC, *args]
    return subprocess.check_output(cmd, text=True, timeout=60)


def rail_tip(asset: str) -> tuple[int, str]:
    raw = ctl("status")
    i = raw.find("{")
    body = json.JSONDecoder().raw_decode(raw[i:])[0]
    body = body.get("body") or body
    for r in body.get("rails") or []:
        if r.get("asset") == asset:
            return int(r.get("tip_height") or 0), str(r.get("tip_hash") or "")
    return 0, ""


def main() -> int:
    topic = keccak_topic()
    print("deposit watcher topic", topic, "bridge", BRIDGE, flush=True)
    st = load_state()
    while True:
        try:
            logs = subprocess.check_output(
                [
                    str(CAST),
                    "logs",
                    "--from-block",
                    str(max(0, int(st.get("last_block") or 0))),
                    "--to-block",
                    "latest",
                    "--address",
                    BRIDGE,
                    topic,
                    "--rpc-url",
                    ETH_RPC,
                    "--json",
                ],
                text=True,
                timeout=40,
            )
            rows = json.loads(logs) if logs.strip() else []
            if isinstance(rows, dict):
                rows = [rows]
        except Exception as e:
            print("cast logs:", e, flush=True)
            time.sleep(POLL)
            continue
        for lg in rows:
            txid = (lg.get("transactionHash") or "").lower()
            idx = int(lg.get("logIndex") or lg.get("index") or 0)
            blk = int(lg.get("blockNumber") or 0)
            data = (lg.get("data") or "0x")[2:]
            topics = lg.get("topics") or []
            if len(topics) < 3:
                continue
            dest_b32 = topics[1].lower().removeprefix("0x")
            token = "0x" + topics[2].lower().removeprefix("0x")[-40:]
            amount = int(data[0:64], 16) if len(data) >= 64 else 0
            lock = "0x" + data[64:128] if len(data) >= 128 else ""
            key = f"{txid}:{idx}"
            if key in st.get("minted", []):
                continue
            native = token.lower() == "0x" + "00" * 20
            asset = "ETH" if native else "USDC"
            lock_id = ETH_LOCK if native else USDC_LOCK
            # dest20 is indexed; we cannot invert to the name. User/watcher
            # must mint via submit-rail-deposit with the named account.
            print(f"DEPOSIT {asset} tx={txid} dest20={dest_b32[-40:]} amount={amount}", flush=True)
            st.setdefault("seen", []).append(
                {
                    "key": key,
                    "asset": asset,
                    "txid": txid,
                    "log_index": idx,
                    "dest20": dest_b32[-40:],
                    "amount": amount,
                    "token": token,
                    "block": blk,
                    "lock_id": lock_id,
                }
            )
            st["last_block"] = max(int(st.get("last_block") or 0), blk)
            save_state(st)
        time.sleep(POLL)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
