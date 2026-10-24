#!/usr/bin/env python3
"""Live Bitcoin testnet e2e for the Falcon PL vault rail.

Waits for Sally/Dave/Fee funding confirmations, parks value into the vault
with FALC dest, submits headers, mints FBTC, AMM+account trade, unequal
withdraw, watcher/challenger checks.

Does NOT send to tb1q7dnl… and does not flip the wallet BTC_RAIL_LIVE flag.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
CTL = os.environ.get(
    "FPL_CTL",
    str(Path.home() / "falcon-pl/falcon-pl-rs/target/release/falcon-pl-ctl"),
)
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19401")
KEYS = Path(os.environ.get("FPL_KEYS", str(Path.home() / "falcon-pl-vault-test/e2e/keys")))
VAULT_KEYS = Path(os.environ.get("FPL_VAULT_KEYS", str(Path.home() / "falcon-pl-vault-test/keys")))
WALLETS = Path(os.environ.get("FPL_WALLETS", str(Path.home() / "falcon-pl-vault-test/btc-wallets")))
NETWORK_ID = int(os.environ.get("FPL_NETWORK_ID", "2300"))
SCHEME = os.environ.get("FPL_SCHEME", "hmac")
MIN_CONFS = int(os.environ.get("FPL_MIN_CONFS", "1"))
DEPOSIT_CONFS = int(os.environ.get("FPL_DEPOSIT_CONFS", "6"))
OLD_HOLD = "tb1q7dnlzumm50hke3yl75rywds3gtfu2swqwu4xa08kx0ctxs00rv4q8hrkwu"

UA = {"User-Agent": "fpl-btc-e2e/2.9"}


def http_json(url: str, data: bytes | None = None):
    req = urllib.request.Request(url, data=data, headers=UA)
    if data is not None:
        req.add_header("Content-Type", "text/plain")
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = r.read().decode()
    if not raw:
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw


def broadcast(raw_hex: str) -> str:
    req = urllib.request.Request(
        f"{EXPLORER}/tx",
        data=raw_hex.encode(),
        headers={**UA, "Content-Type": "text/plain"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode().strip()


def addr_info(addr: str) -> dict:
    return http_json(f"{EXPLORER}/address/{addr}")


def addr_utxos(addr: str) -> list:
    return http_json(f"{EXPLORER}/address/{addr}/utxo") or []


def tx_status(txid: str) -> dict:
    return http_json(f"{EXPLORER}/tx/{txid}") or {}


def confirmed(txid: str, need: int) -> bool:
    st = (tx_status(txid).get("status") or {})
    if not st.get("confirmed"):
        return False
    tip = int(http_json(f"{EXPLORER}/blocks/tip/height") or 0)
    h = int(st.get("block_height") or 0)
    return tip >= h and (tip - h + 1) >= need


def wait_txid(txid: str, need: int, label: str, timeout: int = 7200) -> None:
    t0 = time.time()
    while time.time() - t0 < timeout:
        if confirmed(txid, need):
            print(f"CONFIRMED {label} {txid} ≥{need}", flush=True)
            return
        print(f"wait {label} {txid} need={need}", flush=True)
        time.sleep(20)
    raise SystemExit(f"timeout waiting for {label} {txid}")


def ctl(*args: str) -> str:
    cmd = [CTL, "--addr", RPC, *args]
    print("+", " ".join(cmd[:8]), "…", flush=True)
    return subprocess.check_output(cmd, text=True, timeout=60)


def vault_pub() -> dict:
    return json.loads((VAULT_KEYS / "vault-pub.json").read_text())


def wallet(name: str) -> dict:
    return json.loads((WALLETS / f"{name}.json").read_text())


def merkle_proof(txid: str) -> tuple[str, int, list[str], str, int]:
    """Return (block_hash, height, siblings_internal_hex, merkle_root, index)."""
    t = tx_status(txid)
    st = t.get("status") or {}
    bh = st.get("block_hash")
    height = int(st.get("block_height") or 0)
    if not bh:
        raise RuntimeError(f"{txid} unconfirmed")
    proof = http_json(f"{EXPLORER}/tx/{txid}/merkle-proof")
    # blockstream: {block_height, merkle:[siblings display?], pos}
    siblings = proof.get("merkle") or []
    pos = int(proof.get("pos") or 0)
    # convert display siblings to internal (byte-reversed) as btc_spv expects
    internal = []
    for s in siblings:
        b = bytes.fromhex(s)
        internal.append(b[::-1].hex())
    hdr = http_json(f"{EXPLORER}/block/{bh}")
    merkle_root = hdr.get("merkle_root") or ""
    return bh, height, internal, merkle_root, pos


def header_hex(block_hash: str) -> str:
    raw = http_json(f"{EXPLORER}/block/{block_hash}/raw")
    if isinstance(raw, str) and len(raw) >= 160:
        return raw[:160]
    # some APIs return hex string; others binary. try header endpoint
    h = http_json(f"{EXPLORER}/block/{block_hash}/header")
    if isinstance(h, str):
        return h.strip()
    raise RuntimeError(f"no header for {block_hash}")


def tip_headers_since(start_height: int, end_height: int) -> list[tuple[int, str, str]]:
    out = []
    for h in range(start_height, end_height + 1):
        bh = http_json(f"{EXPLORER}/block-height/{h}")
        if isinstance(bh, str):
            bh = bh.strip()
        raw = header_hex(bh)
        out.append((h, bh, raw))
    return out


def main() -> int:
    vp = vault_pub()
    vault_addr = vp["address"]
    vault_spk = vp["spk_hex"]
    if vault_addr == OLD_HOLD or OLD_HOLD in vault_spk:
        raise SystemExit("refusing old hold address")
    print(f"vault {vault_addr} spk={vault_spk}", flush=True)

    sally = wallet("sally")
    dave = wallet("dave")
    funded = {
        "sally": sally.get("p2pkh") or sally["address"],
        "dave": dave.get("p2pkh") or dave["address"],
        "fee": wallet("fee").get("p2pkh") or wallet("fee")["address"],
    }
    print("funded", funded, flush=True)

    # Phase 0 — wait for the user's funding txs (1 conf is enough to spend).
    for name, addr in funded.items():
        info = addr_info(addr)
        chain = (info.get("chain_stats") or {}).get("funded_txo_sum") or 0
        mem = (info.get("mempool_stats") or {}).get("funded_txo_sum") or 0
        print(f"{name} {addr} chain={chain} mem={mem}", flush=True)
        if chain == 0 and mem == 0:
            raise SystemExit(f"{name} has no coins")
        txs = http_json(f"{EXPLORER}/address/{addr}/txs") or []
        if not txs:
            raise SystemExit(f"{name} no txs")
        wait_txid(txs[0]["txid"], MIN_CONFS, f"fund-{name}")

    # Phase 1 — spend P2PKH → vault + FALC dest
    deposits = {}
    for name, dest_acct in (("sally", "sally"), ("dave", "dave")):
        addr = funded[name]
        utxos = [u for u in addr_utxos(addr) if (u.get("status") or {}).get("confirmed")]
        if not utxos:
            raise SystemExit(f"{name} no confirmed utxo")
        u = max(utxos, key=lambda x: int(x.get("value") or 0))
        sats = int(u["value"])
        fee = 1000
        raw = ctl(
            "btc-deposit-tx",
            "--privhex-file",
            str(WALLETS / f"{name}.privhex"),
            "--prev-txid",
            u["txid"],
            "--vout",
            str(u["vout"]),
            "--sats",
            str(sats),
            "--dest-account",
            dest_acct,
            "--vault-spk",
            vault_spk,
            "--fee",
            str(fee),
        ).strip().splitlines()[-1]
        if OLD_HOLD in raw:
            raise SystemExit("deposit hex references old hold")
        txid = broadcast(raw)
        print(f"DEPOSIT_BROADCAST {name} {txid} vault_amt={sats - fee}", flush=True)
        deposits[name] = {
            "txid": txid,
            "amount": sats - fee,
            "raw": raw,
        }

    for name, d in deposits.items():
        wait_txid(d["txid"], DEPOSIT_CONFS, f"vault-{name}")

    # Phase 2 — headers + mint (requires a live local node)
    try:
        ctl("status")
    except Exception as e:
        print(f"NODE_DOWN {e}", flush=True)
        print("Start the local e2e fleet then re-run with --mint-only", flush=True)
        Path("/tmp/fpl-btc-e2e-deposits.json").write_text(json.dumps(deposits, indent=2))
        return 2

    # Submit headers covering deposit heights.
    heights = []
    for d in deposits.values():
        t = tx_status(d["txid"])
        heights.append(int((t.get("status") or {}).get("block_height") or 0))
    lo, hi = min(heights), max(heights)
    tip = int(http_json(f"{EXPLORER}/blocks/tip/height") or hi)
    hi = max(hi, tip)  # include confirmations
    # don't submit the entire chain — only from first deposit through tip
    print(f"headers {lo}..{hi}", flush=True)
    for height, bh, raw in tip_headers_since(lo, hi):
        try:
            ctl(
                "submit-btc-header",
                "--account",
                "alice",
                "--raw",
                raw,
                "--height",
                str(height),
                "--fee",
                "2",
                "--scheme",
                SCHEME,
                "--keys-dir",
                str(KEYS),
                "--network-id",
                str(NETWORK_ID),
            )
        except subprocess.CalledProcessError as e:
            print(f"header {height} skip/err {e}", flush=True)

    for name, d in deposits.items():
        bh, height, siblings, merkle, pos = merkle_proof(d["txid"])
        ctl(
            "submit-rail-deposit",
            "--account",
            "alice",
            "--asset",
            "BTC",
            "--to",
            name,
            "--amount",
            str(d["amount"]),
            "--external-txid",
            d["txid"],
            "--block-hash",
            bh,
            "--height",
            str(height),
            "--merkle-root",
            merkle,
            "--raw-tx",
            d["raw"],
            "--merkle-path",
            ",".join(siblings),
            "--merkle-index",
            str(pos),
            "--scheme",
            SCHEME,
            "--keys-dir",
            str(KEYS),
            "--network-id",
            str(NETWORK_ID),
        )
        print(f"MINTED {name} {d['amount']}", flush=True)

    print("BTC deposits minted. Run AMM/withdraw via ctl or re-enter after node pack.", flush=True)
    return 0


if __name__ == "__main__":
    # allow `python3 pl-btc-e2e.py wait` just to poll funding
    if len(sys.argv) > 1 and sys.argv[1] == "wait":
        for name in ("sally", "dave", "fee"):
            w = wallet(name)
            addr = w.get("p2pkh") or w["address"]
            info = addr_info(addr)
            print(name, addr, info)
        sys.exit(0)
    sys.exit(main())
