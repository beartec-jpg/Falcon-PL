#!/usr/bin/env python3
"""
Progressive lane-mesh join test (light load, then TPS ramp).

Phases:
  1) Start val1 only — admit txs
  2) Add val2 — mesh connect + light catch-up
  3–5) Add val3..val5 — sync under light load
  6) Ramp submit rate until something breaks; dump diagnostics

Env:
  BIN, CRYPTO=falcon-512, SINGLE_PACKER=false, ACCOUNTS=5
  OUT_DIR=/tmp/lane-progressive-...
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

BIN = os.environ.get("BIN", "/opt/falcon-dag/falcon-dag-rs/target/release/falcon-dagd")
CRYPTO = os.environ.get("CRYPTO", "falcon-512")
SINGLE_PACKER = os.environ.get("SINGLE_PACKER", "false")
ACCOUNTS = int(os.environ.get("ACCOUNTS", "5"))
BASE_RPC = int(os.environ.get("BASE_RPC", "29800"))
BASE_P2P = int(os.environ.get("BASE_P2P", "29900"))
N_MAX = 5
UNL_IDS = [f"val{i}" for i in range(1, N_MAX + 1)]
UNL = ",".join(UNL_IDS)
OUT = Path(os.environ.get("OUT_DIR", f"/tmp/lane-progressive-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

pids: list[int] = []
logdir = OUT / "nodes"
logdir.mkdir(exist_ok=True)
keys = logdir / "keys"
wallets = logdir / "wallets"
report: list[dict] = []


def log(msg: str) -> None:
    line = f"[{time.strftime('%H:%M:%S')}] {msg}"
    print(line, flush=True)
    with open(OUT / "run.log", "a") as f:
        f.write(line + "\n")


def cleanup(*_a) -> None:
    for p in list(pids):
        try:
            os.kill(p, signal.SIGTERM)
        except ProcessLookupError:
            pass
    time.sleep(0.5)
    for p in list(pids):
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass
    for i in range(N_MAX):
        subprocess.run(
            ["fuser", "-k", f"{BASE_RPC + i}/tcp"],
            capture_output=True,
        )
        subprocess.run(
            ["fuser", "-k", f"{BASE_P2P + i}/tcp"],
            capture_output=True,
        )


def rpc(port: int, method: str, params=None, timeout: float = 2.0, retries: int = 3):
    body = json.dumps({"method": method, "params": [params or {}]}).encode()
    last = None
    for attempt in range(retries):
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
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                raw = resp.read()
            if not raw or not raw.strip():
                raise RuntimeError("empty body")
            return json.loads(raw)
        except Exception as e:
            last = e
            time.sleep(0.05 * (attempt + 1))
    raise RuntimeError(f"rpc {method}:{port} failed: {last}")


def wait_ping(port: int, tries: int = 100) -> bool:
    for _ in range(tries):
        try:
            d = rpc(port, "ping", timeout=1.0, retries=1)
            if "pong" in json.dumps(d).lower() or d.get("result"):
                return True
        except Exception:
            pass
        time.sleep(0.1)
    return False


def metrics(i: int) -> dict:
    d = rpc(BASE_RPC + i, "server_info", timeout=2.0, retries=3)
    info = (d.get("result") or {}).get("info") or {}
    m = info.get("metrics") or {}
    if not m:
        raise RuntimeError(f"no metrics val{i+1}: {d}")
    return m


def snap_nodes(live: int) -> list[dict]:
    out = []
    for i in range(live):
        try:
            m = metrics(i)
            out.append(
                {
                    "id": f"val{i+1}",
                    "ok": True,
                    "executed": int(m.get("executed_txs") or 0),
                    "certified": int(m.get("certified") or 0),
                    "ordered": int(m.get("ordered") or 0),
                    "mempool": int(m.get("mempool") or 0),
                    "batches_created": int(m.get("batches_created") or 0),
                    "anchors": int(m.get("bullshark_anchors") or 0),
                    "max_cert_round": int(m.get("max_cert_round") or 0),
                    "peer_count": _peer_count(m),
                    "pull": m.get("pull") or m.get("mesh") or {},
                    "pack_skip": m.get("pack_skip_reasons") or {},
                }
            )
        except Exception as e:
            out.append({"id": f"val{i+1}", "ok": False, "error": str(e)})
    return out


def _peer_count(m: dict) -> int | None:
    # metrics layout varies; try common keys
    for k in ("peer_count", "peers", "p2p_peers"):
        if k in m and isinstance(m[k], (int, float)):
            return int(m[k])
    pull = m.get("pull") or {}
    if isinstance(pull, dict) and "peers" in pull:
        try:
            return int(pull["peers"])
        except Exception:
            pass
    mesh = m.get("mesh") or {}
    if isinstance(mesh, dict):
        for k in ("peer_count", "peers", "connected"):
            if k in mesh:
                try:
                    return int(mesh[k])
                except Exception:
                    pass
    return None


def gen_keys() -> None:
    keys.mkdir(parents=True, exist_ok=True)
    wallets.mkdir(parents=True, exist_ok=True)
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
    log(f"generated keys in {keys}, wallets in {wallets}")


def start_node(i: int) -> None:
    """Start val{i+1}. Peers = all other p2p ports (may not be up yet)."""
    id_ = f"val{i + 1}"
    peers = []
    for j in range(N_MAX):
        if j == i:
            continue
        peers.append(f"127.0.0.1:{BASE_P2P + j}")
    peer_s = ",".join(peers)
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
        f"127.0.0.1:{BASE_RPC + i}",
        "--p2p",
        f"127.0.0.1:{BASE_P2P + i}",
        "--peers",
        peer_s,
        "--unl",
        UNL,
        "--worker-index",
        str(i),
        "--worker-count",
        str(N_MAX),
        "--network-id",
        "9003",
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
        "--sequence-window",
        "512",
        "--unl-threshold",
        "0.6",
        "--max-uncertified-per-creator",
        "8",
        f"--single-packer={SINGLE_PACKER}",
        "--adaptive-batch=true",
        "--data-dir",
        str(data),
    ]
    # multipack Option A
    env = os.environ.copy()
    env["BULLSHARK_TIEBREAKER"] = "ACCOUNT_ID_LEXICOGRAPHIC"
    env["DIRECT_SUBMIT"] = "1"
    p = subprocess.Popen(cmd, stdout=logf, stderr=subprocess.STDOUT, env=env)
    pids.append(p.pid)
    log(f"started {id_} pid={p.pid} rpc={BASE_RPC + i} p2p={BASE_P2P + i}")
    if not wait_ping(BASE_RPC + i):
        raise RuntimeError(f"{id_} did not answer ping")
    log(f"{id_} ping ok")


def submit_light(n_tx: int, dest_port: int | None = None, label: str = "") -> dict:
    """Submit n_tx across ACCOUNTS faucet accounts to dest (default val1)."""
    port = dest_port if dest_port is not None else BASE_RPC
    n_accts = max(1, ACCOUNTS)
    seq_path = OUT / "seq_base.json"
    if seq_path.exists():
        base_seq = json.loads(seq_path.read_text())
    else:
        base_seq = {f"acct{a}": 1 for a in range(1, n_accts + 1)}

    # Build plan: round-robin accounts with contiguous sequences per account
    plan = []
    tmp_seq = dict(base_seq)
    for _ in range(n_tx):
        # pick account with lowest next seq (spread load)
        acct = min(tmp_seq.keys(), key=lambda a: (tmp_seq[a], a))
        seq = int(tmp_seq[acct])
        dest = "acct20" if acct != "acct20" else "acct19"
        plan.append((acct, seq, dest))
        tmp_seq[acct] = seq + 1

    admit_ok = admit_fail = 0
    hist: dict[str, int] = {}
    t0 = time.time()
    for acct, seq, dest in plan:
        code = ""
        detail = ""
        for attempt in range(6):
            try:
                d = rpc(
                    port,
                    "submit",
                    {
                        "account": acct,
                        "sequence": seq,
                        "destination": dest,
                        "amount": 1,
                        "fee": 10,
                    },
                    timeout=3.0,
                    retries=2,
                )
                res = d.get("result") or d
                code = (res.get("engine_result") or "") if isinstance(res, dict) else ""
                detail = (res.get("engine_detail") or "") if isinstance(res, dict) else ""
                err = res.get("error") if isinstance(res, dict) else None
                if not code and err:
                    code = f"rpc_error:{err}"
                if code == "tesSUCCESS":
                    break
                # retry transient
                if "eof" in str(err).lower() or "empty" in str(err).lower():
                    time.sleep(0.02 * (attempt + 1))
                    continue
                break
            except Exception as e:
                code = "fail"
                detail = type(e).__name__
                time.sleep(0.02 * (attempt + 1))
        key = f"{code}|{detail}" if detail else (code or "empty")
        hist[key] = hist.get(key, 0) + 1
        if code == "tesSUCCESS":
            admit_ok += 1
            base_seq[acct] = max(int(base_seq.get(acct, 1)), seq + 1)
        else:
            admit_fail += 1
    wall = time.time() - t0
    seq_path.write_text(json.dumps(base_seq, indent=2))
    submitted = len(plan)
    res = {
        "label": label,
        "submitted": submitted,
        "admit_ok": admit_ok,
        "admit_fail": admit_fail,
        "wall_s": wall,
        "submit_tps": submitted / wall if wall > 0 else 0,
        "hist": hist,
        "port": port,
    }
    log(
        f"submit[{label}] n={submitted} ok={admit_ok} fail={admit_fail} "
        f"wall={wall:.2f}s tps={res['submit_tps']:.1f} hist={hist}"
    )
    return res


def wait_settle(live: int, min_exec: int, timeout: float = 20.0) -> dict:
    t0 = time.time()
    last = []
    while time.time() - t0 < timeout:
        try:
            last = snap_nodes(live)
        except Exception as e:
            log(f"wait_settle snap err: {e}")
            last = []
        execs = [x.get("executed", 0) for x in last if x.get("ok")]
        if min_exec == 0:
            time.sleep(0.5)
            continue
        if execs and min(execs) >= min_exec and min(execs) == max(execs):
            break
        if execs and min(execs) >= min_exec:
            # progress even if not agreed yet — keep waiting for sync until timeout
            pass
        time.sleep(0.5)
    return {"snaps": last, "wait_s": time.time() - t0}


def analyze_phase(name: str, live: int, submit_res: dict | None = None) -> dict:
    snaps = snap_nodes(live)
    execs = [s.get("executed", 0) for s in snaps if s.get("ok")]
    peers = [s.get("peer_count") for s in snaps if s.get("ok")]
    certs = [s.get("certified", 0) for s in snaps if s.get("ok")]
    phase = {
        "phase": name,
        "live_nodes": live,
        "snaps": snaps,
        "exec_min": min(execs) if execs else None,
        "exec_max": max(execs) if execs else None,
        "exec_spread": (max(execs) - min(execs)) if execs else None,
        "peer_counts": peers,
        "cert_min": min(certs) if certs else None,
        "submit": submit_res,
        "notes": [],
    }
    # connectivity: each live node should eventually see live-1 peers (best effort)
    if live >= 2:
        good_peers = [p for p in peers if p is not None and p >= 1]
        if len(good_peers) < live:
            phase["notes"].append(
                f"peer_count weak: {peers} (want each >=1 when live={live})"
            )
        else:
            phase["notes"].append(f"peer metrics present: {peers}")
    if live >= 3:
        # cert quorum possible (0.6 of 5 ≈ 3)
        if phase["cert_min"] is not None and phase["cert_min"] < 2:
            phase["notes"].append("certs still low after 3+ nodes — mesh/sig lag?")
        if phase["exec_spread"] is not None and phase["exec_spread"] > 5:
            phase["notes"].append(
                f"exec spread {phase['exec_spread']} — nodes not fully synced"
            )
    if submit_res and submit_res["admit_ok"] == 0 and submit_res["submitted"] > 0:
        phase["notes"].append("NO admits — submit path broken")
    log(
        f"ANALYZE {name}: live={live} exec={phase['exec_min']}-{phase['exec_max']} "
        f"certs_min={phase['cert_min']} peers={peers} notes={phase['notes']}"
    )
    for s in snaps:
        log(f"  {json.dumps(s, default=str)[:300]}")
    report.append(phase)
    (OUT / f"phase_{name}.json").write_text(json.dumps(phase, indent=2, default=str))
    return phase


def main() -> int:
    signal.signal(signal.SIGTERM, lambda *_: (cleanup(), sys.exit(1)))
    signal.signal(signal.SIGINT, lambda *_: (cleanup(), sys.exit(1)))
    log(f"OUT={OUT} BIN={BIN} CRYPTO={CRYPTO} single_packer={SINGLE_PACKER}")
    if not os.path.isfile(BIN) or not os.access(BIN, os.X_OK):
        log(f"missing binary {BIN}")
        return 2
    gen_keys()

    # ---- Phase 1: single node, admit ----
    log("==== PHASE 1: start val1, admit light txs ====")
    start_node(0)
    time.sleep(1)
    s1 = submit_light(10, label="p1_admit10")
    time.sleep(2)
    p1 = analyze_phase("1_single_node", 1, s1)
    if s1["admit_ok"] < 1:
        # try alternate submit via multinode script knowledge
        log("WARN: admits failed — dumping val1.log tail")
        log((logdir / "val1.log").read_text()[-2000:])

    # ---- Continuous light drip while growing the mesh ----
    # Baseline binary: every-round Bullshark, no Tip-BFT order, pack stagger on.
    # UNL lists all 5 from t=0; nodes join online progressively under light load.
    def drip(n: int, label: str) -> dict:
        return submit_light(n, label=label)

    log("==== PHASE 2: add val2 under light drip ====")
    start_node(1)
    time.sleep(2)
    s2a = drip(8, "p2_drip_a")
    time.sleep(2)
    s2b = drip(8, "p2_drip_b")
    w2 = wait_settle(2, min_exec=1, timeout=25.0)
    p2 = analyze_phase("2_two_nodes", 2, s2b)
    p2["wait"] = w2["wait_s"]
    p2["drip"] = [s2a, s2b]

    for i, name in ((2, "3_three_nodes"), (3, "4_four_nodes"), (4, "5_five_nodes")):
        log(f"==== PHASE {i+1}: add val{i+1} while txs keep dripping ====")
        drip(6, f"p{i+1}_pre_join_drip")
        start_node(i)
        time.sleep(2)
        s_a = drip(10, f"p{i+1}_post_join_drip_a")
        time.sleep(1.5)
        s_b = drip(10, f"p{i+1}_post_join_drip_b")
        wait_s = 35.0 if i + 1 >= 4 else 28.0
        cur = snap_nodes(i + 1)
        execs = [x.get("executed", 0) for x in cur if x.get("ok")]
        target = (min(execs) if execs else 0) + 1
        log(f"post-join wait up to {wait_s}s for catch-up (live={i+1}) target_min_exec~{target}")
        w = wait_settle(i + 1, min_exec=max(1, target), timeout=wait_s)
        time.sleep(5)
        ph = analyze_phase(name, i + 1, s_b)
        ph["wait"] = w["wait_s"] + 5
        ph["drip"] = [s_a, s_b]

    log("==== PHASE 6: light settle 40 txs on 5-node mesh, wait agreement ====")
    s6 = drip(40, "p6_light40")
    log("phase6 wait up to 45s for exec agreement")
    t0 = time.time()
    last = []
    while time.time() - t0 < 45.0:
        last = snap_nodes(5)
        execs = [x.get("executed", 0) for x in last if x.get("ok")]
        if execs and min(execs) == max(execs) and min(execs) > 0:
            break
        time.sleep(0.5)
    p6 = analyze_phase("6_light_settle", 5, s6)
    p6["wait"] = time.time() - t0
    p6["final_snaps"] = last

    # ---- Phase 7: ramp TPS until break ----
    log("==== PHASE 7: ramp load until break ====")
    break_info = None
    # ramp: burst sizes at increasing rates
    for burst, pause in (
        (20, 0.5),
        (40, 0.3),
        (80, 0.15),
        (120, 0.05),
        (200, 0.0),
    ):
        log(f"RAMP burst={burst} pause={pause}")
        t0 = time.time()
        sr = submit_light(burst, label=f"ramp_{burst}")
        # spin small waits between batches if pause
        if pause > 0:
            time.sleep(pause)
        time.sleep(5.0)
        snaps = snap_nodes(5)
        execs = [s.get("executed", 0) for s in snaps if s.get("ok")]
        mems = [s.get("mempool", 0) for s in snaps if s.get("ok")]
        ok_nodes = sum(1 for s in snaps if s.get("ok"))
        spread = (max(execs) - min(execs)) if execs else 999
        log(
            f"  after burst{burst}: exec={execs} mempool={mems} "
            f"spread={spread} ok_nodes={ok_nodes} admit={sr['admit_ok']}/{sr['submitted']}"
        )
        broken = False
        reasons = []
        if ok_nodes < 5:
            broken = True
            reasons.append(f"node_rpc_down ok={ok_nodes}/5")
        if sr["admit_ok"] < sr["submitted"] * 0.5:
            broken = True
            reasons.append(f"admit_collapse {sr['admit_ok']}/{sr['submitted']}")
        if spread > 50:
            broken = True
            reasons.append(f"exec_desync spread={spread}")
        if max(mems or [0]) > 150 and min(execs or [0]) == max(execs or [0]) == 0:
            broken = True
            reasons.append("mempool_stuck_zero_exec")
        ramp_row = {
            "burst": burst,
            "submit": sr,
            "snaps": snaps,
            "spread": spread,
            "broken": broken,
            "reasons": reasons,
        }
        report.append({"phase": f"7_ramp_{burst}", **ramp_row})
        (OUT / f"ramp_{burst}.json").write_text(
            json.dumps(ramp_row, indent=2, default=str)
        )
        if broken:
            break_info = ramp_row
            log(f"BREAK at burst={burst}: {reasons}")
            # dump logs tails
            for i in range(5):
                lp = logdir / f"val{i+1}.log"
                if lp.exists():
                    tail = lp.read_text()[-1500:]
                    (OUT / f"break_val{i+1}_log_tail.txt").write_text(tail)
            break
    else:
        log("RAMP completed without hard break (mesh held under bursts)")

    summary = {
        "out": str(OUT),
        "crypto": CRYPTO,
        "single_packer": SINGLE_PACKER,
        "phases": report,
        "break": break_info,
        "verdict": _verdict(report, break_info),
    }
    (OUT / "summary.json").write_text(json.dumps(summary, indent=2, default=str))
    log("==== VERDICT ====")
    for line in summary["verdict"]:
        log(line)
    log(f"Wrote {OUT}/summary.json")
    cleanup()
    return 0


def _verdict(phases: list, break_info) -> list[str]:
    lines = []
    by = {p.get("phase"): p for p in phases if "phase" in p}
    p1 = by.get("1_single_node") or {}
    sub = (p1.get("submit") or {})
    if sub.get("admit_ok", 0) > 0:
        lines.append("PASS phase1: single node admits txs")
    else:
        lines.append("FAIL phase1: single node did not admit txs")
    p2 = by.get("2_two_nodes") or {}
    peers = p2.get("peer_counts") or []
    if any(p is not None and p >= 1 for p in peers):
        lines.append(f"PASS phase2: some peer connectivity visible {peers}")
    else:
        lines.append(
            f"WARN/FAIL phase2: peer_count not visible or zero {peers} "
            "(check mesh logs; pings may still work)"
        )
    for key in ("3_three_nodes", "4_four_nodes", "5_five_nodes"):
        p = by.get(key) or {}
        if not p.get("live_nodes"):
            continue
        spread = p.get("exec_spread")
        em, ex = p.get("exec_min"), p.get("exec_max")
        if spread is not None and spread == 0 and (em or 0) > 0:
            lines.append(f"PASS {key}: exec agree at {em} certs_min={p.get('cert_min')}")
        elif spread is not None and spread <= 3 and (em or 0) > 0:
            lines.append(
                f"WARN {key}: mild exec spread {em}-{ex} certs_min={p.get('cert_min')} notes={p.get('notes')}"
            )
        else:
            lines.append(
                f"FAIL/INFO {key}: exec={em}-{ex} spread={spread} "
                f"certs_min={p.get('cert_min')} notes={p.get('notes')}"
            )
    p6 = by.get("6_light_settle") or {}
    if p6:
        sp = p6.get("exec_spread")
        if sp == 0 and (p6.get("exec_min") or 0) > 0:
            lines.append(
                f"PASS light_settle: exec agree at {p6.get('exec_min')} "
                f"certs_min={p6.get('cert_min')}"
            )
        else:
            lines.append(
                f"WARN/FAIL light_settle: exec={p6.get('exec_min')}-{p6.get('exec_max')} "
                f"spread={sp} certs_min={p6.get('cert_min')}"
            )
    if break_info:
        lines.append(
            f"BREAK under ramp burst={break_info.get('burst')}: {break_info.get('reasons')}"
        )
    else:
        lines.append("INFO ramp: no hard break criteria hit")
    return lines


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        log(f"FATAL: {e}")
        cleanup()
        raise
