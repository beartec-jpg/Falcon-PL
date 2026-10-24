#!/usr/bin/env python3
"""Operator BTC dest-lock e2e on Falcon PL 2300.

Dave recovery pay → dest20 deposit to FROST P2TR → 6 confs → RailDeposit mint
→ distributed FROST Kickoff dest-lock → CSV=6 take.

Does NOT send to tb1q7dnl… / tb1qesum…. Does NOT flip BTC_RAIL_LIVE.
Does NOT gather t FROST shares onto one disk. Operator funds only.
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
COORD = os.environ.get("FPL_FROST_COORD", str(ROOT / "scripts/pl-btc-frost-coord.sh"))
NETWORK_ID = os.environ.get("FPL_NETWORK_ID", "2300")
SCHEME = os.environ.get("FPL_SCHEME", "falcon-512")
WATCHER = os.environ.get("FPL_WATCHER_ACCOUNT", "watcher-browser")
ABORT_SEED = os.environ.get("FPL_ABORT_SEED", str(KEYS / "bitvm-abort-seed"))
OLD_HOLDS = (
    "tb1q7dnlzumm50hke3yl75rywds3gtfu2swqwu4xa08kx0ctxs00rv4q8hrkwu",
    "tb1qesum",
)
RECOVERY = os.environ.get(
    "FPL_RECOVERY_TX",
    "7040bcb176edc4fb2db34109026eb4b7a3595bca1508ce4bdfeb01f9305d8e83",
)
DEPOSIT_CONFS = int(os.environ.get("FPL_DEPOSIT_CONFS", "6"))
CSV_BLOCKS = int(os.environ.get("FPL_CSV_BLOCKS", "6"))
DEPOSIT_FEE = int(os.environ.get("FPL_DEPOSIT_FEE", "1000"))
KICKOFF_AMOUNT = int(os.environ.get("FPL_KICKOFF_AMOUNT", "50000"))
KICKOFF_FEE = int(os.environ.get("FPL_KICKOFF_FEE", "1000"))
TAKE_FEE = int(os.environ.get("FPL_TAKE_FEE", "1000"))
STATE = Path(os.environ.get("FPL_E2E_STATE", str(ROOT / "run/btc-dest-lock-e2e.json")))
UA = {"User-Agent": "fpl-btc-dest-lock-e2e/2.9.36"}


def log(msg: str) -> None:
    print(msg, flush=True)


def save(st: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(st, indent=2) + "\n")
    tmp.replace(STATE)


def load_state() -> dict:
    try:
        return json.loads(STATE.read_text())
    except (OSError, json.JSONDecodeError):
        return {}


def http(url: str, data: bytes | None = None, timeout: int = 30):
    req = urllib.request.Request(url, data=data, headers=UA)
    if data is not None:
        req.add_header("Content-Type", "text/plain")
    with urllib.request.urlopen(req, timeout=timeout) as r:
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
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.read().decode().strip()
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")[:400]
        raise SystemExit(f"broadcast failed {e.code}: {body}") from e


def tx_status(txid: str) -> dict:
    try:
        return http(f"{EXPLORER}/tx/{txid}") or {}
    except urllib.error.HTTPError:
        return {}


def tip_height() -> int:
    return int(http(f"{EXPLORER}/blocks/tip/height") or 0)


def confirmed(txid: str, need: int) -> bool:
    st = (tx_status(txid).get("status") or {})
    if not st.get("confirmed"):
        return False
    h = int(st.get("block_height") or 0)
    return tip_height() >= h and (tip_height() - h + 1) >= need


def wait_txid(txid: str, need: int, label: str, timeout: int = 14400) -> dict:
    t0 = time.time()
    while time.time() - t0 < timeout:
        t = tx_status(txid)
        st = t.get("status") or {}
        if st.get("confirmed"):
            h = int(st.get("block_height") or 0)
            have = tip_height() - h + 1
            log(f"wait {label} {txid[:16]}… height={h} confs={have} need={need}")
            if have >= need:
                return t
        else:
            log(f"wait {label} {txid[:16]}… unconfirmed tip={tip_height()}")
        time.sleep(20)
    raise SystemExit(f"timeout waiting for {label} {txid}")


def ctl(*args: str) -> str:
    cmd = [CTL, "--addr", RPC, *args]
    log("+ " + " ".join(cmd[:10]))
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


def refuse_old(s: str) -> None:
    low = s.lower()
    for h in OLD_HOLDS:
        if h.lower() in low:
            raise SystemExit(f"refusing retired BTC hold {h}")


def merkle_proof(txid: str) -> tuple[str, int, list[str], str, int]:
    t = tx_status(txid)
    st = t.get("status") or {}
    bh = st.get("block_hash")
    height = int(st.get("block_height") or 0)
    if not bh:
        raise RuntimeError(f"{txid} unconfirmed")
    proof = http(f"{EXPLORER}/tx/{txid}/merkle-proof")
    siblings = proof.get("merkle") or []
    pos = int(proof.get("pos") or 0)
    internal = []
    for s in siblings:
        b = bytes.fromhex(s)
        internal.append(b[::-1].hex())
    hdr = http(f"{EXPLORER}/block/{bh}")
    merkle_root = hdr.get("merkle_root") or ""
    return bh, height, internal, merkle_root, pos


def wait_rail_headers(want: int, timeout: int = 7200) -> None:
    t0 = time.time()
    while time.time() - t0 < timeout:
        tip = int(rail_btc().get("tip_height") or 0)
        log(f"FPL BTC headers tip={tip} want>={want}")
        if tip >= want:
            return
        time.sleep(15)
    raise SystemExit(f"FPL BTC headers stalled want={want}")


def frost_sign(sighash: str) -> str:
    out = subprocess.check_output(["bash", COORD, "sign-message", sighash], text=True, timeout=180)
    lines = [ln.strip() for ln in out.splitlines() if ln.strip()]
    sig = lines[-1]
    if len(sig) != 128 or any(c not in "0123456789abcdefABCDEF" for c in sig):
        raise SystemExit(f"frost-coord did not print 64-byte sig: {sig[:80]}")
    return sig.lower()


def main() -> int:
    st = load_state()
    vp = json.loads((KEYS / "vault-frost-pub.json").read_text())
    vault_addr = vp["address"]
    vault_spk = vp["spk_hex"]
    refuse_old(vault_addr)
    refuse_old(vault_spk)
    dave = json.loads((WALLETS / "dave.json").read_text())
    dave_addr = dave["address"]
    dave_spk = "0014d7c0f3570eb55e131181b1c210c94230f8d8f1ee"
    log(f"vault {vault_addr}")
    log(f"dave  {dave_addr}")

    rec = st.get("recovery") or RECOVERY
    wait_txid(rec, 1, "recovery")
    rec_tx = tx_status(rec)
    dave_sats = int(rec_tx["vout"][0]["value"])
    st["recovery"] = rec
    st["dave_sats"] = dave_sats
    save(st)

    if not st.get("deposit_txid"):
        raw = ctl(
            "btc-deposit-tx",
            "--privhex-file",
            str(WALLETS / "dave.privhex"),
            "--prev-txid",
            rec,
            "--vout",
            "0",
            "--sats",
            str(dave_sats),
            "--dest-account",
            "dave",
            "--vault-spk",
            vault_spk,
            "--prev-spk",
            dave_spk,
            "--fee",
            str(DEPOSIT_FEE),
        ).strip().splitlines()[-1]
        refuse_old(raw)
        dep = broadcast(raw)
        st["deposit_txid"] = dep
        st["deposit_raw"] = raw
        st["deposit_amount"] = dave_sats - DEPOSIT_FEE
        save(st)
        log(f"DEPOSIT_BROADCAST {dep} amount={st['deposit_amount']}")
    wait_txid(st["deposit_txid"], DEPOSIT_CONFS, "deposit")
    bh, height, siblings, merkle, pos = merkle_proof(st["deposit_txid"])
    st["deposit_height"] = height
    st["deposit_block"] = bh
    save(st)
    wait_rail_headers(height + DEPOSIT_CONFS - 1)

    if not st.get("minted"):
        out = ctl(
            "submit-rail-deposit",
            "--account",
            WATCHER,
            "--asset",
            "BTC",
            "--to",
            "dave",
            "--amount",
            str(st["deposit_amount"]),
            "--external-txid",
            st["deposit_txid"],
            "--block-hash",
            bh,
            "--height",
            str(height),
            "--merkle-root",
            merkle,
            "--raw-tx",
            st["deposit_raw"],
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
        log(out[-500:])
        st["minted"] = True
        save(st)

    r = rail_btc()
    minted = int(r.get("total_minted") or 0)
    log(f"BTC minted={minted} burned={r.get('total_burned')} vault={r.get('vault_address')}")
    if minted < st["deposit_amount"]:
        raise SystemExit(f"mint missing: minted={minted} want={st['deposit_amount']}")

    kickoff_utxo = f"{st['deposit_txid']}:0:{st['deposit_amount']}"
    if not st.get("kickoff_raw"):
        unsigned = json.loads(
            ctl(
                "btc-frost-kickoff-unsigned",
                "--utxo",
                kickoff_utxo,
                "--dest",
                dave_addr,
                "--amount",
                str(KICKOFF_AMOUNT),
                "--fee",
                str(KICKOFF_FEE),
                "--vault-spk",
                vault_spk,
                "--from",
                "dave",
                "--abort-seed-file",
                ABORT_SEED,
            )
        )
        log(f"kickoff unsigned note={unsigned.get('note_id')} sighash={unsigned.get('sighash')}")
        st["kickoff_unsigned"] = unsigned
        save(st)
        sig = frost_sign(unsigned["sighash"])
        raw = ctl(
            "btc-frost-kickoff-finish",
            "--utxo",
            kickoff_utxo,
            "--dest",
            dave_addr,
            "--amount",
            str(KICKOFF_AMOUNT),
            "--fee",
            str(KICKOFF_FEE),
            "--vault-spk",
            vault_spk,
            "--from",
            "dave",
            "--abort-seed-file",
            ABORT_SEED,
            "--sig-hex",
            sig,
        ).strip().splitlines()[-1]
        refuse_old(raw)
        st["kickoff_raw"] = raw
        save(st)
        wd = ctl(
            "submit-rail-withdraw",
            "--account",
            "dave",
            "--asset",
            "BTC",
            "--amount",
            str(KICKOFF_AMOUNT),
            "--external-to",
            dave_addr,
            "--signed-btc-tx",
            raw,
            "--scheme",
            SCHEME,
            "--keys-dir",
            str(KEYS),
            "--network-id",
            NETWORK_ID,
        )
        log(wd[-500:])
        st["withdraw"] = wd[-400:]
        save(st)
        kid = broadcast(raw)
        st["kickoff_txid"] = kid
        save(st)
        log(f"KICKOFF_BROADCAST {kid}")

    wait_txid(st["kickoff_txid"], CSV_BLOCKS, "kickoff-csv")
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
            dave_addr,
            "--from",
            "dave",
            "--amount",
            str(KICKOFF_AMOUNT),
            "--abort-seed-file",
            ABORT_SEED,
            "--fee",
            str(TAKE_FEE),
        ).strip().splitlines()[-1]
        refuse_old(take)
        tid = broadcast(take)
        st["take_txid"] = tid
        st["take_raw"] = take
        save(st)
        log(f"TAKE_BROADCAST {tid}")
    wait_txid(st["take_txid"], 1, "take")
    r = rail_btc()
    log(
        "E2E_GREEN "
        f"minted={r.get('total_minted')} burned={r.get('total_burned')} "
        f"deposit={st.get('deposit_txid')} kickoff={st.get('kickoff_txid')} take={st.get('take_txid')}"
    )
    log("BTC_RAIL_LIVE still false — flip only after this print is green.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
