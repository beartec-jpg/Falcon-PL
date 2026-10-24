#!/usr/bin/env python3
"""Permissionless BitVM2 challenger.

Watch instance Kickoffs. Honest = dest-lock vout0 matching an open burn AND
change back to the instance. Anything else: submit RailChallenge and broadcast
the setup-once abort (return to instance) or Challenge (Assert connector).
No watch key. No live n-of-n. No operator fronting.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl"))
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
POLL = int(os.environ.get("BITVM2_CHALLENGE_POLL", "30"))
UA = {"User-Agent": "fpl-bitvm2-challenger/1.0"}
INSTANCE_JSON = Path(
    os.environ.get("BITVM2_INSTANCE_JSON", str(ROOT / "run/bitvm2-instance.json"))
)
CHALLENGE_STATE = Path(
    os.environ.get("BITVM2_CHALLENGE_STATE", str(ROOT / "run/bitvm2-challenger.json"))
)


def log(msg: str) -> None:
    print(msg, flush=True)


def status() -> dict:
    out = subprocess.check_output([CTL, "--addr", RPC, "status"], text=True, timeout=30)
    body = json.loads(out[out.find("{") :])
    return body.get("body") or body


def http_json(url: str) -> dict | list | None:
    req = urllib.request.Request(url, headers=UA)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read())
    except Exception as e:
        log(f"WARN explorer {e}")
        return None


ABORT_SEED = Path(os.environ.get("FPL_ABORT_SEED", str(ROOT / "keys/bitvm-abort-seed")))
CLAIM_CSV = int(os.environ.get("FPL_CLAIM_CSV", "6"))


def sha256(b: bytes) -> bytes:
    import hashlib

    return hashlib.sha256(b).digest()


def note_id_of(frm: str, amount: int, dest: str) -> str:
    return sha256(f"wd:BTC:{frm}:{amount}:{dest}".encode()).hex()


def abort_commit_for(note_id: str) -> bytes:
    seed = ABORT_SEED.read_bytes()[:32]
    pre = sha256(b"fpl-bitvm-abort-v1|" + seed + note_id.encode())
    return sha256(pre)


def dest_h160(addr: str) -> bytes:
    a = addr.strip().lower()
    # testnet P2WPKH tb1q
    import binascii

    if a.startswith("tb1q") or a.startswith("bc1q"):
        # bech32 decode
        charset = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
        raw = a.split("1", 1)[1]
        data = [charset.index(c) for c in raw]
        # drop 6 checksum
        vals = data[:-6]
        # convert 5-bit to 8-bit, skip witness version (first)
        acc = 0
        bits = 0
        out = bytearray()
        for v in vals[1:]:
            acc = (acc << 5) | v
            bits += 5
            if bits >= 8:
                bits -= 8
                out.append((acc >> bits) & 255)
        return bytes(out[:20])
    raise ValueError(f"dest not p2wpkh: {addr}")


def dest_lock_spk_hex(dest: str, note_id: str, csv: int = CLAIM_CSV) -> str:
    commit = abort_commit_for(note_id)
    h160 = dest_h160(dest)
    s = bytearray([0x63, 0xA8, 0x20])
    s.extend(commit)
    s.extend([0x88, 0x51, 0x67])
    if csv <= 16:
        s.append(0x50 + csv)
    else:
        raise ValueError("csv")
    s.extend([0xB2, 0x75, 0x76, 0xA9, 0x14])
    s.extend(h160)
    s.extend([0x88, 0xAC, 0x68])
    return "0020" + sha256(bytes(s)).hex()


def instance_addr() -> str:
    if INSTANCE_JSON.exists():
        try:
            return str(json.loads(INSTANCE_JSON.read_text()).get("address") or "")
        except (OSError, json.JSONDecodeError):
            return ""
    return os.environ.get("BITVM2_INSTANCE_ADDRESS", "")


# #49: highest fee an honest Kickoff may leave (matches MAX_KICKOFF_FEE_SATS in
# bitvm2_instance.rs).
MAX_KICKOFF_FEE_SATS = int(os.environ.get("BITVM2_MAX_KICKOFF_FEE_SATS", "5000"))


def kickoff_shape_alerts(tx: dict, inst: str, want_lock: str, amount: int) -> list[str]:
    """#49: an honest dest-lock Kickoff has exactly 1 or 2 value outputs: vout0 the
    exact dest-lock spk at `amount`, optional vout1 change to the instance, and a
    fee (inputs - outputs) at most MAX_KICKOFF_FEE_SATS. No plain-address fallback."""
    alerts: list[str] = []
    vouts = [v for v in (tx.get("vout") or []) if int(v.get("value") or 0) > 0]
    if not vouts:
        return ["no value outputs"]
    if len(vouts) > 2:
        alerts.append(f"{len(vouts)} value outputs (max dest-lock + change)")
    v0 = vouts[0]
    if not want_lock or str(v0.get("scriptpubkey") or "").lower() != want_lock.lower():
        alerts.append(f"vout0 {v0.get('scriptpubkey_address')} is not the dest-lock")
    if amount and int(v0.get("value") or 0) != amount:
        alerts.append(f"vout0 amount {int(v0.get('value') or 0)} != burn {amount}")
    if len(vouts) > 1:
        ch = str(vouts[1].get("scriptpubkey_address") or "")
        if not inst or ch.lower() != inst.lower():
            alerts.append(f"change {ch} != instance {inst}")
    in_sum = sum(int((v.get("prevout") or {}).get("value") or 0) for v in (tx.get("vin") or []))
    out_sum = sum(int(v.get("value") or 0) for v in (tx.get("vout") or []))
    fee = in_sum - out_sum
    if in_sum and fee > MAX_KICKOFF_FEE_SATS:
        alerts.append(f"fee/leftover {fee} > {MAX_KICKOFF_FEE_SATS} (change must return to instance)")
    return alerts


def check_claim(c: dict) -> list[str]:
    alerts: list[str] = []
    note = str(c.get("note_id") or "")[:16]
    dest = str(c.get("dest") or "")
    amount = int(c.get("amount") or 0)
    peg = str(c.get("pegout_txid_hex") or "").strip()
    st = str(c.get("status") or "")
    if not peg:
        if st in ("pegout_posted", "claim_posted"):
            alerts.append(f"{note} status={st} but empty pegout txid")
        return alerts
    tx = http_json(f"{EXPLORER}/tx/{peg}")
    if not isinstance(tx, dict):
        alerts.append(f"{note} pegout {peg[:16]} not found on explorer")
        return alerts
    inst = instance_addr()
    vins = tx.get("vin") or []
    spends_instance = False
    if inst:
        for vin in vins:
            prev = str(vin.get("prevout", {}).get("scriptpubkey_address") or "")
            if prev.lower() == inst.lower():
                spends_instance = True
                break
        if not spends_instance:
            return []
    want_lock = ""
    try:
        nid = str(c.get("note_id") or "") or note_id_of(str(c.get("from") or ""), amount, dest)
        want_lock = dest_lock_spk_hex(dest, nid)
    except Exception as e:
        log(f"WARN dest-lock spk {e}")
    for a in kickoff_shape_alerts(tx, inst, want_lock, amount):
        alerts.append(f"{note} {a}")
    return alerts


def broadcast(raw_hex: str) -> str:
    req = urllib.request.Request(
        f"{EXPLORER}/tx",
        data=raw_hex.strip().encode(),
        headers={**UA, "Content-Type": "text/plain"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode().strip()


def maybe_broadcast_challenge(alerts: list[str]) -> None:
    if not alerts:
        return
    inst_path = INSTANCE_JSON
    if not inst_path.exists():
        return
    inst = json.loads(inst_path.read_text(encoding="utf-8"))
    raw = str(
        inst.get("abort_raw_hex")
        or inst.get("challenge_raw_hex")
        or ""
    ).strip()
    abort_path = Path(os.environ.get("BITVM2_ABORT_JSON", str(ROOT / "run/bitvm2-abort.json")))
    if abort_path.exists() and not raw:
        try:
            raw = str(json.loads(abort_path.read_text()).get("raw_hex") or "")
        except (OSError, json.JSONDecodeError):
            raw = ""
    if not raw:
        return
    st: dict = {}
    if CHALLENGE_STATE.exists():
        try:
            st = json.loads(CHALLENGE_STATE.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            st = {}
    if st.get("broadcast_txid") or st.get("broadcast_fail"):
        return
    try:
        txid = broadcast(raw)
    except Exception as e:
        log(f"ALERT challenge broadcast failed: {e}")
        return
    st["broadcast_txid"] = txid
    st["alerts"] = alerts
    st["at"] = time.time()
    CHALLENGE_STATE.parent.mkdir(parents=True, exist_ok=True)
    CHALLENGE_STATE.write_text(json.dumps(st, indent=2) + "\n", encoding="utf-8")
    log(f"CHALLENGE broadcast txid={txid} (presigned abort/Challenge)")


def watch_instance_utxos(rail: dict) -> list[str]:
    addr = instance_addr()
    if not addr:
        return []
    utxos = http_json(f"{EXPLORER}/address/{addr}/utxo")
    if not isinstance(utxos, list):
        return []
    st: dict = {}
    if CHALLENGE_STATE.exists():
        try:
            st = json.loads(CHALLENGE_STATE.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            st = {}
    prev = st.get("utxos") or {}
    now = {f"{u.get('txid')}:{u.get('vout')}": int(u.get("value") or 0) for u in utxos}
    alerts: list[str] = []
    claims = [c for c in (rail.get("bitvm2_claims") or []) if isinstance(c, dict)]
    for key in prev:
        if key in now:
            continue
        txid, _, vout = key.partition(":")
        spend = http_json(f"{EXPLORER}/tx/{txid}/outspend/{vout}") or {}
        spend_txid = str(spend.get("txid") or "")
        dest, amt, v0_spk = "", 0, ""
        stx: dict = {}
        if spend_txid:
            stx = http_json(f"{EXPLORER}/tx/{spend_txid}") or {}
            vouts = [v for v in (stx.get("vout") or []) if int(v.get("value") or 0) > 0]
            if vouts:
                dest = str(vouts[0].get("scriptpubkey_address") or "")
                v0_spk = str(vouts[0].get("scriptpubkey") or "").lower()
                amt = int(vouts[0].get("value") or 0)
        matched = False
        for c in claims:
            cdest = str(c.get("dest") or "")
            camt = int(c.get("amount") or 0)
            if camt != amt:
                continue
            nid = str(c.get("note_id") or "") or note_id_of(
                str(c.get("from") or ""), camt, cdest
            )
            try:
                lock = dest_lock_spk_hex(cdest, nid)
            except Exception:
                lock = ""
            if lock and v0_spk == lock and not kickoff_shape_alerts(stx, addr, lock, camt):
                matched = True
                break
        if matched:
            log(f"HOLD_OUT honest {key} dest-lock amt={amt}")
        else:
            msg = f"unexpected spend {key} → {spend_txid} dest={dest} amt={amt}"
            log(f"ALERT {msg}")
            alerts.append(msg)
    st["utxos"] = now
    CHALLENGE_STATE.parent.mkdir(parents=True, exist_ok=True)
    CHALLENGE_STATE.write_text(json.dumps(st, indent=2) + "\n", encoding="utf-8")
    log(f"instance {addr} utxos={len(now)}")
    return alerts


def once() -> int:
    st = status()
    rail = next((r for r in st.get("rails") or [] if r.get("asset") == "BTC"), {})
    claims = rail.get("bitvm2_claims") or []
    n_alert = 0
    alerts: list[str] = []
    for c in claims:
        if not isinstance(c, dict):
            continue
        for a in check_claim(c):
            log(f"ALERT {a}")
            alerts.append(a)
            n_alert += 1
    alerts.extend(watch_instance_utxos(rail))
    n_alert = len(alerts)
    if n_alert:
        maybe_broadcast_challenge(alerts)
    log(f"challenger scanned n={len(claims)} alerts={n_alert}")
    return n_alert


def main() -> int:
    if "--once" in sys.argv:
        return 1 if once() else 0
    while True:
        try:
            once()
        except Exception as e:
            log(f"WARN {e}")
        time.sleep(POLL)


if __name__ == "__main__":
    raise SystemExit(main())
