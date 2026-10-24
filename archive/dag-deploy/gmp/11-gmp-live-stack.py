#!/usr/bin/env python3
"""
Live full-stack test: falcon-gmpd + falcon-gcpd + N=3 packing mesh + capacity-weighted issuer.

Design: docs/GLOBAL_MEMPOOL_CONF_POOL_DESIGN.md (PR-G1..G3)

v0 path:
  Clients → GMP only (no owner-route at client)
  Capacity-weighted Lease → issuer
  Issuer submits each leased tx to the **account owner** val RPC
    (current engine packs owned lanes only; soft affinity + handoff in PR-G4)
  GCP registry gets open-batch meta for observability
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
OUT = Path(os.environ.get("OUT", f"/tmp/gmp-live-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

GMP = os.environ.get("GMP_URL", "http://127.0.0.1:18500")
GCP = os.environ.get("GCP_URL", "http://127.0.0.1:18501")

# Tunables (env overrides)
HOLD_S = float(os.environ.get("GMP_HOLD", "50"))
RATE = int(os.environ.get("GMP_RATE", "20"))
DRAIN_S = float(os.environ.get("GMP_DRAIN", "25"))
MAX_INFLIGHT_PER_ACCT = int(os.environ.get("GMP_MAX_INFLIGHT", "2"))

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


def capacity_loop(stop: threading.Event, n: int = 3):
    """Report capacity adverts with deliberate skew: val1 >> val3."""
    while not stop.is_set():
        for i in range(1, n + 1):
            vid = f"val{i}"
            try:
                h = sc.host_for_val(n, i)
                m = sc.metrics(h, sc.VAL_RPC[i])
                ps = m.get("pack_stall") or {}
                # Strong skew for capacity-weighted dig
                budget = {1: 8192, 2: 4096, 3: 1024}.get(i, 2048)
                used = {1: 200, 2: 800, 3: 950}.get(i, 400)
                used = min(budget - 1, used + int(ps.get("own_open_n") or 0) * 20)
                http_json(
                    f"{GMP}/v1/capacity",
                    "POST",
                    {
                        "validator_id": vid,
                        "mem_budget_mb": budget,
                        "mem_used_mb": used,
                        "open_uncertified": ps.get("own_open_n") or 0,
                        "max_in_flight_leases": 64 if i == 1 else (32 if i == 2 else 8),
                        "recent_clearance_tps": 18.0 if i == 1 else (10.0 if i == 2 else 2.0),
                        "lease_expiry_rate": 0.0,
                        "ts": time.time(),
                    },
                )
            except Exception:
                pass
        stop.wait(0.35)


def issuer_loop(stop: threading.Event, n: int, stats: dict):
    """
    Capacity-weighted lease pull + soft-affinity submit:
    each val leases from GMP proportional to capacity, then submits each
    tx to the **account owner** RPC (current multipack packs owned lanes).

    Lease max_n=1 so complete/nack is atomic (partial nack must not requeue
    already-admitted txs — that caused tecSEQ_PAST thrash).

    Pull cadence is weight-paced: low-capacity vals sleep longer between
    lease attempts so higher-capacity vals clear more of the global backlog.
    """
    lock = threading.Lock()
    weights = {f"val{i}": 0.3 for i in range(1, n + 1)}
    wlock = threading.Lock()

    def weight_refresh():
        while not stop.is_set():
            try:
                st = http_json(f"{GMP}/v1/status", timeout=2)
                vs = st.get("validators") or {}
                with wlock:
                    for k, v in vs.items():
                        if isinstance(v, dict) and "weight" in v:
                            weights[k] = float(v["weight"])
            except Exception:
                pass
            stop.wait(0.5)

    def owner_vi(account: str) -> int:
        return (sc.account_shard(account) % n) + 1

    def one_val(i: int):
        vid = f"val{i}"
        while not stop.is_set():
            try:
                with wlock:
                    w = max(0.02, weights.get(vid, 0.1))
                    wmax = max(weights.values()) if weights else 1.0
                # Pace: full-speed at max weight; ~w/wmax of attempts otherwise
                # Extra sleep for low weight so dig shows capacity skew.
                pace = 0.002 + 0.06 * (1.0 - (w / max(wmax, 1e-9)))
                time.sleep(pace)

                lease = http_json(
                    f"{GMP}/v1/lease",
                    "POST",
                    {"validator_id": vid, "max_n": 1},
                    timeout=3,
                )
                if lease.get("error"):
                    time.sleep(0.05)
                    continue
                txs = lease.get("txs") or []
                lid = lease.get("lease_id") or ""
                if not txs or not lid:
                    time.sleep(0.02)
                    continue
                tx = txs[0]
                code = None
                ovi = owner_vi(tx["account"])
                try:
                    h = sc.host_for_val(n, ovi)
                    d = sc.rpc(
                        h,
                        sc.VAL_RPC[ovi],
                        "submit",
                        {
                            "account": tx["account"],
                            "sequence": tx["sequence"],
                            "destination": tx["destination"],
                            "amount": tx.get("amount", 1),
                            "fee": tx.get("fee", 10),
                        },
                        timeout=3,
                    )
                    code = (d.get("result") or {}).get("engine_result")
                except Exception:
                    with lock:
                        stats["submit_err"] = stats.get("submit_err", 0) + 1
                        stats["submit_exc"] = stats.get("submit_exc", 0) + 1
                    code = None

                try:
                    # Already applied / past-seq: drop from GMP (do not requeue)
                    if code in ("tesSUCCESS", "tecSEQ_PAST", "tefPAST_SEQ", "tefALREADY"):
                        if code == "tesSUCCESS":
                            with lock:
                                stats["submit_ok"] = stats.get("submit_ok", 0) + 1
                                stats[f"lease_to_{vid}"] = stats.get(f"lease_to_{vid}", 0) + 1
                                stats[f"submit_owner_val{ovi}"] = (
                                    stats.get(f"submit_owner_val{ovi}", 0) + 1
                                )
                        else:
                            with lock:
                                stats["submit_stale"] = stats.get("submit_stale", 0) + 1
                                stats[f"code_{code}"] = stats.get(f"code_{code}", 0) + 1
                        http_json(f"{GMP}/v1/complete", "POST", {"lease_id": lid})
                        if code == "tesSUCCESS":
                            try:
                                http_json(
                                    f"{GCP}/v1/batch",
                                    "POST",
                                    {
                                        "batch_hash": f"lease-{lid}",
                                        "creator": vid,
                                        "ntx": 1,
                                        "parents": [],
                                        "ts": time.time(),
                                    },
                                )
                            except Exception:
                                pass
                    else:
                        with lock:
                            stats["submit_err"] = stats.get("submit_err", 0) + 1
                            stats[f"code_{code}"] = stats.get(f"code_{code}", 0) + 1
                            stats["nack"] = stats.get("nack", 0) + 1
                        http_json(
                            f"{GMP}/v1/nack",
                            "POST",
                            {"lease_id": lid, "reason": f"code_{code}"},
                        )
                except Exception:
                    pass
            except Exception:
                time.sleep(0.08)

    threading.Thread(target=weight_refresh, daemon=True).start()
    threads = [threading.Thread(target=one_val, args=(i,), daemon=True) for i in range(1, n + 1)]
    for t in threads:
        t.start()
    while not stop.is_set():
        time.sleep(0.2)


def client_flood(hold: float, rate: int, stats: dict, n: int):
    """
    Submit only to GMP (no owner-route). Reserve sequence under lock to avoid
    duplicate (account,seq). Cap in-flight per account to limit pack seq gaps.
    """
    t0 = time.time()
    bank = sc.SeqBank(n)
    seq = {f"acct{a}": bank._seq.get(f"acct{a}", 1) for a in range(1, 41)}
    inflight = {f"acct{a}": 0 for a in range(1, 41)}
    lock = threading.Lock()
    print(f"  client seed seq sample acct1={seq['acct1']} acct2={seq['acct2']}", flush=True)

    def one():
        with lock:
            candidates = [a for a in seq if inflight[a] < MAX_INFLIGHT_PER_ACCT]
            if not candidates:
                return
            acct = min(candidates, key=lambda a: seq[a])
            s = seq[acct]
            # reserve sequence under lock (prevents concurrent duplicate submit)
            seq[acct] = s + 1
            inflight[acct] += 1
        try:
            r = http_json(
                f"{GMP}/v1/submit",
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
                    err = str(r.get("error"))
                    key = "gmp_err_" + err.split(":")[0][:40]
                    stats[key] = stats.get(key, 0) + 1
                    # rollback reservation if nothing accepted
                    if seq[acct] == s + 1:
                        seq[acct] = s
                    inflight[acct] = max(0, inflight[acct] - 1)
                else:
                    stats["gmp_ok"] = stats.get("gmp_ok", 0) + 1
        except Exception as e:
            with lock:
                stats["gmp_err"] = stats.get("gmp_err", 0) + 1
                stats["gmp_exc"] = stats.get("gmp_exc", 0) + 1
                stats[f"gmp_exc_{type(e).__name__}"] = stats.get(f"gmp_exc_{type(e).__name__}", 0) + 1
                if seq[acct] == s + 1:
                    seq[acct] = s
                inflight[acct] = max(0, inflight[acct] - 1)

    def decay():
        """Free inflight slots as issuers drain (~matches complete rate)."""
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


def main():
    print(f"OUT={OUT}", flush=True)
    procs = []
    gmpd = MESH_BIN / "falcon-gmpd"
    gcpd = MESH_BIN / "falcon-gcpd"
    if not gmpd.exists() or not gcpd.exists():
        print("building falcon-gmpd/gcpd…", flush=True)
        subprocess.check_call(
            ["cargo", "build", "-p", "fd-gmp", "--release"],
            cwd=str(RS),
        )
        MESH_BIN.mkdir(parents=True, exist_ok=True)
        for n in ("falcon-gmpd", "falcon-gcpd"):
            src = RS / "target" / "release" / n
            dst = MESH_BIN / n
            subprocess.check_call(["cp", "-f", str(src), str(dst)])

    # free ports if prior gmp/gcp still up
    for port in (18500, 18501):
        subprocess.call(
            f"fuser -k {port}/tcp 2>/dev/null || true",
            shell=True,
        )
    time.sleep(0.3)

    procs.append(
        start_bin(
            "falcon-gmpd",
            ["--listen", "0.0.0.0:18500", "--shards", "16", "--lease-ttl-s", "8"],
            OUT / "gmpd.log",
        )
    )
    procs.append(start_bin("falcon-gcpd", ["--listen", "0.0.0.0:18501"], OUT / "gcpd.log"))
    time.sleep(0.6)
    st = http_json(f"{GMP}/v1/status")
    print("GMP status", st, flush=True)
    print("GCP status", http_json(f"{GCP}/v1/status"), flush=True)

    n = 3
    sc.SINGLE_PACKER = False
    sc.PACK_OPEN = 0
    sc.stop_all()
    sc.wipe_data()
    sc.ensure_keys(n)
    sc.sh(f"rsync -az {sc.BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)
    sc.start_mesh(n, 9303)
    time.sleep(3)

    # smoke: owner-route one tx so we know mesh packs before GMP flood
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
    print("smoke submit", acct, s, "->", ovi, (d.get("result") or {}).get("engine_result"), flush=True)
    time.sleep(2)
    smoke_h = sc.health(n, sc.snap_vals(n))
    print("smoke health", smoke_h, flush=True)
    if (smoke_h.get("exec_min") or 0) < 1:
        print("FATAL: mesh smoke exec failed — abort GMP live", flush=True)
        for p in procs:
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except Exception:
                p.terminate()
        return 2

    stop = threading.Event()
    stats: dict = {}
    threading.Thread(target=capacity_loop, args=(stop, n), daemon=True).start()
    time.sleep(0.8)  # adverts land
    threading.Thread(target=issuer_loop, args=(stop, n, stats), daemon=True).start()
    time.sleep(0.3)

    before = sc.snap_vals(n)
    t0 = time.time()
    print(f"client flood → GMP only (rate={RATE}, hold={HOLD_S}s, max_inflight={MAX_INFLIGHT_PER_ACCT})", flush=True)
    client_flood(HOLD_S, RATE, stats, n)
    print(f"drain wait {DRAIN_S}s…", flush=True)
    time.sleep(DRAIN_S)
    after = sc.snap_vals(n)
    elapsed = time.time() - t0
    hb, ha = sc.health(n, before), sc.health(n, after)
    deltas = []
    for i in range(1, n + 1):
        b = before.get(f"val{i}", {}).get("exec")
        a = after.get(f"val{i}", {}).get("exec")
        if isinstance(b, int) and isinstance(a, int):
            deltas.append(a - b)
    exec_tps_min = (min(deltas) / elapsed) if deltas else 0.0

    gmp_st = http_json(f"{GMP}/v1/status")
    gcp_st = http_json(f"{GCP}/v1/status")
    lease_shares = {k: v for k, v in stats.items() if k.startswith("lease_to_")}

    report = {
        "suite": "gmp_live_fullstack",
        "design": "docs/GLOBAL_MEMPOOL_CONF_POOL_DESIGN.md",
        "elapsed_s": round(elapsed, 2),
        "stats": stats,
        "lease_shares": lease_shares,
        "exec_tps_min": round(exec_tps_min, 2),
        "exec_delta": deltas,
        "health_before": hb,
        "health_after": ha,
        "gmp_status": gmp_st,
        "gcp_status": gcp_st,
        "smoke_exec": smoke_h.get("exec_min"),
        "pass": (
            exec_tps_min >= 3.0
            and ha.get("vals_up") == 3
            and (ha.get("lag_max") or 0) <= 5
            and (ha.get("exec_spread") or 0) <= 5
            and stats.get("gmp_ok", 0) > 50
            and stats.get("submit_ok", 0) > 30
            and len(lease_shares) >= 2
        ),
    }
    if "lease_to_val1" in lease_shares and "lease_to_val3" in lease_shares:
        report["capacity_skew_ok"] = lease_shares["lease_to_val1"] >= lease_shares["lease_to_val3"]
    else:
        report["capacity_skew_ok"] = False

    report["pass"] = bool(report["pass"] and report.get("capacity_skew_ok", True))

    (OUT / "GMP_LIVE_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")
    md = [
        "# GMP live full-stack report",
        "",
        f"**Pass:** {report['pass']}",
        f"exec_tps_min={report['exec_tps_min']} gmp_ok={stats.get('gmp_ok')} "
        f"submit_ok={stats.get('submit_ok')} lease_shares={lease_shares}",
        f"capacity_skew_ok={report.get('capacity_skew_ok')} health_after={ha}",
        f"gmp={gmp_st}",
        f"gcp={gcp_st}",
        "",
    ]
    (OUT / "GMP_LIVE_REPORT.md").write_text("\n".join(md))
    print(json.dumps(report, indent=2), flush=True)

    stop.set()
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            p.terminate()
    print(f"wrote {OUT}/GMP_LIVE_REPORT.json mesh left up N=3", flush=True)
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
