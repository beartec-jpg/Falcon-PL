#!/usr/bin/env python3
"""
Shard v0 dig: independent parallel chains (no cross-shard bridge yet).

A) Single shard N=3, accounts acct1-40, client rate R
B) Two shards N=3 each:
     shard0: val1-3, acct1-20,  GMP :18500, network_id 9910, rate R/2
     shard1: val4-6, acct21-40, GMP :18510, network_id 9911, rate R/2

North-star: exec_tps_min(A)  vs  exec_tps_min(shard0)+exec_tps_min(shard1)
Hypothesis: shards add capacity (sum B > A) without growing one UNL.
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
OUT = Path(os.environ.get("OUT", f"/tmp/shard-v0-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

HOLD = float(os.environ.get("HOLD", "35"))
RATE = int(os.environ.get("RATE", "80"))  # total client rate
DRAIN = float(os.environ.get("DRAIN", "18"))
MAX_INFLIGHT = int(os.environ.get("GMP_MAX_INFLIGHT", "3"))

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


def shlex_quote(s: str) -> str:
    return "'" + s.replace("'", "'\"'\"'") + "'"


def start_bin(name, args, log):
    p = subprocess.Popen(
        [str(MESH_BIN / name), *args],
        stdout=open(log, "w"),
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    print(f"started {name} pid={p.pid}", flush=True)
    return p


def stop_all():
    sc.stop_all()
    subprocess.call("pkill -x falcon-dagd 2>/dev/null || true", shell=True)
    sc.sh(f"ssh {sc.SSH_F2} 'pkill -x falcon-dagd 2>/dev/null || true'", check=False)
    time.sleep(0.6)


def client_flood(gmp: str, hold: float, rate: int, accounts: list[str], stats: dict, dests: dict):
    t0 = time.time()
    # seed sequences via any host - use account_info if possible, else 1
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
        dest = dests.get(acct, accounts[-1] if accounts[-1] != acct else accounts[0])
        try:
            r = http_json(
                f"{gmp}/v1/submit",
                "POST",
                {
                    "account": acct,
                    "sequence": s,
                    "destination": dest,
                    "amount": 1,
                    "fee": 10,
                },
                timeout=3,
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
                        inflight[a] -= 1

    threading.Thread(target=decay, daemon=True).start()
    with ThreadPoolExecutor(max_workers=10) as ex:
        while time.time() - t0 < hold:
            sec0 = time.time()
            futs = [ex.submit(one) for _ in range(rate)]
            for f in as_completed(futs):
                _ = f.result()
            left = 1.0 - (time.time() - sec0)
            if left > 0:
                time.sleep(left)


def node_cmd(
    nid: str,
    role: str,
    profile: str,
    rpc: int,
    p2p: int,
    peers: str,
    unl: list[str],
    widx: int,
    wcnt: int,
    network_id: int,
    gmp_url: str,
) -> list[str]:
    force = ["--force-node-profile"] if profile != "validator" else []
    cmd = [
        str(BIN),
        "--id",
        nid,
        "--role",
        role,
        "--node-profile",
        profile,
        *force,
        "--rpc",
        f"0.0.0.0:{rpc}",
        "--p2p",
        f"0.0.0.0:{p2p}",
        "--peers",
        peers,
        "--unl",
        ",".join(unl),
        "--worker-index",
        str(widx),
        "--worker-count",
        str(wcnt),
        "--network-id",
        str(network_id),
        "--crypto",
        "falcon-512",
        "--keys-dir",
        str(MESH / "keys"),
        "--wallet-dir",
        str(MESH / "wallets"),
        "--require-tx-sigs",
        "--auto-sign",
        "--single-packer=false",
        "--adaptive-batch=true",
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
        "--pack-open-cap=0",
        "--uncertified-ttl",
        "0",
        "--open-body-repool-after",
        "8",
        "--admit-ahead-cap",
        "2000",
        "--admit-busy-uncert-age",
        "0",
        "--conf-pool",
        "true",
        "--pack-check-slot",
        "0.25",
        "--snapshot-interval",
        "15",
        "--data-dir",
        str(MESH / "data" / nid),
    ]
    if profile == "validator":
        cmd += [
            "--gmp-url",
            gmp_url,
            "--gmp-lease-max",
            "4",
            "--gmp-budget-mb",
            "4096",
            "--gmp-max-in-flight",
            "32",
            "--gmp-affinity=true",
        ]
    return cmd


def start_node(cmd: list[str], nid: str, remote: bool):
    if not remote:
        sc.start_local(cmd, nid)
        return
    remote_cmd = " ".join(shlex_quote(c) for c in cmd)
    script = f"""
