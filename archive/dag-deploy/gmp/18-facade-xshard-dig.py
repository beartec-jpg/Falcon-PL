#!/usr/bin/env python3
"""
Facade + cross-shard dig on dual independent N=3 shards.

  Clients → falcon-gmp-facade :18499
              ├ same-shard → shard GMP
              └ cross-shard → debit source → (wait exec) → credit dest

Setup mirrors 17-shard-v0 dual mesh + two gmpd + facade.
"""
from __future__ import annotations

import importlib.util
import json
import os
import signal
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCALE = ROOT / "real-mesh" / "07-scale-n-curve.py"
MESH = Path.home() / "falcon-dag-mesh"
BIN = MESH / "bin" / "falcon-dagd"
MESH_BIN = MESH / "bin"
RS = Path.home() / "falcon-dag" / "falcon-dag-rs"
OUT = Path(os.environ.get("OUT", f"/tmp/facade-xshard-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

_spec = importlib.util.spec_from_file_location("sc", SCALE)
sc = importlib.util.module_from_spec(_spec)
assert _spec.loader
_spec.loader.exec_module(sc)


def http_json(url, method="GET", body=None, timeout=8.0):
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
    time.sleep(0.5)


def node_cmd(nid, role, profile, rpc, p2p, peers, unl, widx, wcnt, network_id, gmp_url):
    force = ["--force-node-profile"] if profile != "validator" else []
    cmd = [
        str(BIN), "--id", nid, "--role", role, "--node-profile", profile, *force,
        "--rpc", f"0.0.0.0:{rpc}", "--p2p", f"0.0.0.0:{p2p}", "--peers", peers,
        "--unl", ",".join(unl), "--worker-index", str(widx), "--worker-count", str(wcnt),
        "--network-id", str(network_id), "--crypto", "falcon-512",
        "--keys-dir", str(MESH / "keys"), "--wallet-dir", str(MESH / "wallets"),
        "--require-tx-sigs", "--auto-sign", "--single-packer=false",
        "--adaptive-batch=true", "--batch-interval", "0.05", "--max-batch-txs", "64",
        "--max-batch-parents", "3", "--unl-threshold", "0.6",
        "--max-uncertified-per-creator", "8", "--pack-open-cap=0",
        "--uncertified-ttl", "0", "--open-body-repool-after", "8",
        "--admit-ahead-cap", "2000", "--admit-busy-uncert-age", "0",
        "--conf-pool", "true", "--pack-check-slot", "0.25",
        "--snapshot-interval", "15", "--data-dir", str(MESH / "data" / nid),
    ]
    if profile == "validator" and gmp_url:
        cmd += [
            "--gmp-url", gmp_url, "--gmp-lease-max", "4",
            "--gmp-budget-mb", "4096", "--gmp-max-in-flight", "32", "--gmp-affinity=true",
        ]
    return cmd


def start_node(cmd, nid, remote):
    if not remote:
        sc.start_local(cmd, nid)
        return
    remote_cmd = " ".join(shlex_quote(c) for c in cmd)
    script = f"""
set -e
mkdir -p {MESH}/data/{nid} {MESH}/logs
nohup {remote_cmd} > {MESH}/logs/{nid}.log 2>&1 &
echo $! > {MESH}/logs/{nid}.pid
"""
    sc.sh(f"ssh {sc.SSH_F2} bash -s <<'EOF'\n{script}\nEOF")
    print(f"    remote {nid}", flush=True)


def wait_rpc(nodes, timeout=50):
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
    raise RuntimeError("rpc not ready")


def rpc_seq(host, port, account):
    r = sc.rpc(host, port, "account_info", {"account": account}, timeout=3)
    res = r.get("result") or {}
    return int(res.get("sequence") or 1)


def rpc_bal(host, port, account):
    r = sc.rpc(host, port, "account_info", {"account": account}, timeout=3)
    res = r.get("result") or {}
    # balance field names vary
    for k in ("balance", "balances", "native_balance", "fdag"):
        if k in res:
            v = res[k]
            if isinstance(v, (int, float)):
                return int(v)
            if isinstance(v, dict) and "FDAG" in v:
                return int(v["FDAG"])
    bals = res.get("balances") or res.get("assets") or {}
    if isinstance(bals, dict):
        for k in ("FDAG", "fdag", "native"):
            if k in bals:
                return int(bals[k])
    return None


def start_dual_mesh(gmp0, gmp1):
    stop_all()
    sc.wipe_data()
    sc.ensure_keys(6)
    sc.sh(f"rsync -az {BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)

    shard0 = {
        "unl": ["val1", "val2", "val3"],
        "network_id": 9920,
        "gmp": gmp0,
        "vals": [
            ("val1", sc.F1, 18001, 19001, 0),
            ("val2", sc.F1, 18002, 19002, 1),
            ("val3", sc.F2, 18003, 19003, 2),
        ],
        "arch": [("archA1", sc.F1, 18011, 19011), ("archA2", sc.F2, 18012, 19012)],
    }
    shard1 = {
        "unl": ["val4", "val5", "val6"],
        "network_id": 9921,
        "gmp": gmp1,
        "vals": [
            ("val4", sc.F1, 18004, 19004, 0),
            ("val5", sc.F1, 18005, 19005, 1),
            ("val6", sc.F2, 18006, 19006, 2),
        ],
        "arch": [("archB1", sc.F1, 18013, 19013), ("archB2", sc.F2, 18014, 19014)],
    }

    def peers_for(shard, self_nid):
        hosts, ports = {}, {}
        for n, h, _r, p2p, *_ in [(v[0], v[1], v[2], v[3], v[4]) for v in shard["vals"]]:
            hosts[n], ports[n] = h, p2p
        for n, h, _r, p2p in shard["arch"]:
            hosts[n], ports[n] = h, p2p
        return ",".join(f"{hosts[n]}:{ports[n]}" for n in ports if n != self_nid)

    rpc_wait = []
    for shard in (shard0, shard1):
        unl, n = shard["unl"], len(shard["unl"])
        for nid, host, rpc, p2p, widx in shard["vals"]:
            cmd = node_cmd(
                nid, "full", "validator", rpc, p2p, peers_for(shard, nid),
                unl, widx, n, shard["network_id"], shard["gmp"],
            )
            start_node(cmd, nid, host == sc.F2)
            rpc_wait.append((host, rpc))
        for nid, host, rpc, p2p in shard["arch"]:
            cmd = node_cmd(
                nid, "order-exec", "archival", rpc, p2p, peers_for(shard, nid),
                unl, 0, n, shard["network_id"], "",
            )
            start_node(cmd, nid, host == sc.F2)
            rpc_wait.append((host, rpc))
    wait_rpc(rpc_wait)
    time.sleep(2)
    return shard0, shard1


def main():
    print(f"OUT={OUT}", flush=True)
    for n in ("falcon-dagd", "falcon-gmpd", "falcon-gmp-facade"):
        src = RS / "target" / "release" / n
        if src.exists():
            subprocess.check_call(["cp", "-f", str(src), str(MESH_BIN / n)])

    procs = []
    for port in (18500, 18510, 18499):
        subprocess.call(f"fuser -k {port}/tcp 2>/dev/null || true", shell=True)
    time.sleep(0.3)
    procs.append(start_bin("falcon-gmpd", ["--listen", "0.0.0.0:18500", "--shards", "16", "--lease-ttl-s", "8"], OUT / "gmp0.log"))
    procs.append(start_bin("falcon-gmpd", ["--listen", "0.0.0.0:18510", "--shards", "16", "--lease-ttl-s", "8"], OUT / "gmp1.log"))
    time.sleep(0.4)

    gmp0 = f"http://{sc.F1}:18500"
    gmp1 = f"http://{sc.F1}:18510"
    facade = f"http://127.0.0.1:18499"

    shard0, shard1 = start_dual_mesh(gmp0, gmp1)

    # facade with routes
    procs.append(
        start_bin(
            "falcon-gmp-facade",
            [
                "--listen", "0.0.0.0:18499",
                "--xshard-timeout-s", "40",
                "--shard", f"id=0,name=s0,gmp={gmp0},rpc=http://{sc.F1}:18001,bridge=acct20,lo=1,hi=20",
                "--shard", f"id=1,name=s1,gmp={gmp1},rpc=http://{sc.F1}:18004,bridge=acct40,lo=21,hi=40",
            ],
            OUT / "facade.log",
        )
    )
    time.sleep(0.5)
    print("facade status", http_json(f"{facade}/v1/status"), flush=True)

    # --- route checks ---
    r5 = http_json(f"{facade}/v1/route", "POST", {"account": "acct5"})
    r25 = http_json(f"{facade}/v1/route", "POST", {"account": "acct25"})
    print("route acct5", r5, "acct25", r25, flush=True)
    route_ok = r5.get("shard_id") == 0 and r25.get("shard_id") == 1

    # --- same-shard flood sample ---
    same_ok = 0
    same_err = 0
    seq = {f"acct{a}": rpc_seq(sc.F1, 18001, f"acct{a}") for a in range(1, 6)}
    for _ in range(20):
        acct = min(seq.keys(), key=lambda a: seq[a])
        s = seq[acct]
        dest = "acct10" if acct != "acct10" else "acct11"
        try:
            r = http_json(
                f"{facade}/v1/submit",
                "POST",
                {"account": acct, "sequence": s, "destination": dest, "amount": 1, "fee": 10},
            )
            if r.get("error") or r.get("route") != "same_shard":
                same_err += 1
            else:
                same_ok += 1
                seq[acct] = s + 1
        except Exception:
            same_err += 1
    time.sleep(8)
    h0 = sc.metrics(sc.F1, 18001).get("executed_txs")
    print(f"same-shard submits ok={same_ok} err={same_err} val1_exec={h0}", flush=True)

    # --- cross-shard: acct5 (s0) → acct25 (s1) ---
    # Use seq from chain for acct5
    s5 = rpc_seq(sc.F1, 18001, "acct5")
    bal25_before = rpc_bal(sc.F1, 18004, "acct25")
    print(f"xshard start acct5 seq={s5} → acct25 bal_before={bal25_before}", flush=True)
    xs = http_json(
        f"{facade}/v1/submit",
        "POST",
        {"account": "acct5", "sequence": s5, "destination": "acct25", "amount": 5, "fee": 10},
        timeout=10,
    )
    print("xshard submit", json.dumps(xs)[:500], flush=True)
    xfer = xs.get("xfer_id")
    xshard_accept = xs.get("route") == "cross_shard" and bool(xfer)

    # poll status
    status_final = None
    for i in range(60):
        time.sleep(1)
        if not xfer:
            break
        try:
            status_final = http_json(f"{facade}/v1/xshard/{xfer}")
        except Exception as e:
            status_final = {"error": str(e)}
        st = (status_final or {}).get("status")
        if i % 5 == 0:
            print(f"  xshard poll {i}s status={st}", flush=True)
        if st in ("credited", "credited_rpc", "failed", "timeout_waiting_source_exec"):
            break

    bal25_after = rpc_bal(sc.F1, 18004, "acct25")
    s5_after = rpc_seq(sc.F1, 18001, "acct5")
    credit_ok = (status_final or {}).get("status") in ("credited", "credited_rpc")
    debit_exec = s5_after > s5
    # balance may be None if field shape unknown — then rely on status
    bal_ok = True
    if bal25_before is not None and bal25_after is not None:
        bal_ok = bal25_after >= bal25_before + 5

    facade_st = http_json(f"{facade}/v1/status")
    pass_all = (
        route_ok
        and same_ok >= 10
        and xshard_accept
        and credit_ok
        and debit_exec
        and bal_ok
    )

    report = {
        "suite": "facade_xshard",
        "pass": pass_all,
        "route_ok": route_ok,
        "same_shard": {"ok": same_ok, "err": same_err},
        "xshard": {
            "accept": xshard_accept,
            "xfer_id": xfer,
            "status": status_final,
            "debit_exec": debit_exec,
            "seq_before": s5,
            "seq_after": s5_after,
            "bal25_before": bal25_before,
            "bal25_after": bal25_after,
            "credit_ok": credit_ok,
            "bal_ok": bal_ok,
        },
        "facade_stats": facade_st.get("stats"),
    }
    (OUT / "FACADE_XSHARD_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")
    md = [
        "# GMP facade + cross-shard dig",
        "",
        f"**Pass:** {pass_all}",
        "",
        f"- route_ok={route_ok} (acct5→s0, acct25→s1)",
        f"- same_shard ok/err={same_ok}/{same_err}",
        f"- xshard accept={xshard_accept} credit_ok={credit_ok} debit_exec={debit_exec}",
        f"- acct5 seq {s5}→{s5_after} acct25 bal {bal25_before}→{bal25_after}",
        f"- final status: `{(status_final or {}).get('status')}`",
        f"- facade stats: `{facade_st.get('stats')}`",
        "",
        "Cross-shard lab protocol: source pays bridge → wait exec → dest bridge pays recipient.",
        "",
    ]
    (OUT / "FACADE_XSHARD_REPORT.md").write_text("\n".join(md))
    print("\n" + "\n".join(md), flush=True)
    print(json.dumps(report, indent=2), flush=True)

    stop_all()
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            p.terminate()
    return 0 if pass_all else 1


if __name__ == "__main__":
    sys.exit(main())
