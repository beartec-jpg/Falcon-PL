#!/usr/bin/env python3
"""
Phase A scale dig with GMP front door (PR-G1..G5 path).

For each N in SCALE_NS (default 3,6):
  - falcon-gmpd + falcon-gcpd
  - N multipack vals + 2 archivals (real-mesh placement)
  - falcon-dagd --gmp-url --gcp-url (in-engine leases)
  - Clients submit **only to GMP** (no owner-route)
  - Equal capacity budgets (honest scale-out, not skew dig)
  - Ladder: warm / light / med
  - Metric: exec_tps_min (slowest UNL val)

North-star: does exec_tps_min(N=6) > exec_tps_min(N=3)?

Reports: SCALE_N_GMP_REPORT.{json,md}
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
OUT = Path(os.environ.get("OUT", f"/tmp/gmp-scale-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

NS = [int(x) for x in os.environ.get("SCALE_NS", "3,6").split(",") if x.strip()]
MAX_INFLIGHT = int(os.environ.get("GMP_MAX_INFLIGHT", "3"))
# ladder: label, rate/s, hold_s, workers
_DEFAULT = [
    ("warm", 12, 12, 4),
    ("light", 25, 20, 8),
    ("med", 50, 25, 12),
]
_BY = {x[0]: x for x in _DEFAULT}
if os.environ.get("LADDER"):
    LADDER = [_BY[n.strip()] for n in os.environ["LADDER"].split(",") if n.strip() in _BY]
else:
    LADDER = list(_DEFAULT)

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


def ensure_bins():
    for n in ("falcon-dagd", "falcon-gmpd", "falcon-gcpd"):
        src = RS / "target" / "release" / n
        if src.exists():
            subprocess.check_call(["cp", "-f", str(src), str(MESH_BIN / n)])
        if not (MESH_BIN / n).exists():
            raise SystemExit(f"missing bin {n}")


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
                        inflight[a] = max(0, inflight[a] - 1)

    threading.Thread(target=decay, daemon=True).start()
    workers = min(12, max(4, rate // 3))
    with ThreadPoolExecutor(max_workers=workers) as ex:
        while time.time() - t0 < hold:
            sec0 = time.time()
            futs = [ex.submit(one) for _ in range(rate)]
            for f in as_completed(futs):
                _ = f.result()
            left = 1.0 - (time.time() - sec0)
            if left > 0:
                time.sleep(left)


def run_n(n: int, gmp_url: str, gcp_url: str, network_id: int) -> dict:
    print(f"\n===== N={n} multipack + GMP =====", flush=True)
    # Equal budgets — scale dig not capacity-skew dig
    os.environ["GMP_URL"] = gmp_url
    os.environ["GCP_URL"] = gcp_url
    os.environ["GMP_BUDGET"] = "4096"
    for i in range(1, n + 1):
        os.environ.pop(f"GMP_BUDGET_val{i}", None)
        os.environ.pop(f"GMP_MAX_IF_val{i}", None)
    os.environ["GMP_MAX_IF"] = "32"
    os.environ["GMP_LEASE_MAX"] = "4"
    os.environ["GMP_AFFINITY"] = "1"

    sc.SINGLE_PACKER = False
    sc.PACK_OPEN = 0
    sc.stop_all()
    sc.wipe_data()
    sc.ensure_keys(n)
    sc.sh(f"rsync -az {sc.BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)
    sc.start_mesh(n, network_id)
    time.sleep(3)

    # confirm gmp agents
    for i in range(1, n + 1):
        try:
            st = sc.rpc(sc.host_for_val(n, i), sc.VAL_RPC[i], "gmp_stats", {}, timeout=3)
            en = (st.get("result") or {}).get("enabled")
            print(f"  val{i} gmp_enabled={en}", flush=True)
            if not en:
                return {"n": n, "error": f"val{i} gmp not enabled", "phases": []}
        except Exception as e:
            return {"n": n, "error": f"val{i} gmp_stats: {e}", "phases": []}

    phases = []
    for label, rate, hold, _workers in LADDER:
        stats: dict = {}
        before = sc.snap_vals(n)
        t0 = time.time()
        print(f"  phase {label}: rate={rate}/s hold={hold}s → GMP", flush=True)
        client_flood(gmp_url, hold, rate, stats, n)
        time.sleep(12)  # settle
        after = sc.snap_vals(n)
        elapsed = time.time() - t0
        ha = sc.health(n, after)
        deltas = []
        for i in range(1, n + 1):
            b = before.get(f"val{i}", {}).get("exec")
            a = after.get(f"val{i}", {}).get("exec")
            if isinstance(b, int) and isinstance(a, int):
                deltas.append(a - b)
        exec_tps_min = (min(deltas) / elapsed) if deltas else 0.0
        exec_tps_max = (max(deltas) / elapsed) if deltas else 0.0
        try:
            gmp_st = http_json(f"{gmp_url}/v1/status")
            gcp_st = http_json(f"{gcp_url}/v1/status")
        except Exception as e:
            gmp_st, gcp_st = {"error": str(e)}, {}

        # sample pack_stall on val1
        pack_hint = {}
        try:
            m = sc.metrics(sc.host_for_val(n, 1), sc.VAL_RPC[1])
            ps = m.get("pack_stall") or {}
            pack_hint = {
                "own_open_n": ps.get("own_open_n"),
                "try_certify_attempts": ps.get("try_certify_attempts"),
                "try_certify_ok": ps.get("try_certify_ok"),
                "filter_seq_gap": (ps.get("pack_skip_reasons") or {}).get("filter_seq_gap"),
                "no_ready_txs": (ps.get("pack_skip_reasons") or {}).get("no_ready_txs"),
                "mempool": ps.get("mempool"),
            }
        except Exception:
            pass

        phase = {
            "label": label,
            "rate": rate,
            "hold_s": hold,
            "elapsed_s": round(elapsed, 2),
            "exec_tps_min": round(exec_tps_min, 2),
            "exec_tps_max": round(exec_tps_max, 2),
            "exec_delta": deltas,
            "health": ha,
            "gmp_ok": stats.get("gmp_ok", 0),
            "gmp_err": stats.get("gmp_err", 0),
            "gmp_status": gmp_st,
            "gcp_status": gcp_st,
            "pack_hint_val1": pack_hint,
            "pass_tight": (
                exec_tps_min >= 3.0
                and ha.get("vals_up") == n
                and (ha.get("lag_max") or 0) <= 5
                and (ha.get("exec_spread") or 0) <= 10
            ),
        }
        phases.append(phase)
        print(
            f"    → exec_tps_min={phase['exec_tps_min']} spread={ha.get('exec_spread')} "
            f"lag={ha.get('lag_max')} gmp_ok={phase['gmp_ok']} tight={phase['pass_tight']}",
            flush=True,
        )

    # pick best non-warm phase for summary
    candidates = [p for p in phases if p["label"] != "warm"] or phases
    best = max(candidates, key=lambda p: p["exec_tps_min"])
    return {
        "n": n,
        "placement": sc.PLACEMENT[n],
        "phases": phases,
        "best_exec_tps_min": best["exec_tps_min"],
        "best_label": best["label"],
        "med": next((p for p in phases if p["label"] == "med"), None),
        "light": next((p for p in phases if p["label"] == "light"), None),
    }


def main():
    print(f"OUT={OUT} F1={sc.F1} F2={sc.F2} NS={NS}", flush=True)
    ensure_bins()
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
    gmp_url = f"http://{sc.F1}:18500"
    gcp_url = f"http://{sc.F1}:18501"
    print("GMP", http_json(f"{gmp_url}/v1/status"), flush=True)

    results = []
    for n in NS:
        # unique network id per N
        nid = 9600 + n
        try:
            results.append(run_n(n, gmp_url, gcp_url, nid))
        except Exception as e:
            results.append({"n": n, "error": str(e), "phases": []})
            print(f"  N={n} FAILED: {e}", flush=True)

    # North-star compare
    by_n = {r["n"]: r for r in results if "best_exec_tps_min" in r}
    n3 = by_n.get(3)
    n6 = by_n.get(6)
    scale_out_ok = False
    verdict = "INCONCLUSIVE"
    if n3 and n6:
        t3 = n3["best_exec_tps_min"]
        t6 = n6["best_exec_tps_min"]
        med3 = (n3.get("med") or {}).get("exec_tps_min")
        med6 = (n6.get("med") or {}).get("exec_tps_min")
        # Scale-out success: N=6 best strictly beats N=3 best
        scale_out_ok = t6 > t3 * 1.05  # 5% margin
        if scale_out_ok:
            verdict = f"SCALE_OUT_PASS: N=6 best {t6} > N=3 best {t3}"
        elif t6 >= t3 * 0.9:
            verdict = f"SCALE_OUT_FLAT: N=6 best {t6} ≈ N=3 best {t3} (no gain)"
        else:
            verdict = f"SCALE_OUT_FAIL: N=6 best {t6} < N=3 best {t3} (regression)"
        if med3 is not None and med6 is not None:
            verdict += f" | med: N3={med3} N6={med6}"

    report = {
        "suite": "gmp_scale_n",
        "design": "docs/GLOBAL_MEMPOOL_CONF_POOL_DESIGN.md",
        "hosts": {"F1": sc.F1, "F2": sc.F2},
        "ns": NS,
        "ladder": [{"label": a, "rate": b, "hold": c} for a, b, c, _ in LADDER],
        "path": "clients→GMP → engine lease → multipack pack/cert",
        "capacity": "equal budgets (honest scale dig)",
        "results": results,
        "scale_out_ok": scale_out_ok,
        "verdict": verdict,
        "north_star": "exec_tps_min(N=6) > exec_tps_min(N=3)",
    }
    (OUT / "SCALE_N_GMP_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")

    md = [
        "# GMP scale-N dig (Phase A with global mempool)",
        "",
        f"**Verdict:** {verdict}",
        f"**scale_out_ok:** {scale_out_ok}",
        "",
        "Path: clients → GMP only → in-engine leases → multipack UNL cert/exec.",
        "Equal capacity budgets (not a skew dig).",
        "",
        "| N | best exec_tps_min | best phase | med tps | light tps | lag@med | spread@med |",
        "|---|------------------|------------|---------|-----------|---------|------------|",
    ]
    for r in results:
        if r.get("error"):
            md.append(f"| {r['n']} | ERROR | {r['error'][:40]} | | | | |")
            continue
        med = r.get("med") or {}
        light = r.get("light") or {}
        md.append(
            f"| **{r['n']}** | **{r['best_exec_tps_min']}** | {r['best_label']} | "
            f"{med.get('exec_tps_min', '—')} | {light.get('exec_tps_min', '—')} | "
            f"{(med.get('health') or {}).get('lag_max', '—')} | "
            f"{(med.get('health') or {}).get('exec_spread', '—')} |"
        )
    md += [
        "",
        "## Notes",
        "",
        "- Metric is **exec_tps_min** (slowest UNL validator), not max.",
        "- Prior non-GMP digs: N≈3 multipack works; N=6 often collapses (cert/UNL/pack).",
        "- This dig asks whether **GMP front door alone** unlocks N=6 scale-out.",
        "",
        f"Raw: `{OUT}/SCALE_N_GMP_REPORT.json`",
        "",
    ]
    (OUT / "SCALE_N_GMP_REPORT.md").write_text("\n".join(md))
    print("\n" + "\n".join(md), flush=True)
    print(json.dumps({"verdict": verdict, "scale_out_ok": scale_out_ok}, indent=2), flush=True)

    sc.stop_all()
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            p.terminate()
    print(f"wrote {OUT}/SCALE_N_GMP_REPORT.json", flush=True)
    # exit 0 always — dig reports truth; scale_out_ok may be false
    return 0


if __name__ == "__main__":
    sys.exit(main())
