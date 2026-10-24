#!/usr/bin/env python3
"""
Harder 3-node multi-lane load ladder on baseline (Bullshark-only order).

Levels (default): 30, 100, 200, 400, 800 txs — each as a burst, then settle.
On any fail: dump forensic pack (metrics, cert_fps, pack_skip, pull, log tails).

Env:
  BIN, CRYPTO, OUT_DIR, LEVELS="30,100,200,400,800", SETTLE_S=60, ACCOUNTS=12
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
OUT = Path(os.environ.get("OUT_DIR", f"/tmp/lane-3n-load-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)
logdir = OUT / "nodes"
logdir.mkdir(exist_ok=True)
LEVELS = [int(x) for x in os.environ.get("LEVELS", "30,100,200,400,800").split(",") if x.strip()]
SETTLE_S = float(os.environ.get("SETTLE_S", "60"))
ACCOUNTS = int(os.environ.get("ACCOUNTS", "12"))
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
    time.sleep(0.4)
    for p in list(pids):
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass
    for i in range(N):
        subprocess.run(["fuser", "-k", f"{BASE_RPC+i}/tcp"], capture_output=True)
        subprocess.run(["fuser", "-k", f"{BASE_P2P+i}/tcp"], capture_output=True)


def rpc(port, method, params=None, timeout=4.0, retries=4):
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
    d = rpc(BASE_RPC + i, "server_info", timeout=3.0)
    return ((d.get("result") or {}).get("info") or {}).get("metrics") or {}


def rich_snap(tag: str) -> dict:
    rows = []
    fps = []
    for i in range(N):
        try:
            m = metrics(i)
            pull = m.get("pull") or {}
            row = {
                "id": f"val{i+1}",
                "alive": True,
                "executed": int(m.get("executed_txs") or 0),
                "certified": int(m.get("certified") or 0),
                "ordered": int(m.get("ordered") or 0),
                "mempool": int(m.get("mempool") or 0),
                "batches_created": int(m.get("batches_created") or 0),
                "anchors": int(m.get("bullshark_anchors") or 0),
                "max_cert_round": int(m.get("max_cert_round") or 0),
                "p2p": m.get("peer_count") or m.get("p2p_peers"),
                "pull_pending": (pull.get("pending_pulls") if isinstance(pull, dict) else None),
                "pull_retries": (pull.get("retries") if isinstance(pull, dict) else None),
                "bodies_served": (pull.get("bodies_served") if isinstance(pull, dict) else None),
                "pack_skip": m.get("pack_skip_reasons") or {},
                "mesh_diag": m.get("mesh_diag") or m.get("mesh") or {},
            }
            # cert fingerprint if exposed
            fp = m.get("cert_set_fingerprint") or m.get("cert_fps") or m.get("certified_hashes")
            if isinstance(fp, list):
                fps.append(sorted(fp))
            else:
                # try nested
                fps.append([])
            rows.append(row)
        except Exception as e:
            rows.append({"id": f"val{i+1}", "alive": False, "error": str(e)})
            fps.append([])
    # fill fps via dedicated metric keys if empty
    if not any(fps):
        for i in range(N):
            try:
                m = metrics(i)
                # some builds put fingerprint under mesh or architecture fields
                cfp = m.get("cert_fingerprint") or m.get("cert_set")
                if isinstance(cfp, list):
                    fps[i] = sorted(cfp)
            except Exception:
                pass
    execs = [r.get("executed", 0) for r in rows if r.get("alive")]
    certs = [r.get("certified", 0) for r in rows if r.get("alive")]
    ords = [r.get("ordered", 0) for r in rows if r.get("alive")]
    ancs = [r.get("anchors", 0) for r in rows if r.get("alive")]
    out = {
        "tag": tag,
        "t": time.time(),
        "rows": rows,
        "alive": sum(1 for r in rows if r.get("alive")),
        "exec": execs,
        "cert": certs,
        "ordered": ords,
        "anchors": ancs,
        "batches": [r.get("batches_created") for r in rows if r.get("alive")],
        "exec_spread": (max(execs) - min(execs)) if execs else None,
        "cert_fps": fps,
        "fps_equal": len(set(tuple(x) for x in fps if x)) <= 1 if any(fps) else None,
    }
    path = OUT / f"snap_{tag}.json"
    path.write_text(json.dumps(out, indent=2, default=str))
    return out


def forensic_dump(level: int, reason: str, snap: dict, sub: dict | None = None) -> Path:
    """Heavy dump when a level fails."""
    fdir = OUT / f"forensic_L{level}_{int(time.time())}"
    fdir.mkdir(parents=True, exist_ok=True)
    log(f"FORENSIC dump → {fdir} reason={reason}")
    (fdir / "reason.txt").write_text(reason + "\n")
    (fdir / "snap.json").write_text(json.dumps(snap, indent=2, default=str))
    if sub:
        (fdir / "submit.json").write_text(json.dumps(sub, indent=2, default=str))

    # per-node full metrics + log tails
    for i in range(N):
        vid = f"val{i+1}"
        try:
            m = metrics(i)
            (fdir / f"{vid}_metrics.json").write_text(json.dumps(m, indent=2, default=str))
        except Exception as e:
            (fdir / f"{vid}_metrics_err.txt").write_text(str(e))
        # data-dir snapshot if present
        snap_path = logdir / vid / "snapshot.json"
        if snap_path.exists():
            try:
                # copy may be large; write compact summary instead of full falcon sigs
                d = json.loads(snap_path.read_text())
                summary = {
                    "validator_id": d.get("validator_id"),
                    "batches_created": d.get("batches_created"),
                    "n_batches": len(d.get("batches") or []),
                    "n_certs": len(d.get("certificates") or []),
                    "n_ordered": len(d.get("ordered") or []),
                    "n_executed": len(d.get("executed_txs") or []),
                    "n_mempool": len(d.get("mempool") or []),
                    "ordered": d.get("ordered"),
                    "bullshark_anchors": [
                        {
                            "round": a.get("round"),
                            "batch_hash": a.get("batch_hash"),
                            "leader": a.get("leader"),
                        }
                        for a in (d.get("bullshark_anchors") or [])
                        if isinstance(a, dict)
                    ],
                    "bullshark_last_anchor": d.get("bullshark_last_anchor"),
                    "batch_index": [
                        {
                            "batch_hash": b.get("batch_hash"),
                            "creator": b.get("creator"),
                            "parents": b.get("parents"),
                            "ntx": len(b.get("tx_ids") or []),
                            "nsigs": len(b.get("signatures") or {}),
                            "signers": list((b.get("signatures") or {}).keys()),
                        }
                        for b in (d.get("batches") or [])
                    ],
                    "cert_index": [
                        {
                            "batch_hash": c.get("batch_hash"),
                            "creator": c.get("creator"),
                            "round": c.get("round"),
                            "signers": c.get("signers"),
                            "parents": c.get("parents"),
                        }
                        for c in (d.get("certificates") or [])
                    ],
                }
                (fdir / f"{vid}_dag_summary.json").write_text(
                    json.dumps(summary, indent=2, default=str)
                )
            except Exception as e:
                (fdir / f"{vid}_dag_err.txt").write_text(str(e))
        lp = logdir / f"{vid}.log"
        if lp.exists():
            (fdir / f"{vid}.log.tail").write_text(lp.read_text(errors="replace")[-8000:])

    # process liveness
    try:
        ps = subprocess.check_output(["ps", "aux"], text=True)
        lines = [ln for ln in ps.splitlines() if "falcon-dagd" in ln and "grep" not in ln]
        (fdir / "processes.txt").write_text("\n".join(lines) + "\n")
    except Exception as e:
        (fdir / "processes_err.txt").write_text(str(e))

    # cross-node diff notes
    notes = []
    rows = snap.get("rows") or []
    alive = [r for r in rows if r.get("alive")]
    if alive:
        execs = [r["executed"] for r in alive]
        certs = [r["certified"] for r in alive]
        if max(execs) != min(execs):
            notes.append(f"exec desync {execs}")
        if max(certs) != min(certs):
            notes.append(f"cert count desync {certs}")
        for r in alive:
            pp = r.get("pull_pending")
            if pp and int(pp) > 0:
                notes.append(f"{r['id']} pull_pending={pp} retries={r.get('pull_retries')}")
            skips = r.get("pack_skip") or {}
            hot = {k: v for k, v in skips.items() if isinstance(v, (int, float)) and v > 50}
            if hot:
                notes.append(f"{r['id']} pack_skip_hot={hot}")
    (fdir / "NOTES.txt").write_text("\n".join(notes) + "\n")
    log(f"FORENSIC notes: {notes}")
    return fdir


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
        [
            BIN,
            "--gen-wallet-keys",
            "--wallet-dir",
            str(wallets),
            "--faucet-accounts",
            "20",
        ],
        stdout=subprocess.DEVNULL,
    )
    for i in range(N):
        id_ = f"val{i+1}"
        peers = ",".join(f"127.0.0.1:{BASE_P2P+j}" for j in range(N) if j != i)
        data = logdir / id_
        data.mkdir(exist_ok=True)
        logf = open(logdir / f"{id_}.log", "ab")
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
            "64",
            "--max-batch-parents",
            "3",
            "--unl-threshold",
            "0.6",
            "--max-uncertified-per-creator",
            "8",
            "--single-packer=false",
            "--adaptive-batch=true",
            "--data-dir",
            str(data),
        ]
        env = os.environ.copy()
        env["BULLSHARK_TIEBREAKER"] = "ACCOUNT_ID_LEXICOGRAPHIC"
        env["DIRECT_SUBMIT"] = "1"
        p = subprocess.Popen(cmd, stdout=logf, stderr=subprocess.STDOUT, env=env)
        pids.append(p.pid)
        log(f"started {id_} pid={p.pid}")
        # ping
        for _ in range(80):
            try:
                rpc(BASE_RPC + i, "ping", timeout=1.0, retries=1)
                break
            except Exception:
                time.sleep(0.1)
        else:
            raise RuntimeError(f"{id_} no ping")
        log(f"{id_} ping ok")


def submit_burst(n_tx: int, label: str) -> dict:
    n_accts = max(1, min(ACCOUNTS, 19))
    seq = {f"acct{a}": 1 for a in range(1, n_accts + 1)}
    # continue sequences across levels via file
    seq_path = OUT / "seq.json"
    if seq_path.exists():
        seq.update(json.loads(seq_path.read_text()))
    plan = []
    tmp = dict(seq)
    for _ in range(n_tx):
        acct = min(tmp.keys(), key=lambda a: (tmp[a], a))
        s = int(tmp[acct])
        dest = "acct20" if acct != "acct20" else "acct19"
        plan.append((acct, s, dest))
        tmp[acct] = s + 1
    ok = fail = 0
    hist: dict[str, int] = {}
    t0 = time.time()
    for acct, s, dest in plan:
        code = "fail"
        try:
            d = rpc(
                BASE_RPC,
                "submit",
                {
                    "account": acct,
                    "sequence": s,
                    "destination": dest,
                    "amount": 1,
                    "fee": 10,
                },
                timeout=4.0,
                retries=3,
            )
            res = d.get("result") or d
            code = (res.get("engine_result") or "empty") if isinstance(res, dict) else "bad"
            if code == "tesSUCCESS":
                ok += 1
                seq[acct] = max(int(seq.get(acct, 1)), s + 1)
            else:
                fail += 1
        except Exception as e:
            code = f"ex:{type(e).__name__}"
            fail += 1
        hist[code] = hist.get(code, 0) + 1
    wall = time.time() - t0
    seq_path.write_text(json.dumps(seq))
    sub = {
        "label": label,
        "n_tx": n_tx,
        "ok": ok,
        "fail": fail,
        "wall_s": wall,
        "submit_tps": n_tx / wall if wall > 0 else 0,
        "hist": hist,
    }
    log(
        f"submit[{label}] n={n_tx} ok={ok} fail={fail} wall={wall:.2f}s "
        f"submit_tps={sub['submit_tps']:.1f} hist={hist}"
    )
    return sub


def settle(timeout: float, min_exec: int) -> dict:
    t0 = time.time()
    last = None
    polls = []
    while time.time() - t0 < timeout:
        last = rich_snap(f"poll_{int(time.time()-t0)}")
        polls.append(
            {
                "t": time.time() - t0,
                "exec": last.get("exec"),
                "cert": last.get("cert"),
                "anchors": last.get("anchors"),
                "spread": last.get("exec_spread"),
                "alive": last.get("alive"),
            }
        )
        execs = last.get("exec") or []
        if (
            last.get("alive") == N
            and execs
            and min(execs) >= min_exec
            and min(execs) == max(execs)
        ):
            # optional: cert counts equal
            certs = last.get("cert") or []
            if certs and min(certs) == max(certs):
                break
        # hard fail if node died
        if last.get("alive", 0) < N:
            break
        time.sleep(1.0)
    return {"last": last, "wait_s": time.time() - t0, "polls": polls}


def level_pass(snap: dict, sub: dict, n_tx: int, prev_exec: int = 0) -> tuple[bool, list[str]]:
    reasons = []
    if snap.get("alive") != N:
        reasons.append(f"alive={snap.get('alive')}/{N}")
    if sub["ok"] < max(1, int(n_tx * 0.9)):
        reasons.append(f"admit_weak {sub['ok']}/{n_tx}")
    execs = snap.get("exec") or []
    if not execs:
        reasons.append("no_exec")
    else:
        if min(execs) != max(execs):
            reasons.append(f"exec_desync {execs}")
        if min(execs) < 1 and n_tx >= 30:
            reasons.append("exec_zero")
        # Require apply progress from this burst (not agree-while-stuck).
        gained = min(execs) - prev_exec
        need = max(1, int(n_tx * 0.25))
        if gained < need:
            reasons.append(
                f"apply_stall prev={prev_exec} now={min(execs)} gained={gained} need>={need}"
            )
    certs = snap.get("cert") or []
    if certs and min(certs) != max(certs):
        reasons.append(f"cert_desync {certs}")
    return (len(reasons) == 0, reasons)


def main() -> int:
    signal.signal(signal.SIGTERM, lambda *_: (cleanup(), sys.exit(1)))
    signal.signal(signal.SIGINT, lambda *_: (cleanup(), sys.exit(1)))
    log(f"OUT={OUT} BIN={BIN} CRYPTO={CRYPTO} LEVELS={LEVELS} SETTLE_S={SETTLE_S}")
    log("baseline: every-round Bullshark + ready expand (no Tip-BFT order), multipack+stagger")
    if not os.path.isfile(BIN):
        log(f"missing {BIN}")
        return 2
    start_nodes()
    time.sleep(1.5)
    rich_snap("t0_idle")

    results = []
    broke = None
    for n_tx in LEVELS:
        log(f"==== LEVEL n_tx={n_tx} ====")
        sub = submit_burst(n_tx, label=f"L{n_tx}")
        # min_exec: any progress past previous min if we can read last
        prev_exec = 0
        if results:
            pe = (results[-1].get("final") or {}).get("exec") or [0]
            prev_exec = min(pe) if pe else 0
        min_exec = prev_exec + max(1, int(n_tx * 0.25))
        st = settle(SETTLE_S, min_exec=min_exec)
        last = st["last"] or rich_snap(f"L{n_tx}_final")
        # re-tag final
        (OUT / f"snap_L{n_tx}_final.json").write_text(json.dumps(last, indent=2, default=str))
        execs = last.get("exec") or [0]
        em = min(execs) if execs else 0
        prev_exec = 0
        if results:
            pe = (results[-1].get("final") or {}).get("exec") or [0]
            prev_exec = min(pe) if pe else 0
        ok, reasons = level_pass(last, sub, n_tx, prev_exec=prev_exec)
        apply_note = f"exec={execs} cert={last.get('cert')} anc={last.get('anchors')} spread={last.get('exec_spread')}"
        log(
            f"LEVEL {n_tx}: {'PASS' if ok else 'FAIL'} wait={st['wait_s']:.1f}s "
            f"admit={sub['ok']}/{n_tx} {apply_note} reasons={reasons}"
        )
        row = {
            "n_tx": n_tx,
            "pass": ok,
            "reasons": reasons,
            "submit": sub,
            "wait_s": st["wait_s"],
            "final": {
                "exec": last.get("exec"),
                "cert": last.get("cert"),
                "ordered": last.get("ordered"),
                "anchors": last.get("anchors"),
                "alive": last.get("alive"),
                "exec_spread": last.get("exec_spread"),
                "batches": last.get("batches"),
            },
            "polls_tail": (st.get("polls") or [])[-5:],
        }
        results.append(row)
        (OUT / f"level_{n_tx}.json").write_text(json.dumps(row, indent=2, default=str))
        if not ok:
            fdir = forensic_dump(n_tx, "; ".join(reasons) or "fail", last, sub)
            row["forensic"] = str(fdir)
            broke = row
            log(f"STOP ladder at n_tx={n_tx}")
            break
        # brief cool-down
        time.sleep(2)

    summary = {
        "out": str(OUT),
        "crypto": CRYPTO,
        "levels": LEVELS,
        "results": results,
        "break": broke,
        "verdict": [],
    }
    passed = [r for r in results if r["pass"]]
    failed = [r for r in results if not r["pass"]]
    summary["verdict"].append(f"levels_run={len(results)} pass={len(passed)} fail={len(failed)}")
    if passed:
        summary["verdict"].append(
            f"max_pass_burst={max(r['n_tx'] for r in passed)} "
            f"exec_at_max={passed[-1]['final'].get('exec')}"
        )
    if broke:
        summary["verdict"].append(
            f"BREAK at n_tx={broke['n_tx']}: {broke['reasons']} forensic={broke.get('forensic')}"
        )
    else:
        summary["verdict"].append("ALL LEVELS PASSED (no hard break)")
    (OUT / "summary.json").write_text(json.dumps(summary, indent=2, default=str))
    log("==== VERDICT ====")
    for v in summary["verdict"]:
        log(v)
    log(f"Wrote {OUT}/summary.json")
    cleanup()
    return 0 if not broke else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        log(f"FATAL: {e}")
        cleanup()
        raise
