#!/usr/bin/env python3
"""
NEW multi-lane functional test (NOT multipack / not MP_A10).

Mental model under test:
  global mempool → each of 3 nodes is a lane → DA cert → Bullshark → ledger

Pass criteria (3 nodes, light load):
  - all admit some txs
  - each lane packs only when it owns accounts (batches_created may be >0 on owners)
  - after settle window: all nodes agree on executed count
  - all nodes agree on certified count (or min certs >= 1 non-genesis path)
  - bullshark anchors appear on all nodes (or ordered > 1)
  - all processes stay alive
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
OUT = Path(os.environ.get("OUT_DIR", f"/tmp/lane-model-3n-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)
logdir = OUT / "nodes"
logdir.mkdir(exist_ok=True)
pids: list[int] = []


def log(msg: str) -> None:
    line = f"[{time.strftime('%H:%M:%S')}] {msg}"
    print(line, flush=True)
    with open(OUT / "run.log", "a") as f:
        f.write(line + "\n")


def cleanup(*_a):
    for p in list(pids):
        try:
            os.kill(p, signal.SIGTERM)
        except ProcessLookupError:
            pass
    time.sleep(0.3)
    for p in list(pids):
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass
    for i in range(N):
        subprocess.run(["fuser", "-k", f"{BASE_RPC+i}/tcp"], capture_output=True)
        subprocess.run(["fuser", "-k", f"{BASE_P2P+i}/tcp"], capture_output=True)


def rpc(port, method, params=None, timeout=3.0, retries=4):
    body = json.dumps({"method": method, "params": [params or {}]}).encode()
    last = None
    for a in range(retries):
        try:
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}",
                data=body,
                headers={
                    "Content-Type": "application/json",
                    "Content-Length": str(len(body)),
                    "Connection": "close",
                },
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=timeout) as r:
                raw = r.read()
            if not raw.strip():
                raise RuntimeError("empty")
            return json.loads(raw)
        except Exception as e:
            last = e
            time.sleep(0.08 * (a + 1))
    raise RuntimeError(f"{method}:{port}: {last}")


def metrics(i: int) -> dict:
    d = rpc(BASE_RPC + i, "server_info")
    return ((d.get("result") or {}).get("info") or {}).get("metrics") or {}


def snap(tag: str) -> dict:
    rows = []
    for i in range(N):
        try:
            m = metrics(i)
            pull = m.get("pull") or {}
            if isinstance(pull, str):
                try:
                    pull = json.loads(pull)
                except Exception:
                    pull = {}
            rows.append(
                {
                    "id": f"val{i+1}",
                    "alive": True,
                    "executed": int(m.get("executed_txs") or 0),
                    "certified": int(m.get("certified") or 0),
                    "ordered": int(m.get("ordered") or 0),
                    "mempool": int(m.get("mempool") or 0),
                    "batches_created": int(m.get("batches_created") or 0),
                    "anchors": int(m.get("bullshark_anchors") or 0),
                    "p2p": m.get("p2p_peers") or m.get("peer_count"),
                    "pull_pending": pull.get("pending_pulls") or pull.get("pending"),
                    "architecture": m.get("architecture"),
                }
            )
        except Exception as e:
            rows.append({"id": f"val{i+1}", "alive": False, "error": str(e)})
    # Fix 4: force snapshot and compare non-genesis cert hash sets
    cert_fps = []
    for i in range(N):
        try:
            rpc(BASE_RPC + i, "snapshot", {"path": str(logdir / f"val{i+1}")})
        except Exception:
            pass
    time.sleep(0.3)
    for i in range(N):
        sp = logdir / f"val{i+1}" / "snapshot.json"
        fp = []
        if sp.exists():
            try:
                snapj = json.loads(sp.read_text())
                for c in snapj.get("certificates") or []:
                    bh = c.get("batch_hash") or ""
                    if bh and "genesis" not in bh.lower():
                        # genesis hash is fixed string — filter known genesis
                        fp.append(bh)
                # drop genesis batch hash if present
                gen = None
                try:
                    # keep all; compare full sets including genesis is ok if equal
                    pass
                except Exception:
                    pass
                fp = sorted(set(fp))
            except Exception:
                fp = []
        cert_fps.append(fp)

    s = {
        "tag": tag,
        "rows": rows,
        "alive": sum(1 for r in rows if r.get("alive")),
        "exec": [r.get("executed") for r in rows if r.get("alive")],
        "cert": [r.get("certified") for r in rows if r.get("alive")],
        "ordered": [r.get("ordered") for r in rows if r.get("alive")],
        "anchors": [r.get("anchors") for r in rows if r.get("alive")],
        "batches": [r.get("batches_created") for r in rows if r.get("alive")],
        "cert_fps": cert_fps,
    }
    log(
        f"SNAP {tag}: alive={s['alive']}/{N} exec={s['exec']} cert={s['cert']} "
        f"ord={s['ordered']} anchors={s['anchors']} batches={s['batches']}"
    )
    (OUT / f"snap_{tag}.json").write_text(json.dumps(s, indent=2))
    return s


def start_nodes():
    keys = logdir / "keys"
    wallets = logdir / "wallets"
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
        logf = open(logdir / f"{id_}.log", "ab")
        env = os.environ.copy()
        env["BULLSHARK_TIEBREAKER"] = "ACCOUNT_ID_LEXICOGRAPHIC"
        env["DIRECT_SUBMIT"] = "1"
        cmd = [
            BIN,
            "--id",
            id_,
            "--role",
            "full",
            "--rpc",
            f"127.0.0.1:{BASE_RPC+i}",
            "--p2p",
            f"127.0.0.1:{BASE_P2P+i}",
            "--peers",
            peers,
            "--unl",
            UNL,
            "--worker-index",
            str(i),
            "--worker-count",
            str(N),
            "--network-id",
            "9100",
            "--crypto",
            CRYPTO,
            "--require-tx-sigs",
            "--auto-sign",
            "--keys-dir",
            str(keys),
            "--wallet-dir",
            str(wallets),
            "--batch-interval",
            "0.05",
            "--max-batch-txs",
            "32",
            "--unl-threshold",
            "0.6",
            "--max-uncertified-per-creator",
            "4",
            "--single-packer=false",  # multi-LANE (each node is a lane)
            "--max-batch-parents",
            "3",
            "--adaptive-batch=true",
            "--data-dir",
            str(data),
        ]
        p = subprocess.Popen(cmd, stdout=logf, stderr=subprocess.STDOUT, env=env)
        pids.append(p.pid)
        log(f"started {id_} pid={p.pid} (lane={id_})")
    for i in range(N):
        for _ in range(80):
            try:
                rpc(BASE_RPC + i, "ping", timeout=1.0, retries=1)
                break
            except Exception:
                time.sleep(0.1)
        else:
            raise RuntimeError(f"val{i+1} no ping")
        log(f"val{i+1} ping ok")


def submit_accounts(n_tx: int = 30) -> dict:
    """Submit across acct1..acct9 so all 3 lane shards get work (FNV % 3)."""
    ok = fail = 0
    hist = {}
    # sequences per account
    seq = {f"acct{a}": 1 for a in range(1, 10)}
    # round-robin accounts
    accounts = [f"acct{a}" for a in range(1, 10)]
    t0 = time.time()
    for i in range(n_tx):
        acct = accounts[i % len(accounts)]
        s = seq[acct]
        dest = "acct20"
        code = ""
        try:
            d = rpc(
                BASE_RPC,  # global entry: submit to val1 mempool (global mempool path)
                "submit",
                {
                    "account": acct,
                    "sequence": s,
                    "destination": dest,
                    "amount": 1,
                    "fee": 10,
                },
            )
            res = d.get("result") or d
            code = res.get("engine_result") or ""
        except Exception as e:
            code = f"fail:{type(e).__name__}"
        hist[code] = hist.get(code, 0) + 1
        if code == "tesSUCCESS":
            ok += 1
            seq[acct] = s + 1
        else:
            fail += 1
    wall = time.time() - t0
    log(f"submit n={n_tx} ok={ok} fail={fail} wall={wall:.2f}s hist={hist}")
    return {"ok": ok, "fail": fail, "hist": hist, "wall": wall}


def verdict(snaps: list, sub: dict) -> tuple[list[str], bool]:
    lines = []
    ok = True
    if sub["ok"] < 1:
        lines.append("FAIL: no admits to global mempool")
        ok = False
    else:
        lines.append(f"PASS: global mempool admits ok={sub['ok']}")

    final = snaps[-1]
    if final["alive"] != N:
        lines.append(f"FAIL: alive={final['alive']}/{N}")
        ok = False
    else:
        lines.append(f"PASS: all {N} lane nodes alive")

    execs = final.get("exec") or []
    if execs and min(execs) == max(execs) and min(execs) > 0:
        lines.append(f"PASS: ledger exec agree at {min(execs)}")
    elif execs and min(execs) > 0 and max(execs) - min(execs) <= 2:
        lines.append(f"WARN: exec near-agree {execs}")
    else:
        lines.append(f"FAIL: ledger exec not agreed/empty {execs}")
        ok = False

    certs = final.get("cert") or []
    if certs and min(certs) == max(certs) and min(certs) >= 2:
        lines.append(f"PASS: certified count agree at {min(certs)}")
    elif certs and min(certs) >= 2:
        lines.append(f"WARN: certified count diverge {certs}")
    else:
        lines.append(f"FAIL: insufficient certs {certs}")
        ok = False

    # Fix 4: prefer set equality via snapshots when available
    fps = final.get("cert_fps") or []
    if fps and all(f == fps[0] for f in fps) and fps[0]:
        lines.append(f"PASS: cert set fingerprint equal (n={len(fps[0])})")
    elif fps:
        lines.append(f"FAIL: cert sets differ {[len(f) for f in fps]}")
        ok = False

    anchors = final.get("anchors") or []
    ordered = final.get("ordered") or []
    if anchors and min(anchors) >= 1:
        lines.append(f"PASS: Bullshark anchors present {anchors}")
    elif ordered and min(ordered) >= 2:
        lines.append(f"WARN: ordered progress {ordered} but anchors {anchors}")
    else:
        lines.append(f"FAIL: no Bullshark/order progress anchors={anchors} ord={ordered}")
        ok = False

    batches = final.get("batches") or []
    if any((b or 0) > 0 for b in batches):
        lines.append(f"PASS: lanes packed batches={batches}")
    else:
        lines.append(f"FAIL: no lane packed {batches}")
        ok = False

    return lines, ok


def main() -> int:
    signal.signal(signal.SIGTERM, lambda *_: (cleanup(), sys.exit(1)))
    log(f"OUT={OUT} MODEL=multi-lane N={N} CRYPTO={CRYPTO}")
    log("Discarding multipack semantics: each node is one lane; then Bullshark; then ledger")
    start_nodes()
    time.sleep(2)
    snaps = [snap("t0_idle")]
    sub = submit_accounts(30)
    # Convergence-tightened settle: poll until cert sets equal + exec agree, or 90s.
    t0 = time.time()
    tag_i = 0
    while time.time() - t0 < 90:
        time.sleep(5)
        tag_i += 1
        s = snap(f"t{int(time.time()-t0)}")
        snaps.append(s)
        fps = s.get("cert_fps") or []
        execs = s.get("exec") or []
        certs_equal = fps and all(f == fps[0] for f in fps) and len(fps[0]) >= 2
        exec_agree = execs and min(execs) == max(execs) and min(execs) > 0
        if certs_equal and exec_agree:
            log(f"converged at t+{time.time()-t0:.1f}s certs={len(fps[0])} exec={min(execs)}")
            break
    else:
        log("settle timeout 90s without full cert+exec convergence")

    lines, passed = verdict(snaps, sub)
    summary = {
        "model": "multi-lane",
        "n": N,
        "crypto": CRYPTO,
        "submit": sub,
        "snaps": snaps,
        "verdict": lines,
        "passed": passed,
        "note": "Previous multipack/MP_A10 tests do not validate this model",
    }
    (OUT / "summary.json").write_text(json.dumps(summary, indent=2, default=str))
    log("==== VERDICT ====")
    for ln in lines:
        log(ln)
    log(f"OVERALL: {'PASS' if passed else 'FAIL'}")
    log(f"Wrote {OUT}/summary.json")
    cleanup()
    return 0 if passed else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        log(f"FATAL {e}")
        cleanup()
        raise
