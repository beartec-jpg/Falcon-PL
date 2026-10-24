#!/usr/bin/env python3
"""Continue 2300 BTC dest-lock e2e after dest20 deposit + pre-signed Kickoff.

Waits 6 BTC confs, mints, submit-rail-withdraw with the pre-signed Kickoff,
broadcasts Kickoff, waits CSV=6, dave take. Does not flip BTC_RAIL_LIVE.
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
ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl"))
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
KEYS = Path(os.environ.get("FPL_KEYS", str(ROOT / "keys")))
WALLETS = Path(os.environ.get("FPL_WALLETS", str(Path.home() / "falcon-pl-vault-test/btc-wallets")))
NETWORK_ID = os.environ.get("FPL_NETWORK_ID", "2300")
SCHEME = os.environ.get("FPL_SCHEME", "falcon-512")
WATCHER = os.environ.get("FPL_WATCHER_ACCOUNT", "watcher-browser")
ABORT_SEED = os.environ.get("FPL_ABORT_SEED", str(KEYS / "bitvm-abort-seed"))
STATE = Path(os.environ.get("FPL_E2E_STATE", str(ROOT / "run/btc-dest-lock-e2e.json")))
DEPOSIT = "c31e1894f9ef0efa019e9a40b44343ac6fe4f7920824f1aca5e9c55086e3d11a"
DEPOSIT_CONFS = 6
CSV_BLOCKS = 6
KICKOFF_AMOUNT = 50000
TAKE_FEE = 1000
DAVE_ADDR = "tb1q6lq0x4cwk40pxyvpk8pppj2zxrud3u0wcntz7l"
UA = {"User-Agent": "fpl-btc-dest-lock-continue/2.9.36"}
OLD = "tb1q7dnl"


def log(msg: str) -> None:
    print(msg, flush=True)


def save(st: dict) -> None:
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(st, indent=2) + "\n")
    tmp.replace(STATE)


def http(url: str):
    req = urllib.request.Request(url, headers=UA)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read().decode()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise
    if not raw:
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw


def tip() -> int:
    return int(http(f"{EXPLORER}/blocks/tip/height"))


def tx(txid: str) -> dict:
    return http(f"{EXPLORER}/tx/{txid}") or {}


def confs(txid: str) -> int:
    st = (tx(txid).get("status") or {})
    if not st.get("confirmed"):
        return 0
    h = int(st.get("block_height") or 0)
    return tip() - h + 1


def wait_confs(txid: str, need: int, label: str) -> None:
    while True:
        n = confs(txid)
        t = tip()
        log(f"wait {label} {txid[:16]}… confs={n} need={need} tip={t}")
        if n >= need:
            return
        time.sleep(15)


def ctl(*args: str) -> str:
    cmd = [CTL, "--addr", RPC, *args]
    log("+ " + " ".join(str(a)[:48] for a in cmd[:12]))
    return subprocess.check_output(cmd, text=True, timeout=90)


def rail_btc() -> dict:
    raw = ctl("status")
    i = raw.find("{")
    body = json.JSONDecoder().raw_decode(raw[i:])[0]
    body = body.get("body") or body
    for r in body.get("rails") or []:
        if r.get("asset") == "BTC":
            return r
    raise SystemExit("no BTC rail")


def wait_headers(want: int) -> None:
    while True:
        t = int(rail_btc().get("tip_height") or 0)
        log(f"FPL BTC headers tip={t} want>={want}")
        if t >= want:
            return
        time.sleep(10)


def wait_rail(field: str, want: int, label: str, timeout: int = 120) -> dict:
    t0 = time.time()
    while time.time() - t0 < timeout:
        r = rail_btc()
        got = int(r.get(field) or 0)
        log(f"wait {label} {field}={got} want>={want}")
        if got >= want:
            return r
        time.sleep(2)
    raise SystemExit(f"timeout {label} {field} want={want}")


def broadcast(raw_hex: str) -> str:
    if OLD in raw_hex.lower():
        raise SystemExit("refusing retired hold")
    req = urllib.request.Request(
        f"{EXPLORER}/tx",
        data=raw_hex.encode(),
        headers={**UA, "Content-Type": "text/plain"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.read().decode().strip()
    except urllib.error.HTTPError as e:
        raise SystemExit(f"broadcast {e.code}: {e.read().decode(errors='replace')[:400]}") from e


def merkle(txid: str):
    t = tx(txid)
    st = t.get("status") or {}
    bh = st.get("block_hash")
    height = int(st.get("block_height") or 0)
    proof = http(f"{EXPLORER}/tx/{txid}/merkle-proof")
    siblings = [(bytes.fromhex(s)[::-1].hex()) for s in (proof.get("merkle") or [])]
    pos = int(proof.get("pos") or 0)
    hdr = http(f"{EXPLORER}/block/{bh}")
    return bh, height, siblings, hdr.get("merkle_root") or "", pos


def main() -> int:
    st = json.loads(STATE.read_text()) if STATE.exists() else {}
    st["deposit_txid"] = st.get("deposit_txid") or DEPOSIT
    kickoff_raw = (ROOT / "run/btc-kickoff.hex").read_text().strip().splitlines()[-1]
    unsigned = json.loads((ROOT / "run/btc-kickoff-unsigned.json").read_text())
    st["kickoff_prepared"] = True
    st["kickoff_note"] = unsigned.get("note_id")
    save(st)
    if OLD in kickoff_raw.lower():
        raise SystemExit("kickoff references retired hold")

    wait_confs(st["deposit_txid"], DEPOSIT_CONFS, "deposit")
    bh, height, siblings, merkle_root, pos = merkle(st["deposit_txid"])
    st["deposit_height"] = height
    st["deposit_block"] = bh
    save(st)
    wait_headers(height + DEPOSIT_CONFS - 1)

    if not st.get("minted"):
        dep_raw = st.get("deposit_raw") or ""
        if not dep_raw:
            raise SystemExit("missing deposit_raw")
        out = ctl(
            "submit-rail-deposit",
            "--account",
            WATCHER,
            "--asset",
            "BTC",
            "--to",
            "dave",
            "--amount",
            str(st.get("deposit_amount") or 193000),
            "--external-txid",
            st["deposit_txid"],
            "--block-hash",
            bh,
            "--height",
            str(height),
            "--merkle-root",
            merkle_root,
            "--raw-tx",
            dep_raw,
            "--merkle-path",
            ",".join(siblings),
            "--merkle-index",
            str(pos),
            "--scheme",
            SCHEME,
            "--keys-dir",
            str(KEYS),
            "--network-id",
            NETWORK_ID,
        )
        log(out[-600:])
        st["minted"] = True
        st["mint_out"] = out[-400:]
        save(st)
        dave = ctl("account", "--account", "dave")
        log("dave after mint " + dave[:500])

    r = wait_rail("total_minted", int(st.get("deposit_amount") or 193000), "mint-pack")
    log(f"BTC minted={r.get('total_minted')} burned={r.get('total_burned')}")

    if not st.get("withdraw_ok"):
        wd = ctl(
            "submit-rail-withdraw",
            "--account",
            "dave",
            "--asset",
            "BTC",
            "--amount",
            str(KICKOFF_AMOUNT),
            "--external-to",
            DAVE_ADDR,
            "--signed-btc-tx",
            kickoff_raw,
            "--scheme",
            SCHEME,
            "--keys-dir",
            str(KEYS),
            "--network-id",
            NETWORK_ID,
        )
        log(wd[-600:])
        i = wd.find("{")
        if i >= 0:
            body = json.JSONDecoder().raw_decode(wd[i:])[0]
            if str(body.get("type") or "").lower() == "err":
                raise SystemExit(f"withdraw failed: {wd[-400:]}")
        st["withdraw_ok"] = True
        st["withdraw_out"] = wd[-400:]
        save(st)
        wait_rail("total_burned", KICKOFF_AMOUNT, "withdraw-pack")

    if not st.get("kickoff_txid"):
        kid = broadcast(kickoff_raw)
        st["kickoff_txid"] = kid
        save(st)
        log(f"KICKOFF_BROADCAST {kid}")

    wait_confs(st["kickoff_txid"], CSV_BLOCKS, "kickoff-csv")
    if not st.get("take_txid"):
        take = ctl(
            "btc-dest-lock-take",
            "--privhex-file",
            str(WALLETS / "dave.privhex"),
            "--prev-txid",
            st["kickoff_txid"],
            "--vout",
            "0",
            "--sats",
            str(KICKOFF_AMOUNT),
            "--dest",
            DAVE_ADDR,
            "--from",
            "dave",
            "--amount",
            str(KICKOFF_AMOUNT),
            "--abort-seed-file",
            ABORT_SEED,
            "--fee",
            str(TAKE_FEE),
        ).strip().splitlines()[-1]
        tid = broadcast(take)
        st["take_txid"] = tid
        st["take_raw"] = take
        save(st)
        log(f"TAKE_BROADCAST {tid}")
    wait_confs(st["take_txid"], 1, "take")
    r = rail_btc()
    log(
        "E2E_GREEN "
        f"minted={r.get('total_minted')} burned={r.get('total_burned')} "
        f"deposit={st.get('deposit_txid')} kickoff={st.get('kickoff_txid')} take={st.get('take_txid')}"
    )
    log("BTC_RAIL_LIVE still false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
