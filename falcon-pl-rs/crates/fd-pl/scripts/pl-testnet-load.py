#!/usr/bin/env python3
"""
High-rate wallet load against falcon-pl-node (JSON-TCP wire via ctl or raw).

Uses shared keys dir: signs as w00..wN and alice..dave, submits to one RPC,
tracks sequences locally, reports submit TPS + tip growth.

  python3 pl-testnet-load.py --rpc 127.0.0.1:19101 --keys ~/falcon-pl-testnet/keys
"""
from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import os
import socket
import struct
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path


def sha256_hex(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def load_hmac_secret(keys_dir: Path, account: str) -> str:
    p = keys_dir / f"{account}.json"
    if p.exists():
        d = json.loads(p.read_text())
        sk = bytes.fromhex(d["secret_key"]).decode()
        return sk
    # deterministic fallback matching KeyRing HMAC-dev
    return f"pl-secret:{account}"


def public_for(account: str) -> str:
    return f"pl-pub:{sha256_hex(account.encode())[:16]}"


def sign_tx(account: str, sequence: int, dest: str, amount: int, fee: int, network_id: int, secret: str) -> dict:
    payload = f"pl-tx:v1|{account}|{sequence}|{dest}|{amount}|{fee}|{network_id}"
    sig = hmac.new(secret.encode(), payload.encode(), hashlib.sha256).hexdigest()
    return {
        "account": account,
        "sequence": sequence,
        "destination": dest,
        "amount": amount,
        "fee": fee,
        "network_id": network_id,
        "public_key": public_for(account),
        "signature": sig,
        "tx_id": sha256_hex(payload.encode()),
    }


def rpc(addr: str, msg: dict, timeout: float = 5.0) -> dict:
    host, port_s = addr.rsplit(":", 1)
    port = int(port_s)
    # strip http://
    host = host.replace("http://", "").replace("https://", "")
    data = (json.dumps(msg) + "\n").encode()
    with socket.create_connection((host, port), timeout=timeout) as s:
        s.sendall(data)
        s.settimeout(timeout)
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
            # skip Hello lines
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if not line.strip():
                    continue
                try:
                    obj = json.loads(line.decode())
                except Exception:
                    continue
                if obj.get("type") == "hello":
                    continue
                return obj
    raise RuntimeError(f"no response from {addr}")


def status(addr: str) -> dict:
    r = rpc(addr, {"type": "status_req"})
    return r.get("body") or r


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default="127.0.0.1:19101", help="comma-separated host:port list")
    ap.add_argument("--keys", required=True)
    ap.add_argument("--scheme", default="hmac")
    ap.add_argument("--wallets", type=int, default=32)
    ap.add_argument("--seconds", type=float, default=30.0)
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--amount", type=int, default=1)
    ap.add_argument("--fee", type=int, default=1)
    ap.add_argument("--network-id", type=int, default=9101)
    ap.add_argument("--drain-wait", type=float, default=90.0)
    ap.add_argument(
        "--rate",
        type=float,
        default=0.0,
        help="steady txs/sec (0 = dump/flood max speed)",
    )
    args = ap.parse_args()
    rpcs = [r.strip().replace("http://", "").replace("https://", "") for r in args.rpc.split(",") if r.strip()]

    if args.scheme not in ("hmac", "hmac-sha256-dev", "dev"):
        print("load client currently HMAC-dev only (use PL_SCHEME=hmac for testnet v1)", file=sys.stderr)
        return 2

    keys = Path(args.keys)
    accounts = [f"w{i:02}" for i in range(args.wallets)]
    secrets = {a: load_hmac_secret(keys, a) for a in accounts}
    seqs = {a: 1 for a in accounts}
    seq_lock = threading.Lock()

    primary = rpcs[0]
    st0 = status(primary)
    tip0 = int(st0.get("tip_height") or 0)
    mode = f"steady {args.rate:.0f} tx/s" if args.rate > 0 else "DUMP/flood max"
    print(f"start tip_height={tip0} mempool={st0.get('mempool')} scheme={st0.get('scheme')}")
    print(
        f"load mode={mode} wallets={len(accounts)} workers={args.workers} "
        f"duration={args.seconds}s → {rpcs}"
    )

    ok = 0
    err = 0
    err_lock = threading.Lock()
    stop_at = time.time() + args.seconds
    # Steady rate: global spacing between submits
    rate_interval = (1.0 / args.rate) if args.rate > 0 else 0.0
    next_slot = time.time()
    rate_lock = threading.Lock()

    def worker(wid: int):
        nonlocal ok, err, next_slot
        local_ok = 0
        local_err = 0
        i = wid
        while time.time() < stop_at:
            if rate_interval > 0:
                with rate_lock:
                    now = time.time()
                    if now < next_slot:
                        wait = next_slot - now
                        next_slot += rate_interval
                    else:
                        wait = 0.0
                        next_slot = now + rate_interval
                if wait > 0:
                    time.sleep(wait)
            frm = accounts[i % len(accounts)]
            to = accounts[(i + 1) % len(accounts)]
            i += args.workers
            with seq_lock:
                seq = seqs[frm]
                seqs[frm] = seq + 1
            tx = sign_tx(frm, seq, to, args.amount, args.fee, args.network_id, secrets[frm])
            any_ok = False
            order = rpcs[i % len(rpcs) :] + rpcs[: i % len(rpcs)]
            for j, rpc_addr in enumerate(order):
                try:
                    r = rpc(
                        rpc_addr,
                        {"type": "submit_tx", "tx": tx},
                        timeout=1.5 if j == 0 else 0.8,
                    )
                    if r.get("type") == "ok" or "accept" in str(r.get("msg", "")).lower():
                        any_ok = True
                        if j == 0 and len(rpcs) > 1:
                            try:
                                rpc(order[1], {"type": "submit_tx", "tx": tx}, timeout=0.5)
                            except Exception:
                                pass
                        break
                except Exception:
                    continue
            if any_ok:
                local_ok += 1
            else:
                local_err += 1
        with err_lock:
            ok += local_ok
            err += local_err

    t0 = time.time()
    with ThreadPoolExecutor(max_workers=args.workers) as ex:
        futs = [ex.submit(worker, w) for w in range(args.workers)]
        for f in as_completed(futs):
            f.result()
    elapsed = time.time() - t0

    print("waiting for mempool drain / seals…")
    drain_deadline = time.time() + args.drain_wait
    st1 = status(primary)
    while time.time() < drain_deadline:
        st1 = status(primary)
        mp = int(st1.get("mempool") or 0)
        th = int(st1.get("tip_height") or 0)
        if mp == 0:
            break
        print(f"  … tip={th} mempool={mp}")
        time.sleep(3)
    st1 = status(primary)
    tip1 = int(st1.get("tip_height") or 0)
    submit_tps = ok / elapsed if elapsed > 0 else 0
    chain_delta = tip1 - tip0
    total_wall = elapsed + min(args.drain_wait, time.time() - t0 - elapsed)

    print()
    print("=== load result ===")
    print(f"submit_ok={ok} submit_err={err} elapsed_s={elapsed:.2f} submit_tps={submit_tps:.1f}")
    print(f"tip {tip0} → {tip1} (Δheights={chain_delta}) mempool_now={st1.get('mempool')}")
    print(f"lottery_next={st1.get('lottery_winner_next')} rewards={st1.get('rewards')}")
    if chain_delta > 0:
        # estimate applied txs ≈ sum rewards / fee if fee=1 and mult≈1.2 rough
        print(f"heights/s≈{chain_delta / max(elapsed, 1):.2f} during submit window")
    return 0 if ok > 0 and tip1 > tip0 else 1


if __name__ == "__main__":
    sys.exit(main())
