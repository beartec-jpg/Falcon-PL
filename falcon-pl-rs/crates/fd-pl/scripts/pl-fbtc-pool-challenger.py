#!/usr/bin/env python3
"""Permissionless FBTC-pool challenger.

Watch the pooled P2TR. If it is spent to a dest/amount that does not match an
open FPL BTC withdraw, submit RailChallenge and broadcast the presigned
return-to-pool abort. Does not steal: abort output is the pool script.

Env:
  FBTC_POOL_JSON / FBTC_POOL_KEYS  (dir with pool.json + abort.presign.json)
  FBTC_ABORT_PRESIGN, FBTC_ABORT_PREIMAGE
  FPL_CTL, FPL_RPC, FPL_KEYS, FPL_NETWORK_ID, FPL_CHALLENGE_ACCOUNT
  BTC_EXPLORER, STATE_PATH, POLL_SEC
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
KEYS = Path(os.environ.get("FBTC_POOL_KEYS", "/home/falcon/falcon-pl-public-testnet-2300/keys/fbtc-pool"))
POOL_JSON = Path(os.environ.get("FBTC_POOL_JSON", str(KEYS / "pool.json")))
PRESIGN = Path(os.environ.get("FBTC_ABORT_PRESIGN", str(KEYS / "abort.presign.json")))
PREIMAGE = Path(os.environ.get("FBTC_ABORT_PREIMAGE", str(KEYS / "abort.preimage")))
CTL = os.environ.get("FPL_CTL", "/home/falcon/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl")
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
STATE_PATH = os.environ.get(
    "STATE_PATH", "/home/falcon/falcon-pl-public-testnet-2300/run/fbtc-pool-challenger.json"
)
POLL_SEC = int(os.environ.get("POLL_SEC", "15"))
UA = {"User-Agent": "fpl-fbtc-pool-challenger/1.0"}


def log(msg: str) -> None:
    print(msg, flush=True)


def http_json(url: str):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode("utf8"))


def http_post(url: str, body: bytes) -> str:
    req = urllib.request.Request(url, data=body, method="POST", headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode("utf8")


def load_state() -> dict:
    try:
        with open(STATE_PATH, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError):
        return {"utxos": {}, "seen_spends": []}


def save_state(st: dict) -> None:
    os.makedirs(os.path.dirname(STATE_PATH) or ".", exist_ok=True)
    tmp = STATE_PATH + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(st, f, indent=2)
        f.write("\n")
    os.replace(tmp, STATE_PATH)


def pool_addr() -> str:
    with open(POOL_JSON, encoding="utf-8") as f:
        return (json.load(f).get("address") or "").strip()


def fpl_btc() -> dict:
    try:
        out = subprocess.check_output([CTL, "--addr", RPC, "status"], text=True, timeout=15)
        i = out.find("{")
        body = json.JSONDecoder().raw_decode(out[i:])[0]
        if isinstance(body, dict) and "rails" not in body:
            body = body.get("body", body)
        for r in body.get("rails") or []:
            if r.get("asset") == "BTC":
                return r
    except Exception as e:
        log(f"WARN fpl status {e}")
    return {}


def withdrawals(btc: dict) -> list:
    w = btc.get("withdrawals") or btc.get("open_withdraw_notes") or []
    if isinstance(w, dict):
        out = []
        for nid, v in w.items():
            if isinstance(v, (list, tuple)) and len(v) >= 2:
                out.append({"note_id": nid, "amount": int(v[0]), "dest": str(v[1])})
            elif isinstance(v, dict):
                out.append(
                    {
                        "note_id": nid,
                        "amount": int(v.get("amount") or 0),
                        "dest": str(v.get("external_to") or v.get("dest") or ""),
                    }
                )
        return out
    return w if isinstance(w, list) else []


def claims(btc: dict) -> list:
    return btc.get("bitvm_claims") or btc.get("bitvm2_claims") or []


def matches_burn(dest: str, amt: int, btc: dict) -> bool:
    d = (dest or "").lower()
    for w in withdrawals(btc):
        if str(w.get("dest") or "").lower() == d and int(w.get("amount") or 0) == amt:
            return True
    for c in claims(btc):
        if str(c.get("dest") or "").lower() == d and int(c.get("amount") or 0) == amt:
            if str(c.get("status") or "pending") in ("pending", "final", ""):
                return True
    return False


def submit_challenge(note_id: str, dest: str, amount: int) -> None:
    keys = os.environ.get("FPL_KEYS", "")
    acct = os.environ.get("FPL_CHALLENGE_ACCOUNT", "alice")
    nid = os.environ.get("FPL_NETWORK_ID", "2300")
    cmd = [
        CTL, "--addr", RPC, "submit-rail-challenge",
        "--account", acct, "--asset", "BTC", "--note-id", note_id,
        "--observed-dest", dest, "--observed-amount", str(amount),
        "--network-id", nid,
    ]
    if keys:
        cmd += ["--keys-dir", keys, "--scheme", os.environ.get("FPL_SCHEME", "hmac")]
    try:
        out = subprocess.check_output(cmd, text=True, timeout=30)
        log(f"CHALLENGE_SUBMIT note={note_id} dest={dest} {out[:200]}")
    except Exception as e:
        log(f"CHALLENGE_SUBMIT_FAIL note={note_id} {e}")


def broadcast_abort() -> None:
    if not PRESIGN.is_file():
        log("ABORT_SKIP no presign json")
        return
    cmd = [CTL, "btc-fbtc-pool-abort", "--keys-dir", str(KEYS)]
    if PRESIGN.is_file():
        cmd += ["--presign-json", str(PRESIGN)]
    try:
        raw = subprocess.check_output(cmd, text=True, timeout=30).strip().splitlines()[-1]
    except Exception as e:
        log(f"ABORT_BUILD_FAIL {e}")
        return
    try:
        txid = http_post(f"{EXPLORER}/tx", raw.encode("ascii"))
        log(f"ABORT_BROADCAST {txid}")
    except Exception as e:
        log(f"ABORT_BROADCAST_FAIL {e}")


def loop() -> None:
    addr = pool_addr()
    if not addr:
        log("FATAL empty pool address")
        sys.exit(2)
    log(f"fbtc-pool-challenger watch={addr} explorer={EXPLORER}")
    st = load_state()
    while True:
        try:
            utxos = http_json(f"{EXPLORER}/address/{addr}/utxo")
            now = {}
            for u in utxos:
                key = f"{u.get('txid')}:{u.get('vout')}"
                now[key] = {"value": int(u.get("value") or 0), "status": u.get("status") or {}}
            prev = st.get("utxos") or {}
            disappeared = [k for k in prev if k not in now]
            appeared = [k for k in now if k not in prev]
            btc = fpl_btc()
            for k in appeared:
                log(f"HOLD_IN {k} sats={now[k]['value']}")
            for k in disappeared:
                txid, vout = k.split(":")
                spend_txid, spend_dest, spend_amt = "?", "", int((prev[k] or {}).get("value") or 0)
                try:
                    outspend = http_json(f"{EXPLORER}/tx/{txid}/outspend/{vout}")
                    spend_txid = (outspend or {}).get("txid") or "?"
                    if spend_txid and spend_txid != "?":
                        stx = http_json(f"{EXPLORER}/tx/{spend_txid}") or {}
                        vouts = stx.get("vout") or []
                        if vouts:
                            spend_dest = vouts[0].get("scriptpubkey_address") or ""
                            spend_amt = int(vouts[0].get("value") or 0)
                except Exception as e:
                    spend_txid = f"lookup-failed:{e}"
                if matches_burn(spend_dest, spend_amt, btc):
                    log(f"HOLD_OUT honest {k} spend={spend_txid} dest={spend_dest} amt={spend_amt}")
                else:
                    log(f"ALERT_UNEXPECTED_SPEND {k} spend={spend_txid} dest={spend_dest} amt={spend_amt}")
                    pending = [c for c in claims(btc) if str(c.get("status") or "pending") == "pending"]
                    note = (pending[0].get("note_id") if pending else "") or "fbtc-pool"
                    submit_challenge(str(note), spend_dest or "unknown", spend_amt)
                    broadcast_abort()
                st.setdefault("seen_spends", []).append(
                    {"utxo": k, "spend": spend_txid, "dest": spend_dest, "amt": spend_amt}
                )
            st["utxos"] = now
            save_state(st)
            log(f"ok utxos={len(now)} new={len(appeared)} gone={len(disappeared)}")
        except Exception as e:
            log(f"WARN {e}")
        time.sleep(POLL_SEC)


if __name__ == "__main__":
    try:
        loop()
    except KeyboardInterrupt:
        sys.exit(0)