set -e
mkdir -p {MESH}/data/{nid} {MESH}/logs
nohup {remote_cmd} > {MESH}/logs/{nid}.log 2>&1 &
echo $! > {MESH}/logs/{nid}.pid
echo started {nid} pid=$(cat {MESH}/logs/{nid}.pid)
"""
    r = sc.sh(f"ssh {sc.SSH_F2} bash -s <<'EOF'\n{script}\nEOF")
    print(f"    remote {nid}: {r.stdout.strip()}", flush=True)


def wait_rpc(nodes: list[tuple[str, int]], timeout=50):
    """nodes: (host, rpc_port)"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        ok = 0
        for host, port in nodes:
            try:
                sc.metrics(host, port)
                ok += 1
            except Exception:
                pass
        if ok == len(nodes):
            print(f"  all {ok} RPC up", flush=True)
            return
        time.sleep(0.5)
    raise RuntimeError("RPC not ready")


def snap_vals(specs: list[tuple[str, str, int]]) -> dict:
    """specs: (nid, host, rpc)"""
    out = {}
    for nid, host, rpc in specs:
        try:
            m = sc.metrics(host, rpc)
            ps = m.get("pack_stall") or {}
            out[nid] = {
                "exec": m.get("executed_txs") or 0,
                "lag": ps.get("admit_exec_lag") or 0,
                "root": (m.get("state_root") or "")[:16],
            }
        except Exception as e:
            out[nid] = {"error": str(e)}
    return out


def health(snap: dict, nids: list[str]) -> dict:
    execs, lags, roots = [], [], set()
    up = 0
    for nid in nids:
        s = snap.get(nid) or {}
        if "error" not in s:
            up += 1
            execs.append(int(s.get("exec") or 0))
            lags.append(s.get("lag") or 0)
            if s.get("root"):
                roots.add(s["root"])
    return {
        "vals_up": up,
        "exec_min": min(execs) if execs else 0,
        "exec_max": max(execs) if execs else 0,
        "exec_spread": (max(execs) - min(execs)) if execs else 0,
        "lag_max": max(lags) if lags else 0,
        "roots_agree": len(roots) <= 1,
    }


