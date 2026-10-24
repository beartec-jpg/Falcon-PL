#!/usr/bin/env python3
"""
PR-G5 live dig: engine GMP leases + GCP conf registry hooks.

Pass if:
  - N=3 multipack with --gmp-url + --gcp-url executes under GMP-only client flood
  - GCP registry sees batches (and ideally certs)
  - Node gcp_stats report batch publishes
  - Kill-agent dig still clears backlog
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
OUT = Path(os.environ.get("OUT", f"/tmp/gmp-gcp-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

HOLD_S = float(os.environ.get("GMP_HOLD", "45"))
RATE = int(os.environ.get("GMP_RATE", "28"))
DRAIN_S = float(os.environ.get("GMP_DRAIN", "18"))
MAX_INFLIGHT = int(os.environ.get("GMP_MAX_INFLIGHT", "4"))
KILL_HOLD_S = float(os.environ.get("GMP_KILL_HOLD", "20"))

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


def rpc_method(n: int, i: int, method: str, params=None) -> dict:
    try:
        return (
            sc.rpc(
                sc.host_for_val(n, i),
                sc.VAL_RPC[i],
                method,
                params or {},
                timeout=3,
            ).get("result")
            or {}
        )
    except Exception as e:
        return {"error": str(e)}


def main():
    print(f"OUT={OUT}", flush=True)
    procs = []
    for port in (18500, 18501):
        subprocess.call(f"fuser -k {port}/tcp 2>/dev/null || true", shell=True)
    time.sleep(0.3)

    # Ensure latest bins
    for n in ("falcon-dagd", "falcon-gmpd", "falcon-gcpd"):
        src = RS / "target" / "release" / n
        if src.exists():
            subprocess.check_call(["cp", "-f", str(src), str(MESH_BIN / n)])

    procs.append(
        start_bin(
            "falcon-gmpd",
            ["--listen", "0.0.0.0:18500", "--shards", "16", "--lease-ttl-s", "8"],
            OUT / "gmpd.log",
        )
    )
    procs.append(start_bin("falcon-gcpd", ["--listen", "0.0.0.0:18501"], OUT / "gcpd.log"))
    time.sleep(0.5)

    gmp_url = f"http://{sc.F1}:18500"
    gcp_url = f"http://{sc.F1}:18501"
    os.environ["GMP_URL"] = gmp_url
    os.environ["GCP_URL"] = gcp_url
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
    sc.start_mesh(n, 9505)
    time.sleep(3)

    for i in range(1, n + 1):
        gs = rpc_method(n, i, "gmp_stats")
        cs = rpc_method(n, i, "gcp_stats")
        print(
            f"  val{i} gmp_enabled={gs.get('enabled')} gcp_enabled={cs.get('enabled')} "
            f"gcp_url={cs.get('url')}",
            flush=True,
        )
        if not gs.get("enabled") or not cs.get("enabled"):
            print("FATAL: GMP/GCP not enabled on packer", flush=True)
            for p in procs:
                try:
                    os.killpg(p.pid, signal.SIGTERM)
                except Exception:
                    p.terminate()
            return 2

    gcp0 = http_json(f"{gcp_url}/v1/status")
    print("GCP before", gcp0, flush=True)

    stats: dict = {}
    before = sc.snap_vals(n)
    t0 = time.time()
    print(f"phase A: GMP client flood rate={RATE} hold={HOLD_S}s", flush=True)
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

    gcp_a = http_json(f"{gcp_url}/v1/status")
    open_a = http_json(f"{gcp_url}/v1/open")
    open_n = len(open_a.get("open") or [])
    node_gcp = {f"val{i}": rpc_method(n, i, "gcp_stats") for i in range(1, n + 1)}
    node_batches = sum(int((node_gcp[f"val{i}"] or {}).get("batches") or 0) for i in range(1, n + 1))
    node_certs = sum(int((node_gcp[f"val{i}"] or {}).get("certs") or 0) for i in range(1, n + 1))
    gmp_a = http_json(f"{gmp_url}/v1/status")
    lease_shares = {
        vid: int((v or {}).get("completed") or 0)
        for vid, v in (gmp_a.get("validators") or {}).items()
    }
    skew_ok = lease_shares.get("val1", 0) >= lease_shares.get("val3", 0)

    gcp_ok = (
        int(gcp_a.get("total") or 0) >= 5
        and node_batches >= 3
        and (int(gcp_a.get("certified") or 0) >= 1 or node_certs >= 1 or open_n >= 1)
    )

    pass_a = (
        exec_tps_a >= 3.0
        and ha.get("vals_up") == 3
        and (ha.get("lag_max") or 0) <= 5
        and (ha.get("exec_spread") or 0) <= 5
        and stats.get("gmp_ok", 0) > 40
        and skew_ok
        and gcp_ok
    )

    # Phase B kill dig
    print("phase B: pause val3 GMP agent", flush=True)
    print(" ", rpc_method(n, 3, "gmp_control", {"enabled": False}), flush=True)
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
    gmp_b = http_json(f"{gmp_url}/v1/status")
    gcp_b = http_json(f"{gcp_url}/v1/status")
    rpc_method(n, 3, "gmp_control", {"enabled": True})

    pass_b = (
        exec_tps_b >= 1.0
        and (hb.get("lag_max") or 0) <= 10
        and stats_b.get("gmp_ok", 0) > 15
        and (gmp_b.get("pending") or 0) < 50
    )

    report = {
        "suite": "gmp_gcp_pr_g5",
        "design": "docs/GLOBAL_MEMPOOL_CONF_POOL_DESIGN.md",
        "gmp_url": gmp_url,
        "gcp_url": gcp_url,
        "phase_a": {
            "elapsed_s": round(elapsed_a, 2),
            "exec_tps_min": round(exec_tps_a, 2),
            "exec_delta": deltas_a,
            "health": ha,
            "stats": stats,
            "lease_shares": lease_shares,
            "capacity_skew_ok": skew_ok,
            "gcp_status": gcp_a,
            "gcp_open_n": open_n,
            "node_gcp_stats": node_gcp,
            "node_batches_sum": node_batches,
            "node_certs_sum": node_certs,
            "gcp_ok": gcp_ok,
            "pass": pass_a,
        },
        "phase_b_kill_val3_agent": {
            "elapsed_s": round(elapsed_b, 2),
            "exec_tps_min": round(exec_tps_b, 2),
            "exec_delta": deltas_b,
            "health": hb,
            "stats": stats_b,
            "gmp_pending_after": gmp_b.get("pending"),
            "gcp_status": gcp_b,
            "pass": pass_b,
        },
        "pass": pass_a and pass_b,
    }
    (OUT / "GMP_GCP_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")
    md = [
        "# PR-G5 GMP+GCP live report",
        "",
        f"**Pass:** {report['pass']}",
        "",
        f"## Phase A exec_tps_min={exec_tps_a:.2f} gcp_total={gcp_a.get('total')} "
        f"certified={gcp_a.get('certified')} node_batches={node_batches} pass={pass_a}",
        f"lease_shares={lease_shares} health={ha}",
        "",
        f"## Phase B kill-agent exec_tps_min={exec_tps_b:.2f} pending={gmp_b.get('pending')} pass={pass_b}",
        f"health={hb}",
        "",
    ]
    (OUT / "GMP_GCP_REPORT.md").write_text("\n".join(md))
    print(json.dumps(report, indent=2), flush=True)

    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            p.terminate()
    print(f"wrote {OUT}/GMP_GCP_REPORT.json", flush=True)
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
