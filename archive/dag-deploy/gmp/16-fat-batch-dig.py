#!/usr/bin/env python3
"""
Fat-batch amortization dig (N=3 multipack + GMP).

Hypothesis: if UNL agreement is the bottleneck, covering more txs per cert
(larger max-batch-txs, slightly slower pack cadence) raises exec_tps_min
without increasing certs/s.

Sweep max_batch_txs × batch_interval; report:
  exec_tps_min, batches_created delta, implied txs/batch, certs proxy.

Anti-fudge: exec_tps_min over UNL; same client load each cell.
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
OUT = Path(os.environ.get("OUT", f"/tmp/fat-batch-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

HOLD = float(os.environ.get("HOLD", "35"))
RATE = int(os.environ.get("RATE", "80"))  # high enough to fill fat batches
DRAIN = float(os.environ.get("DRAIN", "18"))
MAX_INFLIGHT = int(os.environ.get("GMP_MAX_INFLIGHT", "4"))
N = 3

# (max_batch_txs, batch_interval_s, adaptive)
# adaptive off so interval is respected for accumulation
SWEEP = [
    (16, 0.05, False),
    (64, 0.05, False),   # baseline-ish
    (128, 0.05, False),
    (256, 0.05, False),
    (256, 0.12, False),  # fat + wait to accumulate
    (512, 0.12, False),
    (512, 0.20, False),
]
if os.environ.get("FAT_SWEEP"):
    # e.g. FAT_SWEEP=64@0.05,256@0.12
    SWEEP = []
    for part in os.environ["FAT_SWEEP"].split(","):
        part = part.strip()
        if not part:
            continue
        mb, iv = part.split("@")
        SWEEP.append((int(mb), float(iv), False))

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


def client_flood(gmp, hold, rate, stats):
    t0 = time.time()
    bank = sc.SeqBank(N)
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
            time.sleep(0.35)
            with lock:
                for a in inflight:
                    if inflight[a] > 0:
                        inflight[a] -= 1

    threading.Thread(target=decay, daemon=True).start()
    with ThreadPoolExecutor(max_workers=12) as ex:
        while time.time() - t0 < hold:
            sec0 = time.time()
            futs = [ex.submit(one) for _ in range(rate)]
            for f in as_completed(futs):
                _ = f.result()
            left = 1.0 - (time.time() - sec0)
            if left > 0:
                time.sleep(left)


def patch_node_cmd(max_batch: int, interval: float, adaptive: bool):
    """Monkey-patch scale module knobs used by node_cmd."""
    sc.MAX_BATCH = max_batch
    sc.BATCH_INTERVAL = interval
    # node_cmd hardcodes adaptive-batch=true — wrap node_cmd
    orig = sc.node_cmd

    def node_cmd_wrapped(*args, **kwargs):
        cmd = orig(*args, **kwargs)
        # replace adaptive and batch flags if present
        out = []
        skip_next = False
        i = 0
        while i < len(cmd):
            if skip_next:
                skip_next = False
                i += 1
                continue
            c = cmd[i]
            if c == "--max-batch-txs":
                out.extend([c, str(max_batch)])
                skip_next = True
            elif c == "--batch-interval":
                out.extend([c, str(interval)])
                skip_next = True
            elif c.startswith("--adaptive-batch"):
                out.append(f"--adaptive-batch={'true' if adaptive else 'false'}")
            else:
                out.append(c)
            i += 1
        return out

    sc.node_cmd = node_cmd_wrapped
    return orig


def val_snap():
    out = {}
    for i in range(1, N + 1):
        nid = f"val{i}"
        try:
            m = sc.metrics(sc.host_for_val(N, i), sc.VAL_RPC[i])
            ps = m.get("pack_stall") or {}
            out[nid] = {
                "exec": m.get("executed_txs") or 0,
                "batches": m.get("batches_created") or 0,
                "certified": m.get("certified") or 0,
                "lag": ps.get("admit_exec_lag") or 0,
                "mempool": ps.get("mempool") or m.get("mempool") or 0,
                "root": (m.get("state_root") or "")[:16],
            }
        except Exception as e:
            out[nid] = {"error": str(e)}
    return out


def run_cell(max_batch: int, interval: float, adaptive: bool, gmp: str, network_id: int) -> dict:
    label = f"mb{max_batch}_iv{interval}"
    print(f"\n===== {label} adaptive={adaptive} =====", flush=True)
    orig_cmd = patch_node_cmd(max_batch, interval, adaptive)
    try:
        os.environ["GMP_URL"] = gmp
        os.environ["GCP_URL"] = gmp.replace(":18500", ":18501")
        os.environ["GMP_BUDGET"] = "4096"
        os.environ["GMP_MAX_IF"] = "48"
        os.environ["GMP_LEASE_MAX"] = "8"
        os.environ["GMP_AFFINITY"] = "1"
        for i in range(1, 7):
            os.environ.pop(f"GMP_BUDGET_val{i}", None)
            os.environ.pop(f"GMP_MAX_IF_val{i}", None)

        sc.SINGLE_PACKER = False
        sc.PACK_OPEN = 0
        sc.stop_all()
        sc.wipe_data()
        sc.ensure_keys(N)
        sc.sh(f"rsync -az {sc.BIN} {sc.SSH_F2}:{sc.MESH}/bin/", check=False)
        sc.sh(f"rsync -az --delete {sc.MESH}/keys/ {sc.SSH_F2}:{sc.MESH}/keys/", check=False)
        sc.sh(f"rsync -az --delete {sc.MESH}/wallets/ {sc.SSH_F2}:{sc.MESH}/wallets/", check=False)
        sc.start_mesh(N, network_id)
        time.sleep(2.5)

        # warm tiny
        stats_w = {}
        client_flood(gmp, 3.0, 15, stats_w)
        time.sleep(2)

        before = val_snap()
        stats = {}
        t0 = time.time()
        print(f"  flood rate={RATE}/s hold={HOLD}s", flush=True)
        client_flood(gmp, HOLD, RATE, stats)
        print(f"  drain {DRAIN}s…", flush=True)
        time.sleep(DRAIN)
        after = val_snap()
        elapsed = time.time() - t0

        exec_d = []
        batch_d = []
        cert_d = []
        lags = []
        roots = set()
        for i in range(1, N + 1):
            nid = f"val{i}"
            b, a = before.get(nid, {}), after.get(nid, {})
            if "error" in a or "error" in b:
                continue
            exec_d.append(int(a.get("exec", 0)) - int(b.get("exec", 0)))
            batch_d.append(int(a.get("batches", 0)) - int(b.get("batches", 0)))
            cert_d.append(int(a.get("certified", 0)) - int(b.get("certified", 0)))
            lags.append(a.get("lag") or 0)
            if a.get("root"):
                roots.add(a["root"])

        exec_min = min(exec_d) if exec_d else 0
        exec_max = max(exec_d) if exec_d else 0
        # batches: sum over vals is multi-count; use mean packer batches
        batches_mean = (sum(batch_d) / len(batch_d)) if batch_d else 0
        certs_mean = (sum(cert_d) / len(cert_d)) if cert_d else 0
        tps_min = exec_min / elapsed if elapsed else 0
        tps_max = exec_max / elapsed if elapsed else 0
        batch_ps = batches_mean / elapsed if elapsed else 0
        cert_ps = certs_mean / elapsed if elapsed else 0
        # txs per batch (approx from min exec / mean batches on slowest path)
        # better: total exec min / sum of unique work — use exec_min / max(batches_mean, 1e-9)
        txs_per_batch = (exec_min / batches_mean) if batches_mean > 0.5 else None
        # efficiency: exec per cert
        exec_per_cert = (exec_min / certs_mean) if certs_mean > 0.5 else None

        cell = {
            "label": label,
            "max_batch_txs": max_batch,
            "batch_interval_s": interval,
            "adaptive_batch": adaptive,
            "elapsed_s": round(elapsed, 2),
            "exec_delta": exec_d,
            "batch_delta": batch_d,
            "cert_delta": cert_d,
            "exec_tps_min": round(tps_min, 2),
            "exec_tps_max": round(tps_max, 2),
            "batches_per_s_mean": round(batch_ps, 3),
            "certs_per_s_mean": round(cert_ps, 3),
            "txs_per_batch_approx": round(txs_per_batch, 2) if txs_per_batch else None,
            "exec_per_cert_approx": round(exec_per_cert, 2) if exec_per_cert else None,
            "lag_max": max(lags) if lags else None,
            "exec_spread": (exec_max - exec_min) if exec_d else None,
            "roots_agree": len(roots) <= 1,
            "gmp_ok": stats.get("gmp_ok", 0),
            "gmp_err": stats.get("gmp_err", 0),
            "pass_stable": (
                tps_min >= 3.0
                and (max(lags) if lags else 0) <= 15
                and (exec_max - exec_min) <= 20
            ),
        }
        print(
            f"  → tps_min={cell['exec_tps_min']} batch/s={cell['batches_per_s_mean']} "
            f"cert/s={cell['certs_per_s_mean']} txs/batch≈{cell['txs_per_batch_approx']} "
            f"exec/cert≈{cell['exec_per_cert_approx']} lag={cell['lag_max']}",
            flush=True,
        )
        return cell
    finally:
        sc.node_cmd = orig_cmd


def main():
    print(f"OUT={OUT} F1={sc.F1} F2={sc.F2} RATE={RATE} HOLD={HOLD}", flush=True)
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
            ["--listen", "0.0.0.0:18500", "--shards", "16", "--lease-ttl-s", "10"],
            OUT / "gmpd.log",
        )
    )
    procs.append(
        start_bin("falcon-gcpd", ["--listen", "0.0.0.0:18501"], OUT / "gcpd.log")
    )
    time.sleep(0.5)
    gmp = f"http://{sc.F1}:18500"

    cells = []
    for i, (mb, iv, ad) in enumerate(SWEEP):
        try:
            cells.append(run_cell(mb, iv, ad, gmp, 9800 + i))
        except Exception as e:
            print(f"  CELL FAIL {mb}@{iv}: {e}", flush=True)
            cells.append(
                {
                    "label": f"mb{mb}_iv{iv}",
                    "max_batch_txs": mb,
                    "batch_interval_s": iv,
                    "error": str(e),
                }
            )

    ok = [c for c in cells if "exec_tps_min" in c]
    best = max(ok, key=lambda c: c["exec_tps_min"]) if ok else None
    base = next((c for c in ok if c.get("max_batch_txs") == 64 and c.get("batch_interval_s") == 0.05), None)
    if best and base:
        if best["exec_tps_min"] > base["exec_tps_min"] * 1.08:
            verdict = (
                f"FAT_BATCH_HELPS: best {best['label']} tps={best['exec_tps_min']} "
                f"> baseline64 {base['exec_tps_min']}"
            )
        elif best["exec_tps_min"] >= base["exec_tps_min"] * 0.95:
            verdict = (
                f"FAT_BATCH_FLAT: best {best['label']} tps={best['exec_tps_min']} "
                f"≈ baseline64 {base['exec_tps_min']}"
            )
        else:
            verdict = (
                f"FAT_BATCH_NO_WIN: best {best['label']} tps={best['exec_tps_min']} "
                f"< baseline64 {base['exec_tps_min']}"
            )
    elif best:
        verdict = f"FAT_BATCH_BEST={best['label']} tps={best['exec_tps_min']} (no baseline64 cell)"
    else:
        verdict = "FAT_BATCH_FAIL: no successful cells"

    # amortization signal: among stable cells, does higher txs/batch correlate with higher tps?
    amort = None
    if len(ok) >= 2:
        by_tpb = sorted(
            [c for c in ok if c.get("txs_per_batch_approx")],
            key=lambda c: c["txs_per_batch_approx"],
        )
        if len(by_tpb) >= 2:
            lo, hi = by_tpb[0], by_tpb[-1]
            amort = {
                "lowest_txs_per_batch": lo["txs_per_batch_approx"],
                "highest_txs_per_batch": hi["txs_per_batch_approx"],
                "tps_at_lowest": lo["exec_tps_min"],
                "tps_at_highest": hi["exec_tps_min"],
                "certs_s_at_lowest": lo["certs_per_s_mean"],
                "certs_s_at_highest": hi["certs_per_s_mean"],
            }

    report = {
        "suite": "fat_batch_amortization",
        "hypothesis": "Larger batches amortize UNL cert cost → higher exec_tps_min, lower certs/s",
        "load": {"rate": RATE, "hold_s": HOLD, "drain_s": DRAIN, "n": N},
        "verdict": verdict,
        "best": best,
        "baseline_64_0.05": base,
        "amortization_signal": amort,
        "cells": cells,
    }
    (OUT / "FAT_BATCH_REPORT.json").write_text(json.dumps(report, indent=2) + "\n")

    md = [
        "# Fat-batch amortization dig (N=3 + GMP)",
        "",
        f"**Verdict:** {verdict}",
        "",
        f"Load: {RATE}/s × {HOLD}s + {DRAIN}s drain · multipack N=3 · clients→GMP only",
        "",
        "| max_batch | interval | exec_tps_min | batch/s | cert/s | txs/batch≈ | exec/cert≈ | lag | stable |",
        "|-----------|----------|--------------|---------|--------|------------|------------|-----|--------|",
    ]
    for c in cells:
        if c.get("error"):
            md.append(f"| {c.get('max_batch_txs')} | {c.get('batch_interval_s')} | ERR | | | | | | |")
            continue
        md.append(
            f"| {c['max_batch_txs']} | {c['batch_interval_s']} | **{c['exec_tps_min']}** | "
            f"{c['batches_per_s_mean']} | {c['certs_per_s_mean']} | {c['txs_per_batch_approx']} | "
            f"{c['exec_per_cert_approx']} | {c['lag_max']} | {c['pass_stable']} |"
        )
    md += [
        "",
        "## How to read",
        "",
        "- **exec_tps_min** — north-star (slowest UNL val)",
        "- **cert/s** — agreement load; should drop if amortization works",
        "- **txs/batch≈** — how much work each pack unit carries",
        "- **exec/cert≈** — work covered per certificate (amortization ratio)",
        "",
        "If fat cells raise exec/cert and tps while cert/s falls → theory confirmed.",
        "",
    ]
    if amort:
        md.append(f"Amortization span: {json.dumps(amort)}")
        md.append("")
    (OUT / "FAT_BATCH_REPORT.md").write_text("\n".join(md))
    print("\n" + "\n".join(md), flush=True)

    sc.stop_all()
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except Exception:
            p.terminate()
    print(f"wrote {OUT}/FAT_BATCH_REPORT.json", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
