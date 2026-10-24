#!/usr/bin/env python3
"""Permissionless BitVM challenger — watch the pool, abort unmatched spends.

Compares Bitcoin spends of the vault to FPL bitvm_claims. If dest/amount
do not match a pending claim, submits RailChallenge so the protocol
publishes that claim's abort preimage (no COMPLETE file).

Env:
  BTC_EXPLORER, WATCH_ADDRESS / VAULT_PUB, FPL_CTL, FPL_RPC
  FPL_KEYS, FPL_NETWORK_ID, FPL_CHALLENGE_ACCOUNT (default alice)
  STATE_PATH, POLL_SEC
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.request

EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
def _default_watch() -> str:
    p = os.environ.get("VAULT_PUB", "/home/falcon/falcon-pl-vault-test/keys/vault-pub.json")
    try:
        with open(p, encoding="utf-8") as f:
            addr = json.load(f).get("address") or ""
            if addr:
                return addr
    except (OSError, json.JSONDecodeError):
        pass
    return "tb1qesum00x0jm6w2a0dt5vksckhyt45430c0yg5sj"


WATCH = os.environ.get("WATCH_ADDRESS", _default_watch())
OLD_HOLD = "tb1q7dnlzumm50hke3yl75rywds3gtfu2swqwu4xa08kx0ctxs00rv4q8hrkwu"
CTL = os.environ.get("FPL_CTL", "/home/falcon/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl")
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
STATE_PATH = os.environ.get(
    "STATE_PATH", "/home/falcon/falcon-pl-public-testnet-2300/run/btc-challenger.json"
)
POLL_SEC = int(os.environ.get("POLL_SEC", "30"))


def http_json(url: str):
    req = urllib.request.Request(url, headers={"User-Agent": "fpl-btc-challenger/2.9.35"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode("utf-8"))


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
    except Exception:
        return {}
    return {}


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
        print(f"CHALLENGE_SUBMIT note={note_id} dest={dest} {out[:200]}", flush=True)
    except Exception as e:
        print(f"CHALLENGE_SUBMIT_FAIL note={note_id} {e}", flush=True)


def loop() -> None:
    st = load_state()
    if WATCH == OLD_HOLD:
        print("ALERT_REFUSING_OLD_HOLD", OLD_HOLD, flush=True)
        sys.exit(2)
    print(f"btc-challenger watch={WATCH} explorer={EXPLORER}", flush=True)
    while True:
        try:
            utxos = http_json(f"{EXPLORER}/address/{WATCH}/utxo")
            now = {}
            for u in utxos:
                key = f"{u.get('txid')}:{u.get('vout')}"
                now[key] = {
                    "value": int(u.get("value") or 0),
                    "status": u.get("status") or {},
                }
            prev = st.get("utxos") or {}
            disappeared = [k for k in prev if k not in now]
            appeared = [k for k in now if k not in prev]
            btc = fpl_btc()
            open_wd = int(btc.get("open_withdrawals") or 0)
            claims = btc.get("bitvm_claims") or []
            for k in appeared:
                print(f"HOLD_IN {k} sats={now[k]['value']}", flush=True)
            for k in disappeared:
                prev_v = prev[k]
                txid = k.split(":")[0]
                spend_txid = "?"
                spend_dest = ""
                spend_amt = int(prev_v.get("value") or 0)
                try:
                    outspend = http_json(f"{EXPLORER}/tx/{txid}/outspend/{k.split(':')[1]}")
                    spend_txid = (outspend or {}).get("txid") or "?"
                    if spend_txid and spend_txid != "?":
                        stx = http_json(f"{EXPLORER}/tx/{spend_txid}") or {}
                        vouts = stx.get("vout") or []
                        if vouts:
                            spend_dest = vouts[0].get("scriptpubkey_address") or ""
                            spend_amt = int(vouts[0].get("value") or 0)
                except Exception as e:
                    spend_txid = f"lookup-failed:{e}"
                matched = None
                for c in claims:
                    if str(c.get("dest") or "").lower() == spend_dest.lower() and int(c.get("amount") or 0) == spend_amt:
                        matched = c
                        break
                if matched is None:
                    print(
                        f"ALERT_UNEXPECTED_SPEND {k} spend={spend_txid} dest={spend_dest} amt={spend_amt}",
                        flush=True,
                    )
                    pending = [c for c in claims if c.get("status") == "pending"]
                    if pending:
                        c0 = pending[0]
                        submit_challenge(c0.get("note_id") or "", spend_dest or "unknown", spend_amt)
                else:
                    print(
                        f"HOLD_OUT {k} spend={spend_txid} claim={matched.get('note_id')} dest={spend_dest}",
                        flush=True,
                    )
                seen = st.setdefault("seen_spends", [])
                seen.append({"utxo": k, "spend": spend_txid, "dest": spend_dest, "matched": bool(matched)})
            st["utxos"] = now
            save_state(st)
            print(
                f"ok utxos={len(now)} open_withdrawals={open_wd} claims={len(claims)} new={len(appeared)} gone={len(disappeared)}",
                flush=True,
            )
        except Exception as e:
            print(f"WARN {e}", flush=True)
        time.sleep(POLL_SEC)


if __name__ == "__main__":
    try:
        loop()
    except KeyboardInterrupt:
        sys.exit(0)
