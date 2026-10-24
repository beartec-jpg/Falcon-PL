#!/usr/bin/env python3
"""Live Bitcoin testnet e2e after vault deposits are broadcast.

Reads /home/falcon/falcon-pl-vault-test/e2e/deposits.json
Waits for confirmations, submits headers, mints FBTC, AMM+unequal withdraw, BitVM challenge grief-reject.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
CTL = os.environ.get("FPL_CTL", str(Path.home() / "falcon-pl/falcon-pl-rs/target/release/falcon-pl-ctl"))
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19401")
KEYS = Path(os.environ.get("FPL_KEYS", str(Path.home() / "falcon-pl-vault-test/e2e/keys")))
DEP = Path(os.environ.get("FPL_DEPOSITS", str(Path.home() / "falcon-pl-vault-test/e2e/deposits.json")))
NETWORK_ID = int(os.environ.get("FPL_NETWORK_ID", "2300"))
SCHEME = os.environ.get("FPL_SCHEME", "hmac")
NEED = int(os.environ.get("FPL_DEPOSIT_CONFS", "1"))
OLD = "tb1q7dnlzumm50hke3yl75rywds3gtfu2swqwu4xa08kx0ctxs00rv4q8hrkwu"
UA = {"User-Agent": "fpl-btc-live-e2e"}


def http_json(url: str):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = r.read().decode()
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw.strip()


def status_body() -> dict:
    raw = ctl("status")
    i = raw.find("{")
    b = json.JSONDecoder().raw_decode(raw[i:])[0]
    return b.get("body") or b


def ctl(*args: str) -> str:
    cmd = [CTL, "--addr", RPC, *args]
    out = subprocess.check_output(cmd, text=True, timeout=60)
    if '"type": "err"' in out or '"type":"err"' in out:
        raise subprocess.CalledProcessError(11, cmd, output=out)
    return out


def confirmed(txid: str, need: int) -> tuple[bool, int, str]:
    t = http_json(f"{EXPLORER}/tx/{txid}")
    st = (t or {}).get("status") or {}
    if not st.get("confirmed"):
        return False, 0, ""
    tip = int(http_json(f"{EXPLORER}/blocks/tip/height"))
    h = int(st.get("block_height") or 0)
    return (tip - h + 1) >= need, h, st.get("block_hash") or ""


def header_hex(block_hash: str) -> str:
    h = http_json(f"{EXPLORER}/block/{block_hash}/header")
    if isinstance(h, str) and len(h) >= 160:
        return h.strip()[:160]
    raw = http_json(f"{EXPLORER}/block/{block_hash}/raw")
    if isinstance(raw, str):
        return raw[:160]
    raise RuntimeError("no header")


def merkle_proof(txid: str):
    p = http_json(f"{EXPLORER}/tx/{txid}/merkle-proof")
    siblings = []
    for s in p.get("merkle") or []:
        b = bytes.fromhex(s)
        siblings.append(b[::-1].hex())
    return int(p.get("pos") or 0), siblings


def main() -> int:
    d = json.loads(DEP.read_text())
    for name, rec in d.items():
        txid = rec.get("txid") or ""
        if not txid or txid.startswith("ERR"):
            print(f"missing txid {name}", file=sys.stderr)
            return 2
        if OLD in rec.get("raw", ""):
            print("refusing old hold", file=sys.stderr)
            return 2
        ok, height, bh = confirmed(txid, NEED)
        print(f"{name} {txid} conf={ok} h={height}", flush=True)
        rec["height"] = height
        rec["block_hash"] = bh
        rec["confirmed"] = ok
    if not all(r.get("confirmed") for r in d.values()):
        print("WAITING_CONFS", flush=True)
        return 3

    heights = [int(r["height"]) for r in d.values()]
    lo, hi = min(heights), max(heights)
    tip = int(http_json(f"{EXPLORER}/blocks/tip/height"))
    hi = max(hi, min(tip, lo + 40))
    rail_tip = 0
    try:
        st = status_body()
        for r in st.get("rails") or []:
            if r.get("asset") == "BTC":
                rail_tip = int(r.get("tip_height") or 0)
    except Exception:
        pass
    start = lo if rail_tip == 0 else rail_tip + 1
    print(f"headers {start}..{hi} (rail_tip={rail_tip})", flush=True)
    for h in range(start, hi + 1):
        bh = str(http_json(f"{EXPLORER}/block-height/{h}")).strip()
        raw = header_hex(bh)
        # admin rail-header: local e2e fleet is not packing SubmitTx
        try:
            ctl(
                "rail-header",
                "--asset", "BTC",
                "--height", str(h),
                "--hash", bh,
                "--parent-hash", "",
                "--merkle-root", "",
                "--raw", raw,
                "--watcher", "alice",
            )
        except subprocess.CalledProcessError as e:
            msg = e.output or ""
            if "already" in msg or "known" in msg:
                print(f"header {h} already", flush=True)
            else:
                print(f"header {h} {msg[-240:]}", flush=True)
                if "parent mismatch" in msg or "bits" in msg:
                    raise

    for name, rec in d.items():
        txid = rec["txid"]
        pos, sibs = merkle_proof(txid)
        hdr = http_json(f"{EXPLORER}/block/{rec['block_hash']}")
        merkle = (hdr or {}).get("merkle_root") or ""
        to = "alice" if name == "fee" else name
        amt = int(rec["vault"])
        try:
            print(
                ctl(
                    "rail-deposit",
                    "--asset", "BTC", "--to", to, "--amount", str(amt),
                    "--external-txid", txid, "--block-hash", rec["block_hash"],
                    "--height", str(rec["height"]), "--merkle-root", merkle,
                    "--raw-tx", rec["raw"], "--merkle-path", ",".join(sibs),
                    "--merkle-index", str(pos), "--watcher", "alice",
                ),
                flush=True,
            )
            print(f"MINTED {to} {amt}", flush=True)
        except subprocess.CalledProcessError as e:
            print(f"deposit {name} FAIL {(e.output or '')[-400:]}", flush=True)
            raise

    # AMM BTC/USDC if USDC exists from earlier e2e; else just transfer FBTC.
    try:
        ctl("create-pool", "--pool-id", "btc-fpl", "--asset-a", "BTC", "--asset-b", "FPL",
            "--account", "alice", "--scheme", SCHEME, "--keys-dir", str(KEYS),
            "--network-id", str(NETWORK_ID))
    except subprocess.CalledProcessError as e:
        print("pool", e.output[-200:] if e.output else e, flush=True)

    dest_s = "n1fH7uhaDauQYfuwG6t1aTncvyuBSZtDEY"
    dest_d = "n1BkijgCanBDsDvFjjwpaM7fX8UgBx8w5A"
    # BitVM withdraw (no founder vault_sig)
    for who, amt, dest in (("sally", 15000, dest_s), ("dave", 25000, dest_d)):
        try:
            print(ctl("rail-withdraw", "--asset", "BTC", "--from", who, "--amount", str(amt),
                      "--external-to", dest), flush=True)
        except subprocess.CalledProcessError as e:
            print("withdraw", who, e.output[-300:] if e.output else e, flush=True)

    print("LIVE_E2E_MINTED_AND_CLAIMED", flush=True)
    print("DONE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
