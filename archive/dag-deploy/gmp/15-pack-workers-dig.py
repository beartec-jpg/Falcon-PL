#!/usr/bin/env python3
"""
Dig: non-UNL pack workers + UNL-only certs.

Compare:
  A) Classic N=3 multipack (UNL=packers=val1..3)
  B) Cert UNL=val1..3, packers=val1..3+pack1..3 (6 pack lanes, UNL also packs)
  C) Pure split: packers=pack1..3 only; UNL=val1..3 cert+exec only (never pack)

Path: clients → GMP → packer leases → pack bodies → UNL signs from conf pool.

North-star: does C (pure pack workers, fixed cert UNL) beat A on exec_tps_min?
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
MESH_BIN = MESH / "bin"
RS = Path.home() / "falcon-dag" / "falcon-dag-rs"
OUT = Path(os.environ.get("OUT", f"/tmp/pack-workers-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

HOLD = float(os.environ.get("HOLD", "30"))
RATE = int(os.environ.get("RATE", "50"))
DRAIN = float(os.environ.get("DRAIN", "15"))
MAX_INFLIGHT = int(os.environ.get("GMP_MAX_INFLIGHT", "3"))

_spec = importlib.util.spec_from_file_location("sc", SCALE)
sc = importlib.util.module_from_spec(_spec)
assert _spec.loader
_spec.loader.exec_module(sc)

VAL_RPC = {i: 18000 + i for i in range(1, 7)}
VAL_P2P = {i: 19000 + i for i in range(1, 7)}
# pack workers on ports 18004-18006 / 19004-19006 using indices 4,5,6 in maps
# reuse val maps: pack1→slot4 etc.
PACK_IDS = ["pack1", "pack2", "pack3"]
UNL_IDS = ["val1", "val2", "val3"]
ALL_PACKERS = UNL_IDS + PACK_IDS


def http_json(url, method="GET", body=None, timeout=5.0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data, method=method, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def start_bin(name, args, log):
    bin_path = MESH_BIN / name
    log.parent.mkdir(parents=True, exist_ok=True)
    lf = open(log, "w")
    p = subprocess.Popen(
        [str(bin_path), *args],
        stdout=lf,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    print(f"started {name} pid={p.pid}", flush=True)
    return p


def shlex_quote(s: str) -> str:
    return "'" + s.replace("'", "'\"'\"'") + "'"


def stop_all_extra():
    sc.stop_all()
    # also kill pack* if any
    subprocess.call("pkill -x falcon-dagd 2>/dev/null || true", shell=True)
    sc.sh(
        f"ssh {sc.SSH_F2} 'pkill -x falcon-dagd 2>/dev/null || true'",
        check=False,
    )
    time.sleep(0.5)


def client_flood(gmp, hold, rate, stats, n_for_seq=3):
    t0 = time.time()
    bank = sc.SeqBank(n_for_seq)
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
            time.sleep(0.4)
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


def node_cmd_custom(
    nid: str,
    role: str,
    profile: str,
    rpc: int,
    p2p: int,
    peers: str,
    widx: int,
    wcnt: int,
    network_id: int,
    unl: list[str],
    packers: list[str],
    gmp_url: str,
    gcp_url: str,
    enable_gmp: bool,
) -> list[str]:
    force = ["--force-node-profile"] if profile != "validator" else []
    cmd = [
        str(sc.BIN),
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
        "--packers",
        ",".join(packers),
        "--worker-index",
        str(widx),
        "--worker-count",
        str(wcnt),
        "--network-id",
        str(network_id),
        "--crypto",
        sc.CRYPTO,
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
    # GMP leases only on actual packers (UNL-only signers should not pull leases).
    if enable_gmp:
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
            "--gcp-url",
            gcp_url,
        ]
    elif gcp_url:
        # UNL still publish certs to GCP for observability
        cmd += ["--gcp-url", gcp_url]
    return cmd


def peer_list_ids(ids: list[tuple[str, str, int]], self_id: str) -> str:
    """ids: (node_id, host, p2p_port)"""
    peers = []
    for nid, host, port in ids:
        if nid == self_id:
            continue
        peers.append(f"{host}:{port}")
    return ",".join(peers)


def start_cmd(cmd: list[str], nid: str, remote: bool):
    if not remote:
        sc.start_local(cmd, nid)
    else:
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


def wait_rpc(nodes: list[tuple[str, str, int]], timeout=50):
    deadline = time.time() + timeout
    while time.time() < deadline:
        ok = 0
        for nid, host, rpc in nodes:
            try:
                sc.metrics(host, rpc)
                ok += 1
            except Exception:
                pass
        if ok == len(nodes):
            print(f"  all {ok} nodes up", flush=True)
            return
        time.sleep(0.5)
    raise RuntimeError("mesh not ready")


def snap_unl(unl: list[str], hosts: dict[str, str], rpcs: dict[str, int]) -> dict:
    out = {}
    for nid in unl:
        try:
            m = sc.metrics(hosts[nid], rpcs[nid])
            ps = m.get("pack_stall") or {}
            out[nid] = {
                "exec": m.get("executed_txs"),
                "lag": ps.get("admit_exec_lag"),
                "root": (m.get("state_root") or "")[:16],
                "batches": m.get("batches_created"),
            }
        except Exception as e:
            out[nid] = {"error": str(e)}
    return out


def health_unl(snap: dict, unl: list[str]) -> dict:
    execs = []
    lags = []
    roots = set()
    up = 0
    for nid in unl:
        s = snap.get(nid) or {}
        if "error" not in s:
            up += 1
            if isinstance(s.get("exec"), int):
                execs.append(s["exec"])
            lags.append(s.get("lag") or 0)
            if s.get("root"):
                roots.add(s["root"])
    return {
        "vals_up": up,
        "exec_min": min(execs) if execs else None,
        "exec_max": max(execs) if execs else None,
        "exec_spread": (max(execs) - min(execs)) if execs else None,
        "lag_max": max(lags) if lags else None,
        "roots_agree": len(roots) <= 1,
    }


def run_mode(name: str, unl: list[str], packers: list[str], gmp: str, gcp: str, network_id: int):
    print(f"\n===== MODE {name}: unl={unl} packers={packers} =====", flush=True)
    stop_all_extra()
    sc.wipe_data()
    # ensure keys for all packers exist
    need = [f"{x}.json" for x in packers + unl] + ["arch1.json", "arch2.json", "unl_public.json"]
    for k in need:
        if not (MESH / "keys" / k).exists() and k.startswith("pack"):
            raise SystemExit(f"missing key {k} — generate pack worker keys first")

    sc.sh(f"rsync -az {sc.BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)

    # Placement: F1 val1,val2,pack1 ; F2 val3,pack2,pack3 ; arch1 F1 arch2 F2
    place_f1 = {"val1", "val2", "pack1", "arch1"}
    # ports
    port_map = {
        "val1": (18001, 19001),
        "val2": (18002, 19002),
        "val3": (18003, 19003),
        "pack1": (18004, 19004),
        "pack2": (18005, 19005),
        "pack3": (18006, 19006),
        "arch1": (18011, 19011),
        "arch2": (18012, 19012),
    }
    hosts = {}
    for nid in set(unl) | set(packers) | {"arch1", "arch2"}:
        hosts[nid] = sc.F1 if nid in place_f1 or nid == "arch1" else sc.F2
        if nid == "val3":
            hosts[nid] = sc.F2
        if nid in ("pack2", "pack3", "arch2"):
            hosts[nid] = sc.F2
        if nid in ("val1", "val2", "pack1", "arch1"):
            hosts[nid] = sc.F1

    # For classic mode A only start unl packers (no pack1-3)
    active_packers = list(packers)
    active_nodes = list(dict.fromkeys(unl + active_packers + ["arch1", "arch2"]))

    # Build peer inventory for active packing+arch nodes
    inv = []
    for nid in active_nodes:
        inv.append((nid, hosts[nid], port_map[nid][1]))

    wcnt = max(1, len(active_packers))
    # worker index: rank in packer list
    widx = {p: i for i, p in enumerate(active_packers)}

    print(f"  hosts={hosts}", flush=True)
    pure_split = name.startswith("C_")  # UNL never packs
    for nid in active_nodes:
        rpc, p2p = port_map[nid]
        peers = peer_list_ids(inv, nid)
        is_packer_node = nid in active_packers
        is_unl_node = nid in unl
        if nid.startswith("arch"):
            role, profile = "order-exec", "archival"
            wi, wc = 0, wcnt
            enable_gmp = False
        elif is_packer_node and not is_unl_node:
            # pure pack worker: DA only — bodies for UNL to certify
            role, profile = "da-worker", "validator"
            wi, wc = widx.get(nid, 0), wcnt
            enable_gmp = True
        elif is_unl_node and is_packer_node:
            # classic / mode B: UNL also packs
            role, profile = "full", "validator"
            wi, wc = widx.get(nid, 0), wcnt
            enable_gmp = True
        elif is_unl_node and not is_packer_node:
            # mode C: UNL cert + exec only (no pack, no GMP lease)
            role, profile = "full", "validator"
            wi, wc = 0, wcnt
            enable_gmp = False
        else:
            role, profile = "full", "validator"
            wi, wc = 0, wcnt
            enable_gmp = False
        if name == "A_classic_n3" and nid in unl:
            role, profile = "full", "validator"
            enable_gmp = True
        cmd = node_cmd_custom(
            nid,
            role,
            profile,
            rpc,
            p2p,
            peers,
            wi,
            wc,
            network_id,
            unl,
            active_packers,
            gmp,
            gcp,
            enable_gmp,
        )
        remote = hosts[nid] == sc.F2
        start_cmd(cmd, nid, remote)
        if pure_split and is_unl_node:
            print(f"    {nid}: UNL cert-only (no pack, no gmp)", flush=True)
        if pure_split and is_packer_node and not is_unl_node:
            print(f"    {nid}: pure pack worker (da-worker + gmp)", flush=True)

    rpc_nodes = [(nid, hosts[nid], port_map[nid][0]) for nid in active_nodes]
    wait_rpc(rpc_nodes)
    time.sleep(2)

    # gmp agents on packers
    for nid in active_packers:
        try:
            st = sc.rpc(hosts[nid], port_map[nid][0], "gmp_stats", {}, timeout=3)
            print(f"  {nid} gmp_enabled={(st.get('result') or {}).get('enabled')}", flush=True)
        except Exception as e:
            print(f"  {nid} gmp_stats err {e}", flush=True)

    stats = {}
    before = snap_unl(unl, hosts, {n: port_map[n][0] for n in unl})
    t0 = time.time()
    print(f"  flood rate={RATE} hold={HOLD}s → GMP", flush=True)
    client_flood(gmp, HOLD, RATE, stats, n_for_seq=3)
    time.sleep(DRAIN)
    after = snap_unl(unl, hosts, {n: port_map[n][0] for n in unl})
    elapsed = time.time() - t0
    ha = health_unl(after, unl)
    deltas = []
    for nid in unl:
        b = before.get(nid, {}).get("exec")
        a = after.get(nid, {}).get("exec")
        if isinstance(b, int) and isinstance(a, int):
            deltas.append(a - b)
    tps_min = (min(deltas) / elapsed) if deltas else 0.0
    tps_max = (max(deltas) / elapsed) if deltas else 0.0

    # sample packer batch creates
    pack_batches = {}
    for nid in active_packers:
        try:
            m = sc.metrics(hosts[nid], port_map[nid][0])
            pack_batches[nid] = m.get("batches_created")
        except Exception as e:
            pack_batches[nid] = str(e)

    try:
        gmp_st = http_json(f"{gmp}/v1/status")
        gcp_st = http_json(f"{gcp}/v1/status")
    except Exception as e:
        gmp_st, gcp_st = {"error": str(e)}, {}

    result = {
        "mode": name,
        "unl": unl,
        "packers": active_packers,
        "elapsed_s": round(elapsed, 2),
        "exec_tps_min": round(tps_min, 2),
        "exec_tps_max": round(tps_max, 2),
        "exec_delta": deltas,
        "health": ha,
        "gmp_ok": stats.get("gmp_ok", 0),
        "gmp_err": stats.get("gmp_err", 0),
        "pack_batches_created": pack_batches,
        "gmp_status": gmp_st,
        "gcp_status": gcp_st,
        "pass_stable": (
            tps_min >= 3.0
            and ha.get("vals_up") == len(unl)
            and (ha.get("lag_max") or 0) <= 10
            and (ha.get("exec_spread") or 0) <= 15
        ),
    }
    print(
        f"  → exec_tps_min={result['exec_tps_min']} spread={ha.get('exec_spread')} "
        f"lag={ha.get('lag_max')} batches={pack_batches} stable={result['pass_stable']}",
        flush=True,
    )
    return result


def main():
    print(f"OUT={OUT} F1={sc.F1} F2={sc.F2}", flush=True)
    for n in ("falcon-dagd", "falcon-gmpd", "falcon-gcpd"):
        src = RS / "target" / "release" / n
        if src.exists():
            subprocess.check_call(["cp", "-f", str(src), str(MESH_BIN / n)])

    procs = []
    for port in (18500, 18501):
        subprocess.call(f"fuser -k {port}/tcp 2>/dev/null || true", shell=True)
    time.sleep(0.3)
    procs.append(
        start_bin(
            "falcon-gmpd",
            ["--listen", "0.0.0.0:18500", "--shards", "16", "--lease-ttl-s", "8"],
            OUT / "gmpd.log",
        )
    )
    procs.append(start_bin("falcon-gcpd", ["--listen", "0.0.0.0:18501"], OUT / "gcpd.log"))
    time.sleep(0.5)
    gmp = f"http://{sc.F1}:18500"
    gcp = f"http://{sc.F1}:18501"

    # Mode A: classic N=3
    a = run_mode(
        "A_classic_n3",
        UNL_IDS,
        UNL_IDS,  # packers = unl only
        gmp,
        gcp,
        9701,
    )
    # Mode B: 3 UNL + 3 extra pack workers (UNL still packs)
    b = run_mode(
        "B_unl3_pack6",
        UNL_IDS,
        ALL_PACKERS,
        gmp,
        gcp,
        9702,
    )
    # Mode C: pure split — only pack1..3 pack; UNL only cert+exec
    c = run_mode(
        "C_pure_pack3_unl3",
        UNL_IDS,
        PACK_IDS,  # packers exclude UNL
        gmp,
        gcp,
        9703,
    )

    def vs_a(label: str, r: dict) -> str:
        if r["exec_tps_min"] > a["exec_tps_min"] * 1.05:
            return f"{label}_HELP: {r['exec_tps_min']} > A {a['exec_tps_min']}"
        if abs(r["exec_tps_min"] - a["exec_tps_min"]) <= a["exec_tps_min"] * 0.1:
            return f"{label}_FLAT: {r['exec_tps_min']} ≈ A {a['exec_tps_min']}"
        return f"{label}_REGRESS: {r['exec_tps_min']} < A {a['exec_tps_min']}"

    verdict_b = vs_a("B", b)
    verdict_c = vs_a("C", c)
    # Primary focus this run: pure split C
    win_c = c["exec_tps_min"] > a["exec_tps_min"] * 1.05
    verdict = f"{verdict_c} | {verdict_b}"

    report = {
        "suite": "pack_workers_unl_split",
        "verdict": verdict,
        "win_c": win_c,
        "A": a,
        "B": b,
        "C": c,
        "hypothesis": {
            "B": "UNL+extra packers vs classic N=3",
            "C": "Pure pack-only workers (UNL never packs) vs classic N=3",
        },
    }
    (OUT / "PACK_WORKERS_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")
    md = [
        "# Pack workers + UNL-only cert dig",
        "",
        f"**Verdict:** {verdict}",
        "",
        "| Mode | UNL role | Packers | exec_tps_min | spread | lag | gmp_ok | stable |",
        "|------|----------|---------|--------------|--------|-----|--------|--------|",
        f"| A classic | pack+cert | 3 (UNL) | **{a['exec_tps_min']}** | {a['health'].get('exec_spread')} | {a['health'].get('lag_max')} | {a['gmp_ok']} | {a['pass_stable']} |",
        f"| B mixed | pack+cert | 6 (UNL+pack) | **{b['exec_tps_min']}** | {b['health'].get('exec_spread')} | {b['health'].get('lag_max')} | {b['gmp_ok']} | {b['pass_stable']} |",
        f"| **C pure** | **cert only** | **3 (pack*)** | **{c['exec_tps_min']}** | {c['health'].get('exec_spread')} | {c['health'].get('lag_max')} | {c['gmp_ok']} | {c['pass_stable']} |",
        "",
        f"A batches: `{a.get('pack_batches_created')}`",
        f"B batches: `{b.get('pack_batches_created')}`",
        f"C batches: `{c.get('pack_batches_created')}`",
        "",
        "Mode C: UNL never packs and does not lease from GMP; only pack1–3 pack; UNL only signs/executes.",
        "",
    ]
    (OUT / "PACK_WORKERS_REPORT.md").write_text("\n".join(md))
    print("\n" + "\n".join(md), flush=True)

    stop_all_extra()
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            p.terminate()
    print(f"wrote {OUT}/PACK_WORKERS_REPORT.json", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
