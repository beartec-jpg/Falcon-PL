#!/usr/bin/env python3
"""Boot 2 or 3 independent N=3 multipack shards for lab stack."""
from __future__ import annotations

import argparse
import importlib.util
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCALE = ROOT / "real-mesh" / "07-scale-n-curve.py"
MESH = Path.home() / "falcon-dag-mesh"
BIN = MESH / "bin" / "falcon-dagd"

_spec = importlib.util.spec_from_file_location("sc", SCALE)
sc = importlib.util.module_from_spec(_spec)
assert _spec.loader
_spec.loader.exec_module(sc)


def shlex_quote(s: str) -> str:
    return "'" + s.replace("'", "'\"'\"'") + "'"


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
echo started {nid} pid=$(cat {MESH}/logs/{nid}.pid)
"""
    r = sc.sh(f"ssh {sc.SSH_F2} bash -s <<'EOF'\n{script}\nEOF")
    print(f"    remote {nid}: {(r.stdout or '').strip()}", flush=True)


def wait_rpc(nodes, timeout=55):
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
        time.sleep(0.4)
    raise SystemExit("mesh RPC not ready")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shards", type=int, default=2, choices=[2, 3])
    ap.add_argument(
        "--network-id-base",
        type=int,
        default=int(os.environ.get("NETWORK_ID_BASE", "9930")),
        help="Shard s uses network_id = base+s (public testnet: 9003)",
    )
    args = ap.parse_args()
    nshards = args.shards
    nid_base = args.network_id_base

    sc.wipe_data()
    sc.ensure_keys(9 if nshards == 3 else 6)
    # ensure val7-9 keys if needed
    if nshards == 3:
        keys = MESH / "keys"
        need = [f"val{i}.json" for i in range(7, 10)]
        if not all((keys / k).exists() for k in need):
            print("generating keys val1-9…", flush=True)
            unl = ",".join(f"val{i}" for i in range(1, 10))
            subprocess.check_call([str(BIN), "--gen-keys", "--keys-dir", str(keys), "--unl", unl])
            # restore arch if wiped
            if not (keys / "arch1.json").exists():
                tmp = Path("/tmp/arch-keys-boot")
                if tmp.exists():
                    import shutil
                    shutil.rmtree(tmp)
                tmp.mkdir()
                subprocess.check_call([str(BIN), "--gen-keys", "--keys-dir", str(tmp), "--unl", "arch1,arch2"])
                import shutil
                for n in ("arch1.json", "arch2.json"):
                    shutil.copy2(tmp / n, keys / n)
                import json
                pub = json.loads((keys / "unl_public.json").read_text())
                apub = json.loads((tmp / "unl_public.json").read_text())
                pub.update(apub)
                (keys / "unl_public.json").write_text(json.dumps(pub, indent=2) + "\n")

    sc.sh(f"rsync -az {BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)

    f1, f2 = sc.F1, sc.F2
    gmp = {
        0: f"http://{f1}:18500",
        1: f"http://{f1}:18510",
        2: f"http://{f1}:18520",
    }

    # port layout: shard s uses val 3s+1..3s+3 on 18000+id / 19000+id
    shards = []
    for s in range(nshards):
        base = s * 3
        unl = [f"val{base + i}" for i in range(1, 4)]
        # place: first two local, third remote when possible
        vals = []
        for i, nid in enumerate(unl):
            vi = base + i + 1
            host = f1 if i < 2 else f2
            if nshards == 3 and s == 2 and i == 0:
                host = f1
            vals.append((nid, host, 18000 + vi, 19000 + vi, i))
        arch = [
            (f"archS{s}a", f1, 18011 + s * 2, 19011 + s * 2),
            (f"archS{s}b", f2, 18012 + s * 2, 19012 + s * 2),
        ]
        # fix arch ports to not collide: s0: 18011/12, s1: 18013/14, s2: 18015/16
        arch = [
            (f"archS{s}a", f1, 18011 + s * 2, 19011 + s * 2),
            (f"archS{s}b", f2, 18012 + s * 2, 19012 + s * 2),
        ]
        shards.append({
            "unl": unl,
            "network_id": nid_base + s,
            "gmp": gmp[s],
            "vals": vals,
            "arch": arch,
        })

    def peers_for(shard, self_nid):
        hosts, ports = {}, {}
        for n, h, _r, p2p, *_ in [(v[0], v[1], v[2], v[3], v[4]) for v in shard["vals"]]:
            hosts[n], ports[n] = h, p2p
        for n, h, _r, p2p in shard["arch"]:
            hosts[n], ports[n] = h, p2p
        return ",".join(f"{hosts[n]}:{ports[n]}" for n in ports if n != self_nid)

    rpc_wait = []
    for shard in shards:
        unl, n = shard["unl"], len(shard["unl"])
        for nid, host, rpc, p2p, widx in shard["vals"]:
            cmd = node_cmd(
                nid, "full", "validator", rpc, p2p, peers_for(shard, nid),
                unl, widx, n, shard["network_id"], shard["gmp"],
            )
            start_node(cmd, nid, host == f2)
            rpc_wait.append((host, rpc))
        for nid, host, rpc, p2p in shard["arch"]:
            cmd = node_cmd(
                nid, "order-exec", "archival", rpc, p2p, peers_for(shard, nid),
                unl, 0, n, shard["network_id"], "",
            )
            start_node(cmd, nid, host == f2)
            rpc_wait.append((host, rpc))

    wait_rpc(rpc_wait)
    print(f"booted {nshards} shards", flush=True)


if __name__ == "__main__":
    main()
