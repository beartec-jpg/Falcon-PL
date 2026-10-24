#!/usr/bin/env python3
"""Local ETH + USDC rail e2e (proofs, not live Sepolia)."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

CTL = os.environ.get(
    "FPL_CTL",
    str(Path.home() / "falcon-pl/falcon-pl-rs/target/release/falcon-pl-ctl"),
)
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19401")
ETH_TOKEN = "0000000000000000000000000000000000000000"
USDC_TOKEN = "1c7d4b196cb0c7b01d743fbc6116a902379c7238"


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def dest20(account: str) -> str:
    return sha256(account.strip().lower().encode())[:20].hex()


def leaf(txid: str, log_index: int, token: str, dest: str, amount: int, lock_id: str) -> str:
    s = f"evm-dep-v1|{txid.lower()}|{log_index}|{token.lower()}|{dest20(dest)}|{amount}|{lock_id}"
    return sha256(s.encode()).hex()


def ctl(*args: str) -> str:
    cmd = [CTL, "--addr", RPC, *args]
    out = subprocess.check_output(cmd, text=True, timeout=60)
    if '"type": "err"' in out or '"type":"err"' in out:
        raise subprocess.CalledProcessError(11, cmd, output=out)
    return out


def status() -> dict:
    raw = ctl("status")
    i = raw.find("{")
    body = json.JSONDecoder().raw_decode(raw[i:])[0]
    return body.get("body") or body


def rail(asset: str) -> dict:
    for r in status().get("rails") or []:
        if r.get("asset") == asset:
            return r
    raise SystemExit(f"no rail {asset}")


def main() -> int:
    tag = os.environ.get("E2E_TAG", "run1")
    st = status()
    print("tip", st.get("tip_height"), "product", st.get("product"), flush=True)
    eth_lock = rail("ETH")["lock_id"]
    usdc_lock = rail("USDC")["lock_id"]
    eth_tx, usdc_tx = f"eth-{tag}", f"usdc-{tag}"
    eth_leaf = leaf(eth_tx, 0, ETH_TOKEN, "sally", 8_000, eth_lock)
    usdc_leaf = leaf(usdc_tx, 0, USDC_TOKEN, "dave", 12_000, usdc_lock)

    def headers(asset: str, prefix: str, root1: str) -> None:
        parent = ""
        tip = int(rail(asset).get("tip_height") or 0)
        if tip >= 6:
            return
        for h in range(1, 7):
            hid = f"{prefix}{h}"
            root = root1 if h == 1 else f"{prefix}empty{h}"
            try:
                ctl(
                    "rail-header",
                    "--asset",
                    asset,
                    "--height",
                    str(h),
                    "--hash",
                    hid,
                    "--parent-hash",
                    parent,
                    "--merkle-root",
                    root,
                    "--watcher",
                    "alice",
                )
            except subprocess.CalledProcessError as e:
                if "already" not in (e.output or ""):
                    raise
            parent = hid

    headers("ETH", f"e{tag}", eth_leaf)
    headers("USDC", f"u{tag}", usdc_leaf)

    def deposit(asset, to, amount, txid, block, merkle, token):
        return ctl(
            "rail-deposit",
            "--asset",
            asset,
            "--to",
            to,
            "--amount",
            str(amount),
            "--external-txid",
            txid,
            "--block-hash",
            block,
            "--height",
            "1",
            "--merkle-root",
            merkle,
            "--token",
            token,
            "--watcher",
            "alice",
            "--test-fixture",
        )

    print(deposit("ETH", "sally", 8000, eth_tx, f"e{tag}1", eth_leaf, ETH_TOKEN), flush=True)
    print(deposit("USDC", "dave", 12000, usdc_tx, f"u{tag}1", usdc_leaf, USDC_TOKEN), flush=True)
    try:
        deposit("ETH", "sally", 8000, eth_tx, f"e{tag}1", eth_leaf, ETH_TOKEN)
        print("FAIL replay accepted", file=sys.stderr)
        return 11
    except subprocess.CalledProcessError:
        print("replay rejected OK", flush=True)
    try:
        deposit("USDC", "sally", 12000, usdc_tx + "x", f"u{tag}1", usdc_leaf, USDC_TOKEN)
        print("FAIL wrong dest accepted", file=sys.stderr)
        return 12
    except subprocess.CalledProcessError:
        print("wrong dest rejected OK", flush=True)

    ctl("rail-withdraw", "--asset", "ETH", "--from", "sally", "--amount", "3000",
        "--external-to", "0x2E0630BB9339AC427feB2aaae31DFEf90fD1181e")
    ctl("rail-withdraw", "--asset", "USDC", "--from", "dave", "--amount", "5000",
        "--external-to", "0xa978781b96aFDFe5653BEFbE935F3C235795F5AE")
    print("ETH/USDC local e2e minted+withdrew (unequal).", flush=True)
    print("Live coins: deploy FalconBridge.sol (not the old open-withdraw lock).", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
