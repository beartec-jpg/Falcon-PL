#!/usr/bin/env python3
"""
Phase A — Horizontal scale curve (honest multi-host).

For N in {2,3,4,6}:
  - Bring up N packing validators + 2 archivals on host1/host2 (LAN)
  - Same binary, same knobs (v1-best), no special-case nodes
  - Run fixed load ladder with multi-client style: each account owner-routed
    to its owner val (honest lane model; not a single-hub cheat)
  - Record exec_tps_min (slowest UNL val), spread, lag, submit_tps

North-star: does exec_tps_min increase as N grows?

Anti-fudge:
  - Metrics use MIN over UNL, not max
  - Post-settle root/exec agreement reported
  - Failures recorded raw
"""
from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

MESH = Path(os.environ.get("MESH_ROOT", os.path.expanduser("~/falcon-dag-mesh")))
BIN = MESH / "bin" / "falcon-dagd"
CRYPTO = os.environ.get("CRYPTO", "falcon-512")
NETWORK_BASE = int(os.environ.get("NETWORK_BASE", "9220"))
UNL_THRESHOLD = float(os.environ.get("UNL_THRESHOLD", "0.6"))
# Control plane: host2 LAN sshd may be down; Tailscale IP is reliable for ssh.
SSH_F2 = os.environ.get("SSH_F2", "100.64.0.3")


def _detect_lan_ip() -> str:
    """First non-tailscale, non-docker IPv4 on this host."""
    try:
        out = subprocess.check_output(
            ["bash", "-lc", "ip -4 -br addr | awk '/^lo /{next} /tailscale/{next} /docker/{next} /br-/{next} {print; exit}'"],
            text=True,
        ).strip()
        # e.g. "wlp2s0 UP 192.0.2.1/24"
        for tok in out.split():
            if "/" in tok and tok[0].isdigit():
                return tok.split("/")[0]
    except Exception:
        pass
    return "127.0.0.1"


def _detect_remote_lan_ip(ssh_host: str) -> str:
    try:
        out = subprocess.check_output(
            [
                "ssh",
                "-o",
                "ConnectTimeout=8",
                ssh_host,
                "ip -4 -br addr | awk '/^lo /{next} /tailscale/{next} /docker/{next} /br-/{next} {print; exit}'",
            ],
            text=True,
        ).strip()
        for tok in out.split():
            if "/" in tok and tok[0].isdigit():
                return tok.split("/")[0]
    except Exception:
        pass
    return ""


# Prefer explicit env; else auto-detect (DHCP changes LAN IPs).
F1 = os.environ.get("F1_IP") or _detect_lan_ip()
F2 = os.environ.get("F2_IP") or _detect_remote_lan_ip(SSH_F2) or "192.0.2.2"

# v1-best knobs
ADMIT_AHEAD = 2000
MAX_UNCERT = 8
MAX_BATCH = 64
PACK_OPEN = int(os.environ.get("PACK_OPEN_CAP", os.environ.get("PACK_OPEN", "0")))
BATCH_INTERVAL = 0.05
SNAPSHOT = 15
# single-packer=true → only sorted_unl[0] packs (isolates UNL size from multipack contention)
SINGLE_PACKER = os.environ.get("SINGLE_PACKER", "false").lower() in ("1", "true", "yes")

# Ports
VAL_RPC = {i: 18000 + i for i in range(1, 7)}
VAL_P2P = {i: 19000 + i for i in range(1, 7)}
ARCH_RPC = {1: 18011, 2: 18012}
ARCH_P2P = {1: 19011, 2: 19012}

# Placement: which host runs which val for each N
# Balanced across falcons; always arch1@f1, arch2@f2
PLACEMENT = {
    2: {"f1": [1], "f2": [2]},
    3: {"f1": [1, 2], "f2": [3]},
    4: {"f1": [1, 2], "f2": [3, 4]},
    6: {"f1": [1, 2, 3], "f2": [4, 5, 6]},
}

