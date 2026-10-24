#!/usr/bin/env python3
"""
Deep cert forensics: 3-lane Falcon.
Compares certificate hash sets + pending + mesh_diag after light load.
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

BIN = os.environ.get("BIN", "/opt/falcon-dag/falcon-dag-rs/target/release/falcon-dagd")
CRYPTO = os.environ.get("CRYPTO", "falcon-512")
N = 3
BASE_RPC = 29800
BASE_P2P = 29900
UNL = ",".join(f"val{i}" for i in range(1, N + 1))
OUT = Path(os.environ.get("OUT_DIR", f"/tmp/cert-forensic-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)
logdir = OUT / "nodes"
logdir.mkdir(exist_ok=True)
pids = []


def log(m):
    print(m, flush=True)
    open(OUT / "run.log", "a").write(m + "\n")


def cleanup(*_a):
    for p in pids:
        try:
            os.kill(p, signal.SIGTERM)
        except ProcessLookupError:
            pass
    time.sleep(0.3)
    for p in pids:
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass
    for i in range(N):
        subprocess.run(["fuser", "-k", f"{BASE_RPC+i}/tcp"], capture_output=True)
        subprocess.run(["fuser", "-k", f"{BASE_P2P+i}/tcp"], capture_output=True)


def rpc(port, method, params=None, timeout=5.0):
    body = json.dumps({"method": method, "params": [params or {}]}).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}",
        data=body,
        headers={"Content-Type": "application/json", "Content-Length": str(len(body))},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def metrics(i):
    d = rpc(BASE_RPC + i, "server_info")
    return ((d.get("result") or {}).get("info") or {}).get("metrics") or {}


def start():
    keys, wallets = logdir / "keys", logdir / "wallets"
    keys.mkdir(exist_ok=True)
    wallets.mkdir(exist_ok=True)
    subprocess.check_call(
        [BIN, "--gen-keys", "--keys-dir", str(keys), "--unl", UNL],
        stdout=subprocess.DEVNULL,
    )
    subprocess.check_call(
        [BIN, "--gen-wallet-keys", "--wallet-dir", str(wallets), "--faucet-accounts", "20"],
        stdout=subprocess.DEVNULL,
    )
    for i in range(N):
        id_ = f"val{i+1}"
        peers = ",".join(f"127.0.0.1:{BASE_P2P+j}" for j in range(N) if j != i)
        data = logdir / id_
        data.mkdir(exist_ok=True)
        env = os.environ.copy()
        env["BULLSHARK_TIEBREAKER"] = "ACCOUNT_ID_LEXICOGRAPHIC"
        cmd = [
            BIN, "--id", id_, "--role", "full",
            "--rpc", f"127.0.0.1:{BASE_RPC+i}",
            "--p2p", f"127.0.0.1:{BASE_P2P+i}",
            "--peers", peers, "--unl", UNL,
            "--worker-index", str(i), "--worker-count", str(N),
            "--network-id", "9200", "--crypto", CRYPTO,
            "--require-tx-sigs", "--auto-sign",
            "--keys-dir", str(keys), "--wallet-dir", str(wallets),
            "--single-packer=false", "--batch-interval", "0.05",
            "--max-batch-txs", "32", "--unl-threshold", "0.6",
            "--max-uncertified-per-creator", "4",
            "--snapshot-interval", "2",
            "--data-dir", str(data),
        ]
        p = subprocess.Popen(
            cmd, stdout=open(logdir / f"{id_}.log", "ab"), stderr=subprocess.STDOUT, env=env
        )
        pids.append(p.pid)
        log(f"started {id_}")
    for i in range(N):
        for _ in range(80):
            try:
                rpc(BASE_RPC + i, "ping", timeout=1)
                break
            except Exception:
                time.sleep(0.1)


def submit(n=30):
    ok = 0
    seq = {f"acct{a}": 1 for a in range(1, 10)}
    accts = list(seq.keys())
    for i in range(n):
        acct = accts[i % len(accts)]
        s = seq[acct]
        d = rpc(
            BASE_RPC,
            "submit",
            {
                "account": acct,
                "sequence": s,
                "destination": "acct20",
                "amount": 1,
                "fee": 10,
            },
        )
        res = d.get("result") or d
        if res.get("engine_result") == "tesSUCCESS":
            ok += 1
            seq[acct] = s + 1
    log(f"submit ok={ok}/{n}")
    return ok


def load_snap(i):
    p = logdir / f"val{i+1}" / "snapshot.json"
    if not p.exists():
        # force save
        try:
            rpc(BASE_RPC + i, "snapshot", {"path": str(logdir / f"val{i+1}")})
        except Exception as e:
            log(f"snapshot rpc val{i+1}: {e}")
        time.sleep(0.5)
    if not p.exists():
        return None
    return json.loads(p.read_text())


def analyze():
    # force snapshots
    for i in range(N):
        try:
            rpc(BASE_RPC + i, "snapshot", {"path": str(logdir / f"val{i+1}")})
        except Exception as e:
            log(f"snap err val{i+1}: {e}")
    time.sleep(1)

    nodes = []
    for i in range(N):
        m = metrics(i)
        snap = load_snap(i)
        certs = []
        batches = []
        if snap:
            for c in snap.get("certificates") or []:
                certs.append(
                    {
                        "batch_hash": c.get("batch_hash", "")[:16],
                        "full": c.get("batch_hash"),
                        "creator": c.get("creator"),
                        "round": c.get("round"),
                        "cert_id": (c.get("cert_id") or "")[:12],
                    }
                )
            for b in snap.get("batches") or []:
                batches.append(
                    {
                        "batch_hash": b.get("batch_hash", "")[:16],
                        "full": b.get("batch_hash"),
                        "creator": b.get("creator"),
                        "n_tx": len(b.get("tx_ids") or b.get("txs") or {}),
                        "parents": [p[:12] for p in (b.get("parents") or [])],
                        "n_sigs": len(b.get("signatures") or {}),
                    }
                )
        md = m.get("mesh_diag") or {}
        pull = m.get("pull") or {}
        nodes.append(
            {
                "id": f"val{i+1}",
                "metrics": {
                    "executed": m.get("executed_txs"),
                    "certified": m.get("certified"),
                    "ordered": m.get("ordered"),
                    "mempool": m.get("mempool"),
                    "batches_created": m.get("batches_created"),
                    "anchors": m.get("bullshark_anchors"),
                    "mesh_diag": md,
                    "pull": {
                        "pending": pull.get("pending_pulls") or pull.get("pending"),
                        "inflight": pull.get("inflight_total"),
                        "retries": pull.get("retries"),
                        "wants_sent": pull.get("wants_sent"),
                        "bodies_served": pull.get("bodies_served"),
                    },
                    "shard_flow": m.get("shard_flow"),
                },
                "cert_set": sorted({c["full"] for c in certs if c.get("full")}),
                "batch_set": sorted({b["full"] for b in batches if b.get("full")}),
                "certs_detail": certs,
                "batches_detail": batches,
            }
        )

    # set diffs
    all_certs = set()
    for n in nodes:
        all_certs |= set(n["cert_set"])
    genesis_like = [h for h in all_certs if h and "genesis" in h.lower()]
    report = {"nodes": [], "union_certs": len(all_certs), "diffs": {}}
    for n in nodes:
        cs = set(n["cert_set"])
        missing = sorted(all_certs - cs)
        extra = []  # none if union
        report["nodes"].append(
            {
                "id": n["id"],
                "n_cert": len(cs),
                "n_batch": len(n["batch_set"]),
                "missing_certs_vs_union": [h[:16] for h in missing],
                "missing_full": missing,
                "metrics": n["metrics"],
                "certs_detail": n["certs_detail"],
                "batches_detail": n["batches_detail"],
            }
        )
        log(
            f"{n['id']}: certs={len(cs)} batches={len(n['batch_set'])} "
            f"exec={n['metrics']['executed']} missing_vs_union={len(missing)} "
            f"pull_pending={n['metrics']['pull'].get('pending')} "
            f"mesh={n['metrics'].get('mesh_diag')}"
        )
        if missing:
            log(f"  MISSING certs: {[h[:16] for h in missing]}")
            # who has body for missing?
            for h in missing:
                holders = []
                for o in nodes:
                    if h in o["batch_set"]:
                        holders.append(o["id"])
                    if h in o["cert_set"]:
                        pass
                log(f"    {h[:16]} body_on={holders}")

    # batch not certified on each node
    all_batches = set()
    for n in nodes:
        all_batches |= set(n["batch_set"])
    for n in nodes:
        uncert = sorted(set(n["batch_set"]) - set(n["cert_set"]))
        if uncert:
            log(f"{n['id']} local batches WITHOUT cert: {[h[:16] for h in uncert]}")
            for h in uncert:
                detail = next((b for b in n["batches_detail"] if b["full"] == h), None)
                log(f"    {detail}")

    # creator histogram of certs
    for n in nodes:
        by_c = {}
        for c in n["certs_detail"]:
            by_c[c["creator"]] = by_c.get(c["creator"], 0) + 1
        log(f"{n['id']} certs_by_creator={by_c}")

    (OUT / "forensic.json").write_text(json.dumps(report, indent=2, default=str))
    log(f"Wrote {OUT}/forensic.json")
    return report


def main():
    signal.signal(signal.SIGTERM, lambda *_: (cleanup(), sys.exit(1)))
    log(f"OUT={OUT} CRYPTO={CRYPTO}")
    start()
    time.sleep(2)
    submit(30)
    time.sleep(20)
    analyze()
    cleanup()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        log(f"FATAL {e}")
        cleanup()
        raise
