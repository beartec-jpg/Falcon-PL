#!/usr/bin/env python3
"""
3-shard capacity dig vs 1-shard baseline.

A) single N=3, all acct1-40, rate R
B) three shards N=3 each, accounts 1-14 / 15-28 / 29-40, rate R/3 each

North-star: sum(exec_tps_min shards) > single exec_tps_min
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
MESH = Path.home() / "falcon-dag-mesh"
BIN = MESH / "bin" / "falcon-dagd"
MESH_BIN = MESH / "bin"
RS = Path.home() / "falcon-dag" / "falcon-dag-rs"
OUT = Path(os.environ.get("OUT", f"/tmp/shard3-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

HOLD = float(os.environ.get("HOLD", "30"))
RATE = int(os.environ.get("RATE", "90"))
DRAIN = float(os.environ.get("DRAIN", "15"))
MAX_INFLIGHT = 3

_spec = importlib.util.spec_from_file_location("sc", SCALE)
sc = importlib.util.module_from_spec(_spec)
assert _spec.loader
_spec.loader.exec_module(sc)


def http_json(url, method="GET", body=None, timeout=5.0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data, method=method, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def start_bin(name, args, log):
    p = subprocess.Popen(
        [str(MESH_BIN / name), *args],
        stdout=open(log, "w"),
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    print(f"started {name} pid={p.pid}", flush=True)
    return p


def client_flood(gmp, hold, rate, accounts, stats):
    t0 = time.time()
    seq = {a: 1 for a in accounts}
    inflight = {a: 0 for a in accounts}
    lock = threading.Lock()

    def one():
        with lock:
            cands = [a for a in accounts if inflight[a] < MAX_INFLIGHT]
            if not cands:
                return
            acct = min(cands, key=lambda a: seq[a])
            s = seq[acct]
            seq[acct] = s + 1
            inflight[acct] += 1
        dest = accounts[-1] if acct != accounts[-1] else accounts[0]
        try:
            r = http_json(
                f"{gmp}/v1/submit",
                "POST",
                {"account": acct, "sequence": s, "destination": dest, "amount": 1, "fee": 10},
                timeout=3,
            )
            with lock:
                if r.get("error"):
                    stats["err"] = stats.get("err", 0) + 1
                    if seq[acct] == s + 1:
                        seq[acct] = s
                    inflight[acct] = max(0, inflight[acct] - 1)
                else:
                    stats["ok"] = stats.get("ok", 0) + 1
        except Exception:
            with lock:
                stats["err"] = stats.get("err", 0) + 1
                if seq[acct] == s + 1:
                    seq[acct] = s
                inflight[acct] = max(0, inflight[acct] - 1)

    def decay():
        while time.time() - t0 < hold + 1:
            time.sleep(0.35)
            with lock:
                for a in inflight:
                    if inflight[a] > 0:
                        inflight[a] -= 1

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
    for n in ("falcon-dagd", "falcon-gmpd"):
        src = RS / "target" / "release" / n
        if src.exists():
            subprocess.check_call(["cp", "-f", str(src), str(MESH_BIN / n)])

    procs = []
    for port in (18500, 18510, 18520):
        subprocess.call(f"fuser -k {port}/tcp 2>/dev/null || true", shell=True)
    time.sleep(0.3)

    # --- A single ---
    subprocess.check_call([str(ROOT / "gmp" / "stop-stack.sh")])
    procs.append(start_bin("falcon-gmpd", ["--listen", "0.0.0.0:18500", "--shards", "16", "--lease-ttl-s", "8"], OUT / "gmp0.log"))
    time.sleep(0.4)
    os.environ["GMP_URL"] = f"http://{sc.F1}:18500"
    sc.SINGLE_PACKER = False
    sc.PACK_OPEN = 0
    sc.stop_all()
    sc.wipe_data()
    sc.ensure_keys(6)
    sc.sh(f"rsync -az {BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)
    sc.start_mesh(3, 9940)
    time.sleep(2)
    before = {i: sc.metrics(sc.host_for_val(3, i), sc.VAL_RPC[i]).get("executed_txs") or 0 for i in range(1, 4)}
    st = {}
    t0 = time.time()
    client_flood(f"http://{sc.F1}:18500", HOLD, RATE, [f"acct{a}" for a in range(1, 41)], st)
    time.sleep(DRAIN)
    elapsed = time.time() - t0
    after = {i: sc.metrics(sc.host_for_val(3, i), sc.VAL_RPC[i]).get("executed_txs") or 0 for i in range(1, 4)}
    d = [after[i] - before[i] for i in range(1, 4)]
    tps_a = min(d) / elapsed if elapsed else 0
    print(f"A single tps_min={tps_a:.2f} deltas={d} gmp_ok={st.get('ok')}", flush=True)
    single = {"exec_tps_min": round(tps_a, 2), "deltas": d, "gmp_ok": st.get("ok", 0)}

    # --- B triple ---
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            pass
    procs = []
    subprocess.check_call([str(ROOT / "gmp" / "stop-stack.sh")])
    for port, log in ((18500, "g0"), (18510, "g1"), (18520, "g2")):
        procs.append(start_bin("falcon-gmpd", ["--listen", f"0.0.0.0:{port}", "--shards", "16", "--lease-ttl-s", "8"], OUT / f"{log}.log"))
    time.sleep(0.5)
    subprocess.check_call([sys.executable, str(ROOT / "gmp" / "boot_mesh.py"), "--shards", "3"])

    ranges = [
        (0, f"http://{sc.F1}:18500", [f"acct{a}" for a in range(1, 15)], [18001, 18002, 18003]),
        (1, f"http://{sc.F1}:18510", [f"acct{a}" for a in range(15, 29)], [18004, 18005, 18006]),
        (2, f"http://{sc.F1}:18520", [f"acct{a}" for a in range(29, 41)], [18007, 18008, 18009]),
    ]
    hosts_ports = []
    for sid, _g, _ac, ports in ranges:
        for i, port in enumerate(ports):
            # val placement: first two F1, third F2 for each shard of 3
            host = sc.F1 if i < 2 else sc.F2
            hosts_ports.append((sid, host, port))

    def snap():
        out = {}
        for sid, host, port in hosts_ports:
            try:
                out[(sid, port)] = sc.metrics(host, port).get("executed_txs") or 0
            except Exception:
                out[(sid, port)] = None
        return out

    before_b = snap()
    rate_each = max(1, RATE // 3)
    stats = [{}, {}, {}]
    t0 = time.time()

    def flood_i(i):
        _sid, gmp, accts, _ = ranges[i]
        client_flood(gmp, HOLD, rate_each, accts, stats[i])

    threads = [threading.Thread(target=flood_i, args=(i,)) for i in range(3)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    time.sleep(DRAIN)
    elapsed_b = time.time() - t0
    after_b = snap()

    shard_tps = []
    for sid in range(3):
        ports = ranges[sid][3]
        deltas = []
        for port in ports:
            b0, a0 = before_b.get((sid, port)), after_b.get((sid, port))
            if isinstance(b0, int) and isinstance(a0, int):
                deltas.append(a0 - b0)
        tps = (min(deltas) / elapsed_b) if deltas else 0.0
        shard_tps.append(round(tps, 2))
        print(f"  shard{sid} tps_min={tps:.2f} deltas={deltas} ok={stats[sid].get('ok')}", flush=True)

    sum_b = round(sum(shard_tps), 2)
    win = sum_b > single["exec_tps_min"] * 1.1
    verdict = (
        f"SHARD3_HELPS: sum {sum_b} > single {single['exec_tps_min']}"
        if win
        else f"SHARD3_FLAT_OR_LESS: sum {sum_b} vs single {single['exec_tps_min']}"
    )
    report = {
        "suite": "shard3_capacity",
        "verdict": verdict,
        "win": win,
        "A_single": single,
        "B_shards_tps": shard_tps,
        "B_sum": sum_b,
        "load": {"rate": RATE, "hold": HOLD},
    }
    (OUT / "SHARD3_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")
    md = [
        "# 3-shard capacity dig",
        "",
        f"**Verdict:** {verdict}",
        "",
        f"| Mode | exec_tps_min |",
        f"|------|--------------|",
        f"| A single N=3 | **{single['exec_tps_min']}** |",
        f"| B s0 / s1 / s2 | {shard_tps[0]} / {shard_tps[1]} / {shard_tps[2]} |",
        f"| **B sum** | **{sum_b}** |",
        "",
    ]
    (OUT / "SHARD3_REPORT.md").write_text("\n".join(md))
    print("\n" + "\n".join(md), flush=True)

    subprocess.call([str(ROOT / "gmp" / "stop-stack.sh")])
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