# Fixed ladder (same for every N). Override with LADDER=warm,light,med
_DEFAULT_LADDER = [
    ("warm", 10, 15, 4),
    ("light", 25, 20, 8),
    ("med", 50, 25, 12),
    ("mid", 80, 25, 16),
]
_LADDER_BY_NAME = {x[0]: x for x in _DEFAULT_LADDER}
if os.environ.get("LADDER"):
    LADDER = [_LADDER_BY_NAME[n.strip()] for n in os.environ["LADDER"].split(",") if n.strip() in _LADDER_BY_NAME]
else:
    LADDER = list(_DEFAULT_LADDER)

NS = [int(x) for x in os.environ.get("SCALE_NS", "2,3,4,6").split(",") if x.strip()]


def sh(cmd: str, check: bool = True, timeout: int | None = 120) -> subprocess.CompletedProcess:
    return subprocess.run(
        cmd,
        shell=True,
        text=True,
        capture_output=True,
        check=check,
        timeout=timeout,
    )


def rpc(host: str, port: int, method: str, params=None, timeout: float = 4.0):
    pl = [{}] if params is None else ([params] if not isinstance(params, list) else params)
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": pl}).encode()
    req = urllib.request.Request(
        f"http://{host}:{port}/",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        d = json.load(r)
    if "error" in d and "result" not in d:
        raise RuntimeError(d["error"])
    return d


def metrics(host: str, port: int) -> dict:
    info = ((rpc(host, port, "server_info").get("result") or {}).get("info") or {})
    return info.get("metrics") or {}


def account_shard(account: str) -> int:
    h = 0x811C9DC5
    for b in account.encode():
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h


def host_for_val(n: int, vi: int) -> str:
    place = PLACEMENT[n]
    if vi in place["f1"]:
        return F1
    if vi in place["f2"]:
        return F2
    raise KeyError(vi)


def val_id(i: int) -> str:
    return f"val{i}"


def peer_list(n: int, self_kind: str, self_i: int) -> str:
    """Full mesh peers minus self. self_kind in val|arch."""
    peers = []
    for i in range(1, n + 1):
        if self_kind == "val" and self_i == i:
            continue
        h = host_for_val(n, i)
        peers.append(f"{h}:{VAL_P2P[i]}")
    # arch always both
    for ai, ah in ((1, F1), (2, F2)):
        if self_kind == "arch" and self_i == ai:
            continue
        peers.append(f"{ah}:{ARCH_P2P[ai]}")
    return ",".join(peers)


def stop_all():
    print("  stop_all…", flush=True)
    # local
    sh(f"pkill -x falcon-dagd || true", check=False)
    for p in list(VAL_RPC.values()) + list(VAL_P2P.values()) + list(ARCH_RPC.values()) + list(ARCH_P2P.values()):
        sh(f"fuser -k {p}/tcp 2>/dev/null || true", check=False)
    # remote
    sh(f"ssh {SSH_F2} 'pkill -x falcon-dagd 2>/dev/null || true; "
       f"for p in 18001 18002 18003 18004 18005 18006 18011 18012 "
       f"19001 19002 19003 19004 19005 19006 19011 19012; do fuser -k $p/tcp 2>/dev/null || true; done'",
       check=False)
    time.sleep(1.5)


def wipe_data():
    print("  wipe data…", flush=True)
    local = MESH / "data"
    if local.exists():
        shutil.rmtree(local)
    local.mkdir(parents=True)
    logs = MESH / "logs"
    if logs.exists():
        shutil.rmtree(logs)
    logs.mkdir(parents=True)
    sh(
        f"ssh -o ConnectTimeout=10 {SSH_F2} "
        f"'rm -rf {MESH}/data {MESH}/logs; mkdir -p {MESH}/data {MESH}/logs'",
        check=False,
    )


def ensure_keys(n: int):
    keys = MESH / "keys"
    need = [f"val{i}.json" for i in range(1, n + 1)] + ["arch1.json", "arch2.json", "unl_public.json"]
    if all((keys / k).exists() for k in need):
        # also need val keys up to 6 for reuse
        if all((keys / f"val{i}.json").exists() for i in range(1, 7)):
            return
    print("  generating keys val1-6 + arch…", flush=True)
    keys.mkdir(parents=True, exist_ok=True)
    wallets = MESH / "wallets"
    # gen all 6 for reuse across N
    sh(f"{BIN} --gen-keys --keys-dir {keys} --unl val1,val2,val3,val4,val5,val6")
    tmp = Path("/tmp/arch-keys-scale")
    if tmp.exists():
        shutil.rmtree(tmp)
    tmp.mkdir()
    sh(f"{BIN} --gen-keys --keys-dir {tmp} --unl arch1,arch2")
    for name in ("arch1.json", "arch2.json"):
        shutil.copy2(tmp / name, keys / name)
    # merge public
    pub = json.loads((keys / "unl_public.json").read_text())
    apub = json.loads((tmp / "unl_public.json").read_text())
    pub.update(apub)
    (keys / "unl_public.json").write_text(json.dumps(pub, indent=2) + "\n")
    if not wallets.exists() or not list(wallets.glob("acct*.json")):
        sh(f"{BIN} --gen-wallet-keys --wallet-dir {wallets} --faucet-accounts 40")
    # sync to f2
    sh(f"ssh {SSH_F2} 'mkdir -p {MESH}/{{bin,keys,wallets,data,logs}}'")
    sh(f"rsync -az {BIN} {SSH_F2}:{MESH}/bin/")
    sh(f"rsync -az --delete {keys}/ {SSH_F2}:{MESH}/keys/")
    sh(f"rsync -az --delete {wallets}/ {SSH_F2}:{MESH}/wallets/")


def node_cmd(
    nid: str,
    role: str,
    profile: str,
    rpc_port: int,
    p2p_port: int,
    peers: str,
    widx: int,
    n: int,
    network_id: int,
) -> list[str]:
    force = []
    if profile != "validator":
        force = ["--force-node-profile"]
    cmd = [
        str(BIN),
        "--id", nid,
        "--role", role,
        "--node-profile", profile,
        *force,
        "--rpc", f"0.0.0.0:{rpc_port}",
        "--p2p", f"0.0.0.0:{p2p_port}",
        "--peers", peers,
        "--unl", ",".join(val_id(i) for i in range(1, n + 1)),
        "--worker-index", str(widx),
        "--worker-count", str(n),
        "--network-id", str(network_id),
        "--crypto", CRYPTO,
        "--keys-dir", str(MESH / "keys"),
        "--wallet-dir", str(MESH / "wallets"),
        "--require-tx-sigs", "--auto-sign",
        f"--single-packer={'true' if SINGLE_PACKER else 'false'}",
        "--adaptive-batch=true",
        "--batch-interval", str(BATCH_INTERVAL),
        "--max-batch-txs", str(MAX_BATCH),
        "--max-batch-parents", "3",
        "--unl-threshold", str(UNL_THRESHOLD),
        "--max-uncertified-per-creator", str(MAX_UNCERT),
        f"--pack-open-cap={PACK_OPEN}",
        "--uncertified-ttl", "0",
        "--open-body-repool-after", "8",
        "--admit-ahead-cap", str(ADMIT_AHEAD),
        "--admit-busy-uncert-age", "0",
        "--conf-pool", "true",
        "--pack-check-slot", "0.25",
        "--snapshot-interval", str(SNAPSHOT),
        "--data-dir", str(MESH / "data" / nid),
    ]
    # PR-G4: optional in-engine GMP lease agent (set GMP_URL=http://F1:18500).
    gmp_url = os.environ.get("GMP_URL", "").strip()
    if gmp_url and profile == "validator":
        # Per-val budget skew for capacity digs: GMP_BUDGET_val1=8192 etc.
        budget = os.environ.get(f"GMP_BUDGET_{nid}") or os.environ.get("GMP_BUDGET", "4096")
        max_if = os.environ.get(f"GMP_MAX_IF_{nid}") or os.environ.get("GMP_MAX_IF", "32")
        cmd += [
            "--gmp-url", gmp_url,
            "--gmp-lease-max", os.environ.get("GMP_LEASE_MAX", "4"),
            "--gmp-budget-mb", str(budget),
            "--gmp-max-in-flight", str(max_if),
            f"--gmp-affinity={'true' if os.environ.get('GMP_AFFINITY', '1') not in ('0', 'false', 'False') else 'false'}",
        ]
        gcp_url = os.environ.get("GCP_URL", "").strip()
        if gcp_url:
            cmd += ["--gcp-url", gcp_url]
    return cmd


def start_local(cmd: list[str], nid: str) -> int:
    log = MESH / "logs" / f"{nid}.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    (MESH / "data" / nid).mkdir(parents=True, exist_ok=True)
    with open(log, "w") as lf:
        proc = subprocess.Popen(
            cmd,
            stdout=lf,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    (MESH / "logs" / f"{nid}.pid").write_text(str(proc.pid))
    print(f"    local {nid} pid={proc.pid}", flush=True)
    return proc.pid


def start_remote(cmd: list[str], nid: str):
    # rebuild as shell-safe remote command using remote paths
    # cmd uses local MESH paths which are same on f2
    remote_cmd = " ".join(shlex_quote(c) for c in cmd)
    script = f"""
set -e
mkdir -p {MESH}/data/{nid} {MESH}/logs
nohup {remote_cmd} > {MESH}/logs/{nid}.log 2>&1 &
echo $! > {MESH}/logs/{nid}.pid
echo started {nid} pid=$(cat {MESH}/logs/{nid}.pid)
"""
    r = sh(f"ssh {SSH_F2} bash -s <<'EOF'\n{script}\nEOF")
    print(f"    remote {nid}: {r.stdout.strip()}", flush=True)


def shlex_quote(s: str) -> str:
    return "'" + s.replace("'", "'\"'\"'") + "'"


def start_mesh(n: int, network_id: int):
    print(f"  start mesh N={n} network={network_id}", flush=True)
    print(f"    peers via LAN F1={F1} F2={F2} (ssh control {SSH_F2})", flush=True)
    place = PLACEMENT[n]
    unl = [val_id(i) for i in range(1, n + 1)]
    print(f"    UNL={unl} f1={place['f1']} f2={place['f2']}", flush=True)

    # start vals
    for i in range(1, n + 1):
        nid = val_id(i)
        peers = peer_list(n, "val", i)
        cmd = node_cmd(nid, "full", "validator", VAL_RPC[i], VAL_P2P[i], peers, i - 1, n, network_id)
        if i in place["f1"]:
            start_local(cmd, nid)
        else:
            start_remote(cmd, nid)

    # archivals
    for ai, host_local in ((1, True), (2, False)):
        nid = f"arch{ai}"
        peers = peer_list(n, "arch", ai)
        # archival worker-index 0 (not packing)
        cmd = node_cmd(nid, "order-exec", "archival", ARCH_RPC[ai], ARCH_P2P[ai], peers, 0, n, network_id)
        if host_local:
            start_local(cmd, nid)
        else:
            start_remote(cmd, nid)

    # wait ready (RPC over same IPs used for peers)
    print("  wait RPC…", flush=True)
    deadline = time.time() + 45
    last_err = ""
    while time.time() < deadline:
        ok = 0
        errs = []
        for i in range(1, n + 1):
            h = host_for_val(n, i)
            try:
                metrics(h, VAL_RPC[i])
                ok += 1
            except Exception as e:
                errs.append(f"{val_id(i)}@{h}:{VAL_RPC[i]} {e}")
        for ai, h in ((1, F1), (2, F2)):
            try:
                metrics(h, ARCH_RPC[ai])
                ok += 1
            except Exception as e:
                errs.append(f"arch{ai}@{h}:{ARCH_RPC[ai]} {e}")
        if ok == n + 2:
            print(f"  all {ok} nodes up", flush=True)
            return
        last_err = "; ".join(errs[:4])
        time.sleep(0.5)
    raise RuntimeError(f"mesh not ready N={n} ok partial; sample: {last_err}")


def snap_vals(n: int) -> dict:
    out = {}
    for i in range(1, n + 1):
        nid = val_id(i)
        h = host_for_val(n, i)
        try:
            m = metrics(h, VAL_RPC[i])
            ps = m.get("pack_stall") or {}
            out[nid] = {
                "exec": m.get("executed_txs"),
                "lag": ps.get("admit_exec_lag", m.get("admit_exec_lag")),
                "certified": m.get("certified"),
                "batches": m.get("batches_created"),
                "mempool": m.get("mempool"),
                "uncertified": m.get("uncertified"),
                "p2p": m.get("p2p_peers"),
                "root": (m.get("state_root") or "")[:16],
                "admits": ps.get("local_admits_ok"),
            }
        except Exception as e:
            out[nid] = {"error": str(e)}
    for ai, h in ((1, F1), (2, F2)):
        nid = f"arch{ai}"
        try:
            m = metrics(h, ARCH_RPC[ai])
            ps = m.get("pack_stall") or {}
            out[nid] = {
                "exec": m.get("executed_txs"),
                "lag": ps.get("admit_exec_lag"),
                "certified": m.get("certified"),
                "batches": m.get("batches_created"),
                "profile": m.get("node_profile"),
                "root": (m.get("state_root") or "")[:16],
            }
        except Exception as e:
            out[nid] = {"error": str(e)}
    return out


def health(n: int, snap: dict) -> dict:
    execs = []
    lags = []
    for i in range(1, n + 1):
        s = snap.get(val_id(i), {})
        if isinstance(s.get("exec"), int):
            execs.append(s["exec"])
        if "error" not in s:
            lags.append(s.get("lag") or 0)
    roots = {snap.get(val_id(i), {}).get("root") for i in range(1, n + 1) if snap.get(val_id(i), {}).get("root")}
    return {
        "exec_min": min(execs) if execs else None,
        "exec_max": max(execs) if execs else None,
        "exec_spread": (max(execs) - min(execs)) if execs else None,
        "lag_max": max(lags) if lags else None,
        "vals_up": sum(1 for i in range(1, n + 1) if "error" not in snap.get(val_id(i), {})),
        "roots_agree": len(roots) <= 1,
        "arch1_exec": snap.get("arch1", {}).get("exec"),
        "arch2_exec": snap.get("arch2", {}).get("exec"),
    }


class SeqBank:
    def __init__(self, n: int):
        self.n = n
        self._lock = threading.Lock()
        self._seq = {f"acct{a}": 1 for a in range(1, 41)}
        for a in range(1, 41):
            self.resync(f"acct{a}")

    def owner_idx(self, account: str) -> int:
        return account_shard(account) % self.n

    def resync(self, acct: str):
        for i in range(1, self.n + 1):
            try:
                r = (rpc(host_for_val(self.n, i), VAL_RPC[i], "account_info", {"account": acct}, timeout=2).get("result") or {})
                if "sequence" in r:
                    with self._lock:
                        self._seq[acct] = int(r["sequence"])
                    return
            except Exception:
                continue

    def reserve_lowest(self, accts: list[str]) -> tuple[str, int]:
        with self._lock:
            acct = min(accts, key=lambda a: self._seq[a])
            s = self._seq[acct]
            self._seq[acct] = s + 1
            return acct, s

    def rollback(self, acct: str, used: int):
        with self._lock:
            if self._seq.get(acct) == used + 1:
                self._seq[acct] = used


def run_ladder_phase(n: int, label: str, rate: int, hold: float, workers: int, bank: SeqBank) -> dict:
    accts = [f"acct{i}" for i in range(1, 41)]
    counts = {"ok": 0, "busy": 0, "err": 0}
    codes: dict[str, int] = {}
    by_node = {val_id(i): 0 for i in range(1, n + 1)}
    lock = threading.Lock()
    stop_at = time.time() + hold

    def one():
        acct, s = bank.reserve_lowest(accts)
        oi = bank.owner_idx(acct)
        vi = oi + 1
        h = host_for_val(n, vi)
        dest = "acct40" if acct != "acct40" else "acct39"
        try:
            d = rpc(
                h,
                VAL_RPC[vi],
                "submit",
                {"account": acct, "sequence": s, "destination": dest, "amount": 1, "fee": 10},
                timeout=3.0,
            )
            code = (d.get("result") or {}).get("engine_result") or "null"
            with lock:
                if code == "tesSUCCESS":
                    counts["ok"] += 1
                    by_node[val_id(vi)] += 1
                elif code == "terBUSY":
                    counts["busy"] += 1
                    bank.rollback(acct, s)
                elif code in ("tecSEQ_PAST", "tefPAST_SEQ", "tecSEQ_FUTURE", "tefALREADY"):
                    counts["err"] += 1
                    codes[code] = codes.get(code, 0) + 1
                    bank.resync(acct)
                else:
                    counts["err"] += 1
                    codes[str(code)] = codes.get(str(code), 0) + 1
                    bank.rollback(acct, s)
        except Exception as e:
            bank.rollback(acct, s)
            with lock:
                counts["err"] += 1
                codes[type(e).__name__] = codes.get(type(e).__name__, 0) + 1

    before = snap_vals(n)
    t0 = time.time()
    with ThreadPoolExecutor(max_workers=workers) as ex:
        while time.time() < stop_at:
            sec0 = time.time()
            futs = [ex.submit(one) for _ in range(rate)]
            for f in as_completed(futs):
                _ = f.result()
            left = 1.0 - (time.time() - sec0)
            if left > 0:
                time.sleep(left)
    time.sleep(2.0)
    after = snap_vals(n)
    elapsed = time.time() - t0

    exec_d = {}
    for i in range(1, n + 1):
        nid = val_id(i)
        b, a = before.get(nid, {}), after.get(nid, {})
        if isinstance(b.get("exec"), int) and isinstance(a.get("exec"), int):
            exec_d[nid] = a["exec"] - b["exec"]
        else:
            exec_d[nid] = None
    deltas = [v for v in exec_d.values() if isinstance(v, int)]
    min_d = min(deltas) if deltas else 0
    max_d = max(deltas) if deltas else 0
    hb, ha = health(n, before), health(n, after)

    phase = {
        "label": label,
        "n": n,
        "target_rate": rate,
        "hold_s": hold,
        "workers": workers,
        "elapsed_s": round(elapsed, 2),
        "ok": counts["ok"],
        "busy": counts["busy"],
        "err": counts["err"],
        "err_codes": codes,
        "submits_by_owner": by_node,
        "submit_tps": round(counts["ok"] / elapsed, 2) if elapsed else 0,
        "exec_delta": exec_d,
        "exec_delta_min": min_d,
        "exec_delta_max": max_d,
        "exec_tps_min": round(min_d / elapsed, 2) if elapsed else 0,
        "exec_tps_max": round(max_d / elapsed, 2) if elapsed else 0,
        "health_before": hb,
        "health_after": ha,
        "lag_after": ha.get("lag_max"),
        "spread_after": ha.get("exec_spread"),
        "snap_after": after,
    }
    phase["pass_tight"] = (
        counts["err"] == 0
        and (ha.get("lag_max") or 0) == 0
        and ha.get("vals_up") == n
        and (ha.get("exec_spread") or 0) <= max(20, int(0.05 * max(1, ha.get("exec_min") or 1)))
        and min_d >= max(1, int(counts["ok"] * 0.35))
    )
    return phase


def run_n(n: int, out_dir: Path) -> dict:
    network_id = NETWORK_BASE + n
    print(f"\n========== N={n} network_id={network_id} ==========", flush=True)
    stop_all()
    wipe_data()
    ensure_keys(n)
    # re-sync keys each time (cheap)
    sh(f"rsync -az --delete {MESH}/keys/ {SSH_F2}:{MESH}/keys/", check=False)
    sh(f"rsync -az --delete {MESH}/wallets/ {SSH_F2}:{MESH}/wallets/", check=False)
    sh(f"rsync -az {BIN} {SSH_F2}:{MESH}/bin/", check=False)
    start_mesh(n, network_id)
    time.sleep(2)

    bank = SeqBank(n)
    phases = []
    for label, rate, hold, workers in LADDER:
        print(f"  --- N={n} {label} target={rate}/s hold={hold}s ---", flush=True)
        ph = run_ladder_phase(n, label, rate, hold, workers, bank)
        phases.append(ph)
        print(
            f"      ok={ph['ok']} busy={ph['busy']} err={ph['err']} "
            f"submit_tps={ph['submit_tps']} exec_tps_min={ph['exec_tps_min']} "
            f"spread={ph['spread_after']} lag={ph['lag_after']} tight={ph['pass_tight']}",
            flush=True,
        )
        (out_dir / f"n{n}_{label}.json").write_text(json.dumps(ph, indent=2) + "\n")
        time.sleep(3)

    time.sleep(8)
    final = snap_vals(n)
    fh = health(n, final)
    # best tight + best exec
    tight = [p for p in phases if p["pass_tight"]]
    best_exec = max(phases, key=lambda p: p["exec_tps_min"])
    result = {
        "n": n,
        "network_id": network_id,
        "placement": PLACEMENT[n],
        "phases": phases,
        "final_health": fh,
        "final_snap": final,
        "best_exec_tps_min": best_exec["exec_tps_min"],
        "best_exec_phase": best_exec["label"],
        "best_tight_tps_min": max((p["exec_tps_min"] for p in tight), default=None),
        "best_tight_phase": max(tight, key=lambda p: p["exec_tps_min"])["label"] if tight else None,
        "any_tight_pass": bool(tight),
    }
    (out_dir / f"n{n}_summary.json").write_text(json.dumps(result, indent=2) + "\n")
    stop_all()
    return result


def analyze(results: list[dict]) -> dict:
    """Did throughput grow with N?"""
    by_n = {r["n"]: r for r in results}
    ns = sorted(by_n)
    series_tight = [(n, by_n[n]["best_tight_tps_min"]) for n in ns]
    series_best = [(n, by_n[n]["best_exec_tps_min"]) for n in ns]

    def grows(series):
        # compare consecutive defined points
        vals = [(n, v) for n, v in series if v is not None]
        if len(vals) < 2:
            return False, "insufficient"
        # overall: last vs first
        overall = vals[-1][1] > vals[0][1] * 1.05
        # monotonic non-decreasing mostly
        ups = sum(1 for i in range(1, len(vals)) if vals[i][1] >= vals[i - 1][1] * 0.95)
        return overall, {"overall_up": overall, "non_decreasing_steps": ups, "steps": len(vals) - 1, "series": vals}

    grow_tight, detail_tight = grows(series_tight)
    grow_best, detail_best = grows(series_best)

    return {
        "series_best_exec_tps_min": series_best,
        "series_tight_exec_tps_min": series_tight,
        "scale_out_pass_tight": grow_tight and detail_tight != "insufficient",
        "scale_out_detail_tight": detail_tight,
        "scale_out_pass_best_effort": grow_best and detail_best != "insufficient",
        "scale_out_detail_best": detail_best,
        "verdict": (
            "PASS: tight exec_tps_min grows with N"
            if grow_tight and detail_tight != "insufficient"
            else "FAIL: tight multi-host throughput does NOT grow with N (or insufficient tight passes)"
        ),
    }


def main():
    out = Path(os.environ.get("OUT", f"/tmp/scale-n-curve-{int(time.time())}"))
    out.mkdir(parents=True, exist_ok=True)
    print(f"OUT={out}", flush=True)
    print(
        f"NS={NS} F1={F1} F2={F2} single_packer={SINGLE_PACKER} "
        f"knobs admit={ADMIT_AHEAD} uncert={MAX_UNCERT} batch={MAX_BATCH} ladder={[x[0] for x in LADDER]}",
        flush=True,
    )

    if not BIN.exists():
        print(f"missing binary {BIN}", file=sys.stderr)
        return 1

    results = []
    for n in NS:
        if n not in PLACEMENT:
            print(f"skip unsupported N={n}", flush=True)
            continue
        try:
            results.append(run_n(n, out))
        except Exception as e:
            print(f"N={n} FAILED: {e}", flush=True)
            results.append({"n": n, "error": str(e)})
            stop_all()

    analysis = analyze([r for r in results if "error" not in r])
    report = {
        "suite": "phase_a_scale_n_curve",
        "goal": "exec_tps_min increases as packing N grows (honest multi-host)",
        "transport": "LAN",
        "single_packer": SINGLE_PACKER,
        "knobs": {
            "admit_ahead": ADMIT_AHEAD,
            "max_uncert": MAX_UNCERT,
            "max_batch_txs": MAX_BATCH,
            "pack_open": PACK_OPEN,
            "crypto": CRYPTO,
            "unl_threshold": UNL_THRESHOLD,
            "single_packer": SINGLE_PACKER,
        },
        "results": results,
        "analysis": analysis,
    }
    (out / "SCALE_N_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")

    # markdown
    lines = [
        "# Phase A — Throughput vs N (horizontal scale)",
        "",
        f"OUT: `{out}`",
        f"Transport: LAN `{F1}` / `{F2}` · crypto={CRYPTO} · knobs v1-best",
        "",
        f"**Verdict:** {analysis['verdict']}",
        "",
        "## Curve (best exec_tps_min per N)",
        "",
        "| N | placement f1/f2 | best exec_tps_min | best phase | tight exec_tps_min | tight phase | final spread |",
        "|---|-----------------|-------------------|------------|--------------------|-------------|--------------|",
    ]
    for r in results:
        if "error" in r:
            lines.append(f"| {r['n']} | — | ERROR | {r['error'][:40]} | — | — | — |")
            continue
        pl = r["placement"]
        lines.append(
            f"| {r['n']} | {pl['f1']}/{pl['f2']} | **{r['best_exec_tps_min']}** | {r['best_exec_phase']} | "
            f"{r['best_tight_tps_min']} | {r['best_tight_phase']} | {r['final_health'].get('exec_spread')} |"
        )

    lines += [
        "",
        "## Per-N ladder (exec_tps_min)",
        "",
        "| N | warm(10) | light(25) | med(50) | mid(80) |",
        "|---|----------|-----------|---------|---------|",
    ]
    for r in results:
        if "error" in r:
            continue
        by = {p["label"]: p for p in r["phases"]}
        def e(lab):
            p = by.get(lab)
            return p["exec_tps_min"] if p else "—"
        lines.append(f"| {r['n']} | {e('warm')} | {e('light')} | {e('med')} | {e('mid')} |")

    lines += [
        "",
        "## Scale-out analysis",
        "```json",
        json.dumps(analysis, indent=2),
        "```",
        "",
        "## Anti-fudge notes",
        "- Same binary/knobs for all N",
        "- `exec_tps_min` = slowest UNL validator delta (not fastest)",
        "- Owner-route by FNV lane (real pack ownership model)",
        "- 2 archivals always present (order-exec, no pack)",
        "- LAN multi-host (not localhost-only)",
        "",
    ]
    (out / "SCALE_N_REPORT.md").write_text("\n".join(lines) + "\n")
    # also copy to deploy
    deploy = Path(__file__).resolve().parent / "SCALE_N_REPORT.md"
    try:
        deploy.write_text("\n".join(lines) + "\n")
    except Exception:
        pass

    print("\n" + "\n".join(lines), flush=True)
    print(f"\nWrote {out}/SCALE_N_REPORT.json", flush=True)
    return 0 if analysis.get("scale_out_pass_tight") else 2


if __name__ == "__main__":
    sys.exit(main())
