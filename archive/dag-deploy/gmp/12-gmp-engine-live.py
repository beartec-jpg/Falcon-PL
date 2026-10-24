#!/usr/bin/env python3
"""
PR-G4 live full-stack: in-engine GMP lease agents (no external issuer).

  Clients → GMP only
  Packers (falcon-dagd --gmp-url) lease + admit + pack
  Dig: pause val3 gmp agent via RPC gmp_control; backlog still clears
"""
from __future__ import annotations

import importlib.util
import json
import os
import signal
import subprocess
import sys
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCALE = ROOT / "real-mesh" / "07-scale-n-curve.py"
MESH_BIN = Path.home() / "falcon-dag-mesh" / "bin"
RS = Path.home() / "falcon-dag" / "falcon-dag-rs"
OUT = Path(os.environ.get("OUT", f"/tmp/gmp-engine-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

HOLD_S = float(os.environ.get("GMP_HOLD", "50"))
RATE = int(os.environ.get("GMP_RATE", "30"))
DRAIN_S = float(os.environ.get("GMP_DRAIN", "20"))
MAX_INFLIGHT = int(os.environ.get("GMP_MAX_INFLIGHT", "4"))
KILL_HOLD_S = float(os.environ.get("GMP_KILL_HOLD", "25"))

_spec = importlib.util.spec_from_file_location("sc", SCALE)
sc = importlib.util.module_from_spec(_spec)
assert _spec.loader
_spec.loader.exec_module(sc)


def http_json(url: str, method: str = "GET", body=None, timeout=5.0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data, method=method, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def start_bin(name: str, args: list[str], log_path: Path) -> subprocess.Popen:
    bin_path = MESH_BIN / name
    if not bin_path.exists():
        bin_path = RS / "target" / "release" / name
    log_path.parent.mkdir(parents=True, exist_ok=True)
    lf = open(log_path, "w")
    p = subprocess.Popen(
        [str(bin_path), *args],
        stdout=lf,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    print(f"started {name} pid={p.pid}", flush=True)
    return p


def client_flood(gmp: str, hold: float, rate: int, stats: dict, n: int):
    t0 = time.time()
    bank = sc.SeqBank(n)
    seq = {f"acct{a}": bank._seq.get(f"acct{a}", 1) for a in range(1, 41)}
    inflight = {f"acct{a}": 0 for a in range(1, 41)}
    lock = threading.Lock()
    print(f"  client seed acct1={seq['acct1']}", flush=True)

    def one():
        with lock:
            cands = [a for a in seq if inflight[a] < MAX_INFLIGHT]
            if not cands:
                return
            acct = min(cands, key=lambda a: seq[a])
            s = seq[acct]
            seq[acct] = s + 1
            inflight[acct] += 1
        try:
            r = http_json(
                f"{gmp}/v1/submit",
                "POST",
                {
                    "account": acct,
                    "sequence": s,
                    "destination": "acct40" if acct != "acct40" else "acct39",
                    "amount": 1,
                    "fee": 10,
                },
            )
            with lock:
                if r.get("error"):
                    stats["gmp_err"] = stats.get("gmp_err", 0) + 1
                    if seq[acct] == s + 1:
                        seq[acct] = s
                    inflight[acct] = max(0, inflight[acct] - 1)
                else:
                    stats["gmp_ok"] = stats.get("gmp_ok", 0) + 1
        except Exception:
            with lock:
                stats["gmp_err"] = stats.get("gmp_err", 0) + 1
                if seq[acct] == s + 1:
                    seq[acct] = s
                inflight[acct] = max(0, inflight[acct] - 1)

    def decay():
        while time.time() - t0 < hold + 1:
            time.sleep(0.35)
            with lock:
                for a in inflight:
                    if inflight[a] > 0:
                        inflight[a] = max(0, inflight[a] - 1)

    threading.Thread(target=decay, daemon=True).start()
    with ThreadPoolExecutor(max_workers=8) as ex:
        while time.time() - t0 < hold:
            sec0 = time.time()
            futs = [ex.submit(one) for _ in range(rate)]
            for f in as_completed(futs):
                _ = f.result()
            left = 1.0 - (time.time() - sec0)
            if left > 0:
                time.sleep(left)


def gmp_stats_val(n: int, i: int) -> dict:
    try:
        return sc.rpc(sc.host_for_val(n, i), sc.VAL_RPC[i], "gmp_stats", {}, timeout=3).get(
            "result"
        ) or {}
    except Exception as e:
        return {"error": str(e)}


def gmp_control(n: int, i: int, enabled: bool) -> dict:
    return (
        sc.rpc(
            sc.host_for_val(n, i),
            sc.VAL_RPC[i],
            "gmp_control",
            {"enabled": enabled},
            timeout=3,
        ).get("result")
        or {}
    )


def main():
    print(f"OUT={OUT}", flush=True)
    procs = []
    for port in (18500, 18501):
        subprocess.call(f"fuser -k {port}/tcp 2>/dev/null || true", shell=True)
    time.sleep(0.3)

    # Rebuild if needed
    if not (MESH_BIN / "falcon-dagd").exists() or not (MESH_BIN / "falcon-gmpd").exists():
        subprocess.check_call(
            ["cargo", "build", "-p", "fd-gmp", "-p", "fd-node", "--release"],
            cwd=str(RS),
        )
        for n in ("falcon-dagd", "falcon-gmpd", "falcon-gcpd"):
            subprocess.check_call(
                ["cp", "-f", str(RS / "target" / "release" / n), str(MESH_BIN / n)]
            )

    procs.append(
        start_bin(
            "falcon-gmpd",
            ["--listen", "0.0.0.0:18500", "--shards", "16", "--lease-ttl-s", "8"],
            OUT / "gmpd.log",
        )
    )
    procs.append(start_bin("falcon-gcpd", ["--listen", "0.0.0.0:18501"], OUT / "gcpd.log"))
    time.sleep(0.5)

    # GMP reachable from both hosts via LAN F1
    gmp_url = f"http://{sc.F1}:18500"
    gcp_url = f"http://{sc.F1}:18501"
    os.environ["GMP_URL"] = gmp_url
    os.environ["GCP_URL"] = gcp_url
    # Capacity skew: val1 large, val3 small
    os.environ["GMP_BUDGET_val1"] = "8192"
    os.environ["GMP_BUDGET_val2"] = "4096"
    os.environ["GMP_BUDGET_val3"] = "1024"
    os.environ["GMP_MAX_IF_val1"] = "64"
    os.environ["GMP_MAX_IF_val2"] = "32"
    os.environ["GMP_MAX_IF_val3"] = "8"
    os.environ["GMP_LEASE_MAX"] = "4"
    os.environ["GMP_AFFINITY"] = "1"

    n = 3
    sc.SINGLE_PACKER = False
    sc.PACK_OPEN = 0
    sc.stop_all()
    sc.wipe_data()
    sc.ensure_keys(n)
    sc.sh(f"rsync -az {sc.BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)
    sc.start_mesh(n, 9404)
    time.sleep(3)

    # smoke: ensure nodes have gmp agent
    for i in range(1, n + 1):
        st = gmp_stats_val(n, i)
        print(f"  gmp_stats val{i}: enabled={st.get('enabled')} url={st.get('url')}", flush=True)
        if st.get("enabled") is not True and "error" not in st:
            # enabled false with empty url means flag not passed
            if not st.get("url"):
                print(f"FATAL: val{i} missing GMP agent", flush=True)
                for p in procs:
                    try:
                        os.killpg(p.pid, signal.SIGTERM)
                    except Exception:
                        p.terminate()
                return 2

    # smoke owner-route once
    bank0 = sc.SeqBank(n)
    acct, s = bank0.reserve_lowest([f"acct{i}" for i in range(1, 41)])
    ovi = bank0.owner_idx(acct) + 1
    d = sc.rpc(
        sc.host_for_val(n, ovi),
        sc.VAL_RPC[ovi],
        "submit",
        {
            "account": acct,
            "sequence": s,
            "destination": "acct40" if acct != "acct40" else "acct39",
            "amount": 1,
            "fee": 10,
        },
    )
    print("smoke", acct, s, "->", ovi, (d.get("result") or {}).get("engine_result"), flush=True)
    time.sleep(2)

    stats: dict = {}
    before = sc.snap_vals(n)
    t0 = time.time()
    print(f"phase A: client→GMP only rate={RATE} hold={HOLD_S}s (engine leases)", flush=True)
    client_flood(gmp_url, HOLD_S, RATE, stats, n)
    print(f"drain {DRAIN_S}s…", flush=True)
    time.sleep(DRAIN_S)
    mid = sc.snap_vals(n)
    elapsed_a = time.time() - t0
    ha = sc.health(n, mid)
    deltas_a = []
    for i in range(1, n + 1):
        b = before.get(f"val{i}", {}).get("exec")
        a = mid.get(f"val{i}", {}).get("exec")
        if isinstance(b, int) and isinstance(a, int):
            deltas_a.append(a - b)
    exec_tps_a = (min(deltas_a) / elapsed_a) if deltas_a else 0.0

    agent_stats = {f"val{i}": gmp_stats_val(n, i) for i in range(1, n + 1)}
    gmp_st = http_json(f"{gmp_url}/v1/status")
    gcp_st = http_json(f"{gcp_url}/v1/status")

    # Capacity skew from agent admit_ok or GMP completed
    lease_shares = {}
    for i in range(1, n + 1):
        st = agent_stats.get(f"val{i}") or {}
        lease_shares[f"val{i}"] = int(st.get("admit_ok") or st.get("complete_ok") or 0)
    # Prefer GMP completed counters if present
    vals = (gmp_st.get("validators") or {})
    if vals:
        for i in range(1, n + 1):
            vid = f"val{i}"
            if vid in vals:
                lease_shares[vid] = int(vals[vid].get("completed") or 0)

    skew_ok = lease_shares.get("val1", 0) >= lease_shares.get("val3", 0)

    # Phase B: kill-packer dig — pause val3 GMP agent, flood again
    print("phase B: pause val3 GMP agent (kill-packer dig)", flush=True)
    print("  gmp_control val3 enabled=false →", gmp_control(n, 3, False), flush=True)
    before_b = sc.snap_vals(n)
    stats_b: dict = {}
    t1 = time.time()
    client_flood(gmp_url, KILL_HOLD_S, RATE, stats_b, n)
    time.sleep(DRAIN_S)
    after_b = sc.snap_vals(n)
    elapsed_b = time.time() - t1
    hb = sc.health(n, after_b)
    deltas_b = []
    for i in range(1, n + 1):
        b = before_b.get(f"val{i}", {}).get("exec")
        a = after_b.get(f"val{i}", {}).get("exec")
        if isinstance(b, int) and isinstance(a, int):
            deltas_b.append(a - b)
    exec_tps_b = (min(deltas_b) / elapsed_b) if deltas_b else 0.0
    gmp_after_kill = http_json(f"{gmp_url}/v1/status")
    # Resume val3
    gmp_control(n, 3, True)

    pass_a = (
        exec_tps_a >= 3.0
        and ha.get("vals_up") == 3
        and (ha.get("lag_max") or 0) <= 5
        and (ha.get("exec_spread") or 0) <= 5
        and stats.get("gmp_ok", 0) > 50
        and skew_ok
    )
    # Kill dig: still execute while val3 agent paused; lag ok after drain
    pass_b = (
        exec_tps_b >= 1.0
        and (hb.get("lag_max") or 0) <= 10
        and (hb.get("exec_spread") or 0) <= 10
        and stats_b.get("gmp_ok", 0) > 20
        and (gmp_after_kill.get("pending") or 0) < 50  # backlog drained
    )
    report = {
        "suite": "gmp_engine_pr_g4",
        "design": "docs/GLOBAL_MEMPOOL_CONF_POOL_DESIGN.md",
        "gmp_url": gmp_url,
        "phase_a": {
            "elapsed_s": round(elapsed_a, 2),
            "exec_tps_min": round(exec_tps_a, 2),
            "exec_delta": deltas_a,
            "health": ha,
            "stats": stats,
            "lease_shares": lease_shares,
            "capacity_skew_ok": skew_ok,
            "agent_stats": agent_stats,
            "gmp_status": gmp_st,
            "gcp_status": gcp_st,
            "pass": pass_a,
        },
        "phase_b_kill_val3_agent": {
            "elapsed_s": round(elapsed_b, 2),
            "exec_tps_min": round(exec_tps_b, 2),
            "exec_delta": deltas_b,
            "health": hb,
            "stats": stats_b,
            "gmp_pending_after": gmp_after_kill.get("pending"),
            "pass": pass_b,
        },
        "pass": pass_a and pass_b,
    }

    (OUT / "GMP_ENGINE_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")
    md = [
        "# PR-G4 GMP engine live report",
        "",
        f"**Pass:** {report['pass']}",
        "",
        f"## Phase A (all agents) exec_tps_min={report['phase_a']['exec_tps_min']} "
        f"skew={lease_shares} pass={pass_a}",
        f"health={ha}",
        "",
        f"## Phase B (val3 agent paused) exec_tps_min={report['phase_b_kill_val3_agent']['exec_tps_min']} "
        f"pending={gmp_after_kill.get('pending')} pass={pass_b}",
        f"health={hb}",
        "",
    ]
    (OUT / "GMP_ENGINE_REPORT.md").write_text("\n".join(md))
    print(json.dumps(report, indent=2), flush=True)

    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            p.terminate()
    print(f"wrote {OUT}/GMP_ENGINE_REPORT.json mesh left up", flush=True)
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