def run_single_shard(gmp: str, network_id: int, rate: int) -> dict:
    print("\n===== A) SINGLE shard N=3 all accounts =====", flush=True)
    stop_all()
    sc.wipe_data()
    sc.ensure_keys(6)
    sc.sh(f"rsync -az {BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)

    os.environ["GMP_URL"] = gmp
    os.environ.pop("GCP_URL", None)
    sc.SINGLE_PACKER = False
    sc.PACK_OPEN = 0
    sc.start_mesh(3, network_id)
    time.sleep(2)

    nids = [f"val{i}" for i in range(1, 4)]
    specs = [(f"val{i}", sc.host_for_val(3, i), sc.VAL_RPC[i]) for i in range(1, 4)]
    accts = [f"acct{a}" for a in range(1, 41)]
    dests = {a: ("acct40" if a != "acct40" else "acct39") for a in accts}

    before = snap_vals(specs)
    stats = {}
    t0 = time.time()
    print(f"  flood rate={rate} hold={HOLD}s → {gmp}", flush=True)
    client_flood(gmp, HOLD, rate, accts, stats, dests)
    time.sleep(DRAIN)
    after = snap_vals(specs)
    elapsed = time.time() - t0
    ha = health(after, nids)
    deltas = []
    for nid in nids:
        b = (before.get(nid) or {}).get("exec")
        a = (after.get(nid) or {}).get("exec")
        if isinstance(b, int) and isinstance(a, int):
            deltas.append(a - b)
    tps = (min(deltas) / elapsed) if deltas else 0.0
    return {
        "mode": "single_n3",
        "elapsed_s": round(elapsed, 2),
        "exec_tps_min": round(tps, 2),
        "exec_delta": deltas,
        "health": ha,
        "gmp_ok": stats.get("gmp_ok", 0),
        "gmp_err": stats.get("gmp_err", 0),
    }


def run_dual_shards(gmp0: str, gmp1: str, rate_total: int) -> dict:
    print("\n===== B) DUAL shards N=3 + N=3 (independent) =====", flush=True)
    stop_all()
    sc.wipe_data()
    sc.ensure_keys(6)
    sc.sh(f"rsync -az {BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)

    # Shard definitions
    # Placement: F1 val1,val2,val4,val5 archA1 archB1 ; F2 val3,val6 archA2 archB2
    shard0 = {
        "unl": ["val1", "val2", "val3"],
        "network_id": 9910,
        "gmp": gmp0,
        "vals": [
            ("val1", sc.F1, 18001, 19001, 0),
            ("val2", sc.F1, 18002, 19002, 1),
            ("val3", sc.F2, 18003, 19003, 2),
        ],
        "arch": [
            ("archA1", sc.F1, 18011, 19011),
            ("archA2", sc.F2, 18012, 19012),
        ],
        "accounts": [f"acct{a}" for a in range(1, 21)],
    }
    shard1 = {
        "unl": ["val4", "val5", "val6"],
        "network_id": 9911,
        "gmp": gmp1,
        "vals": [
            ("val4", sc.F1, 18004, 19004, 0),
            ("val5", sc.F1, 18005, 19005, 1),
            ("val6", sc.F2, 18006, 19006, 2),
        ],
        "arch": [
            ("archB1", sc.F1, 18013, 19013),
            ("archB2", sc.F2, 18014, 19014),
        ],
        "accounts": [f"acct{a}" for a in range(21, 41)],
    }

    def peers_for(shard, self_nid):
        ports = {n: p2p for n, _h, _r, p2p, *_ in [(v[0], v[1], v[2], v[3], v[4]) for v in shard["vals"]]}
        hosts = {n: h for n, h, _r, _p, *_ in [(v[0], v[1], v[2], v[3], v[4]) for v in shard["vals"]]}
        # arch peers
        for n, h, _r, p2p in shard["arch"]:
            ports[n] = p2p
            hosts[n] = h
        out = []
        for n, p2p in ports.items():
            if n == self_nid:
                continue
            out.append(f"{hosts[n]}:{p2p}")
        return ",".join(out)

    # Start both shards
    rpc_wait = []
    for shard in (shard0, shard1):
        unl = shard["unl"]
        n = len(unl)
        for nid, host, rpc, p2p, widx in shard["vals"]:
            peers = peers_for(shard, nid)
            cmd = node_cmd(
                nid, "full", "validator", rpc, p2p, peers, unl, widx, n, shard["network_id"], shard["gmp"]
            )
            start_node(cmd, nid, remote=(host == sc.F2))
            rpc_wait.append((host, rpc))
        for i, (nid, host, rpc, p2p) in enumerate(shard["arch"]):
            peers = peers_for(shard, nid)
            cmd = node_cmd(
                nid,
                "order-exec",
                "archival",
                rpc,
                p2p,
                peers,
                unl,
                0,
                n,
                shard["network_id"],
                "",  # no gmp on arch
            )
            # strip gmp if empty - node_cmd only adds gmp for validator
            start_node(cmd, nid, remote=(host == sc.F2))
            rpc_wait.append((host, rpc))

    wait_rpc(rpc_wait)
    time.sleep(2)

    rate_each = max(1, rate_total // 2)
    specs0 = [(n, h, r) for n, h, r, _p, _w in shard0["vals"]]
    specs1 = [(n, h, r) for n, h, r, _p, _w in shard1["vals"]]

    before0, before1 = snap_vals(specs0), snap_vals(specs1)
    stats0, stats1 = {}, {}
    dests0 = {a: ("acct20" if a != "acct20" else "acct19") for a in shard0["accounts"]}
    dests1 = {a: ("acct40" if a != "acct40" else "acct39") for a in shard1["accounts"]}

    t0 = time.time()
    print(f"  dual flood {rate_each}/s each shard hold={HOLD}s", flush=True)

    def flood0():
        client_flood(gmp0, HOLD, rate_each, shard0["accounts"], stats0, dests0)

    def flood1():
        client_flood(gmp1, HOLD, rate_each, shard1["accounts"], stats1, dests1)

    t_a = threading.Thread(target=flood0)
    t_b = threading.Thread(target=flood1)
    t_a.start()
    t_b.start()
    t_a.join()
    t_b.join()
    time.sleep(DRAIN)
    elapsed = time.time() - t0

    after0, after1 = snap_vals(specs0), snap_vals(specs1)
    h0 = health(after0, shard0["unl"])
    h1 = health(after1, shard1["unl"])

    def tps(before, after, unl):
        deltas = []
        for nid in unl:
            b = (before.get(nid) or {}).get("exec")
            a = (after.get(nid) or {}).get("exec")
            if isinstance(b, int) and isinstance(a, int):
                deltas.append(a - b)
        return (min(deltas) / elapsed) if deltas else 0.0, deltas

    tps0, d0 = tps(before0, after0, shard0["unl"])
    tps1, d1 = tps(before1, after1, shard1["unl"])
    return {
        "mode": "dual_shard",
        "elapsed_s": round(elapsed, 2),
        "shard0": {
            "exec_tps_min": round(tps0, 2),
            "exec_delta": d0,
            "health": h0,
            "gmp_ok": stats0.get("gmp_ok", 0),
            "gmp_err": stats0.get("gmp_err", 0),
            "accounts": "acct1-20",
        },
        "shard1": {
            "exec_tps_min": round(tps1, 2),
            "exec_delta": d1,
            "health": h1,
            "gmp_ok": stats1.get("gmp_ok", 0),
            "gmp_err": stats1.get("gmp_err", 0),
            "accounts": "acct21-40",
        },
        "combined_exec_tps_min_sum": round(tps0 + tps1, 2),
    }


def main():
    print(f"OUT={OUT} F1={sc.F1} F2={sc.F2}", flush=True)
    for n in ("falcon-dagd", "falcon-gmpd", "falcon-gcpd"):
        src = RS / "target" / "release" / n
        if src.exists():
            subprocess.check_call(["cp", "-f", str(src), str(MESH_BIN / n)])

    procs = []
    for port in (18500, 18510, 18501):
        subprocess.call(f"fuser -k {port}/tcp 2>/dev/null || true", shell=True)
    time.sleep(0.3)
    procs.append(
        start_bin(
            "falcon-gmpd",
            ["--listen", "0.0.0.0:18500", "--shards", "16", "--lease-ttl-s", "8"],
            OUT / "gmp0.log",
        )
    )
    procs.append(
        start_bin(
            "falcon-gmpd",
            ["--listen", "0.0.0.0:18510", "--shards", "16", "--lease-ttl-s", "8"],
            OUT / "gmp1.log",
        )
    )
    time.sleep(0.5)
    gmp0 = f"http://{sc.F1}:18500"
    gmp1 = f"http://{sc.F1}:18510"

    a = run_single_shard(gmp0, 9901, RATE)
    print(f"  A tps_min={a['exec_tps_min']} gmp_ok={a['gmp_ok']}", flush=True)

    b = run_dual_shards(gmp0, gmp1, RATE)
    print(
        f"  B shard0={b['shard0']['exec_tps_min']} shard1={b['shard1']['exec_tps_min']} "
        f"sum={b['combined_exec_tps_min_sum']}",
        flush=True,
    )

    sum_b = b["combined_exec_tps_min_sum"]
    tps_a = a["exec_tps_min"]
    if sum_b > tps_a * 1.15:
        verdict = f"SHARD_V0_HELPS: dual sum {sum_b} > single {tps_a} (>+15%)"
        win = True
    elif sum_b >= tps_a * 0.95:
        verdict = f"SHARD_V0_FLAT: dual sum {sum_b} ≈ single {tps_a}"
        win = False
    else:
        verdict = f"SHARD_V0_REGRESS: dual sum {sum_b} < single {tps_a}"
        win = False

    report = {
        "suite": "shard_v0_parallel",
        "design": "docs/OPEN_VALIDATORS_AND_SHARDS.md",
        "verdict": verdict,
        "win": win,
        "load_total_rate": RATE,
        "A_single": a,
        "B_dual": b,
        "note": "v0 = independent meshes, no cross-shard txs; proves additive capacity",
    }
    (OUT / "SHARD_V0_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")
    md = [
        "# Shard v0 dig (parallel independent chains)",
        "",
        f"**Verdict:** {verdict}",
        "",
        "No cross-shard bridge — only proves **parallel shards add capacity**.",
        "",
        f"| Mode | exec_tps_min | notes |",
        f"|------|--------------|-------|",
        f"| A single N=3 | **{tps_a}** | all accounts, rate {RATE}/s |",
        f"| B shard0 | **{b['shard0']['exec_tps_min']}** | acct1-20, rate {RATE//2}/s |",
        f"| B shard1 | **{b['shard1']['exec_tps_min']}** | acct21-40, rate {RATE//2}/s |",
        f"| **B sum** | **{sum_b}** | capacity if load is shard-local |",
        "",
        f"A health={a.get('health')} gmp_ok={a.get('gmp_ok')}",
        f"B0 health={b['shard0'].get('health')} B1 health={b['shard1'].get('health')}",
        "",
        "Open validators: see docs/OPEN_VALIDATORS_AND_SHARDS.md (risks + mitigations).",
        "",
    ]
    (OUT / "SHARD_V0_REPORT.md").write_text("\n".join(md))
    print("\n" + "\n".join(md), flush=True)

    stop_all()
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            p.terminate()
    print(f"wrote {OUT}/SHARD_V0_REPORT.json", flush=True)
    return 0 if win else 0  # dig always 0; verdict in report


if __name__ == "__main__":
    sys.exit(main())
