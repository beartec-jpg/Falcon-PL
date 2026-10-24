#!/usr/bin/env python3
"""
30-minute (default) multi-RPC soak with phased load:

  phase soak   — low steady rate
  phase ramp_up — linear to peak_tps
  phase hold   — stay at peak
  phase ramp_down — linear to 0
  phase idle   — no submits; measure drain/convergence

Samples server_info every sample_s. Writes JSON report.
"""
from __future__ import annotations

import argparse
import json
import statistics
import threading
import time
import urllib.request
from collections import Counter


def rpc(host: str, port: int, method: str, params: dict, timeout: float = 8.0) -> dict:
    body = json.dumps({"method": method, "params": [params]}).encode()
    req = urllib.request.Request(
        f"http://{host}:{port}",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def metrics(host: str, port: int) -> dict:
    return rpc(host, port, "server_info", {})["result"]["info"]["metrics"]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--base-rpc", type=int, default=28200)
    ap.add_argument("--validators", type=int, default=5)
    ap.add_argument("--accounts", type=int, default=20)
    ap.add_argument("--duration", type=float, default=1800.0, help="total seconds (default 30m)")
    ap.add_argument("--soak-s", type=float, default=300.0)
    ap.add_argument("--ramp-up-s", type=float, default=300.0)
    ap.add_argument("--hold-s", type=float, default=300.0)
    ap.add_argument("--ramp-down-s", type=float, default=300.0)
    # remainder = idle
    ap.add_argument("--soak-tps", type=float, default=5.0)
    ap.add_argument("--peak-tps", type=float, default=80.0)
    ap.add_argument("--sample-s", type=float, default=5.0)
    ap.add_argument("--report", default="/tmp/fd-soak-ramp-report.json")
    ap.add_argument("--label", default="default")
    args = ap.parse_args()

    total = args.duration
    idle_s = max(0.0, total - (args.soak_s + args.ramp_up_s + args.hold_s + args.ramp_down_s))
    phases = [
        ("soak", args.soak_s, args.soak_tps, args.soak_tps),
        ("ramp_up", args.ramp_up_s, args.soak_tps, args.peak_tps),
        ("hold", args.hold_s, args.peak_tps, args.peak_tps),
        ("ramp_down", args.ramp_down_s, args.peak_tps, 0.0),
        ("idle", idle_s, 0.0, 0.0),
    ]

    ports = [args.base_rpc + i for i in range(args.validators)]
    # wait for ping
    for p in ports:
        for _ in range(100):
            try:
                rpc(args.host, p, "ping", {})
                break
            except Exception:
                time.sleep(0.1)

    seqs = {f"acct{i}": 1 for i in range(1, args.accounts + 1)}
    lock = threading.Lock()
    submitted_ok = 0
    submitted_fail = 0
    codes: Counter[str] = Counter()
    latencies: list[float] = []
    samples: list[dict] = []
    stop = threading.Event()
    t0 = time.time()
    submit_i = 0

    def target_tps_at(elapsed: float) -> tuple[str, float]:
        t = 0.0
        for name, dur, a, b in phases:
            if elapsed < t + dur or name == phases[-1][0]:
                if dur <= 0:
                    return name, b
                u = min(1.0, max(0.0, (elapsed - t) / dur))
                return name, a + (b - a) * u
            t += dur
        return "idle", 0.0

    def one_submit(k: int) -> None:
        nonlocal submitted_ok, submitted_fail, submit_i
        src = f"acct{(k % args.accounts) + 1}"
        dst = f"acct{((k + 1) % args.accounts) + 1}"
        with lock:
            s = seqs[src]
            seqs[src] = s + 1
        port = ports[k % len(ports)]
        t_s = time.time()
        try:
            res = rpc(
                args.host,
                port,
                "submit",
                {
                    "account": src,
                    "sequence": s,
                    "destination": dst,
                    "amount": 1,
                    "fee": 10,
                },
            )
            code = (res.get("result") or res).get("engine_result") or "?"
            dt = time.time() - t_s
            with lock:
                latencies.append(dt)
                codes[code] += 1
                if code == "tesSUCCESS":
                    submitted_ok += 1
                else:
                    submitted_fail += 1
        except Exception as e:
            with lock:
                codes[f"exc:{type(e).__name__}"] += 1
                submitted_fail += 1

    def sampler() -> None:
        while not stop.is_set():
            elapsed = time.time() - t0
            phase, tps = target_tps_at(elapsed)
            try:
                ms = []
                for p in ports:
                    try:
                        ms.append(metrics(args.host, p))
                    except Exception:
                        ms.append({})
                execs = [m.get("executed_txs", -1) for m in ms]
                valid = [e for e in execs if e is not None and e >= 0]
                samples.append(
                    {
                        "t": round(elapsed, 2),
                        "phase": phase,
                        "target_tps": round(tps, 2),
                        "submitted_ok": submitted_ok,
                        "submitted_fail": submitted_fail,
                        "executed_by_val": execs,
                        "executed_max": max(valid) if valid else 0,
                        "executed_min": min(valid) if valid else 0,
                        "executed_spread": (max(valid) - min(valid)) if valid else None,
                        "mempool": ms[0].get("mempool") if ms else None,
                        "certified": ms[0].get("certified") if ms else None,
                        "batches": ms[0].get("batches_created") if ms else None,
                        "equivocations": ms[0].get("equivocations") if ms else None,
                        "rejected_batches": ms[0].get("rejected_batches") if ms else None,
                    }
                )
            except Exception as e:
                samples.append({"t": round(elapsed, 2), "error": str(e)})
            stop.wait(args.sample_s)

    th = threading.Thread(target=sampler, daemon=True)
    th.start()

    next_at = time.time()
    while time.time() - t0 < total:
        elapsed = time.time() - t0
        phase, tps = target_tps_at(elapsed)
        if tps <= 0.01:
            time.sleep(0.2)
            continue
        one_submit(submit_i)
        submit_i += 1
        interval = 1.0 / tps
        next_at += interval
        sleep = next_at - time.time()
        if sleep > 0:
            time.sleep(sleep)
        elif sleep < -1:
            # fell behind: reset schedule
            next_at = time.time()

    # final sample
    time.sleep(2.0)
    stop.set()
    th.join(timeout=3)

    t1 = time.time()
    elapsed = t1 - t0
    final = []
    for p in ports:
        try:
            final.append(metrics(args.host, p))
        except Exception as e:
            final.append({"error": str(e)})
    execs = [m.get("executed_txs", 0) for m in final if "executed_txs" in m]
    lat_ms = [x * 1000 for x in latencies]
    lat_ms.sort()

    def pct(p: float):
        if not lat_ms:
            return None
        i = min(len(lat_ms) - 1, int(p / 100.0 * (len(lat_ms) - 1)))
        return round(lat_ms[i], 2)

    # phase apply rates from samples
    phase_stats = {}
    for s in samples:
        ph = s.get("phase") or "?"
        phase_stats.setdefault(ph, []).append(s)

    report = {
        "ok": True,
        "label": args.label,
        "duration_s": round(elapsed, 2),
        "phases_plan": [
            {"name": n, "seconds": d, "tps_from": a, "tps_to": b}
            for n, d, a, b in phases
        ],
        "validators": args.validators,
        "accounts": args.accounts,
        "submitted_ok": submitted_ok,
        "submitted_fail": submitted_fail,
        "admit_ok_rate": round(
            submitted_ok / max(1, submitted_ok + submitted_fail), 4
        ),
        "engine_codes": dict(codes),
        "submit_tps_avg": round(submitted_ok / max(elapsed, 0.001), 2),
        "executed_max": max(execs) if execs else 0,
        "executed_min": min(execs) if execs else 0,
        "executed_spread": (max(execs) - min(execs)) if execs else None,
        "apply_tps_avg": round((max(execs) if execs else 0) / max(elapsed, 0.001), 2),
        "submit_latency_ms": {
            "p50": pct(50),
            "p95": pct(95),
            "p99": pct(99),
            "mean": round(statistics.mean(lat_ms), 2) if lat_ms else None,
        },
        "final_metrics": final,
        "sample_count": len(samples),
        "samples": samples,
    }
    with open(args.report, "w") as f:
        json.dump(report, f, indent=2)

    summary = {k: report[k] for k in report if k not in ("samples", "final_metrics")}
    summary["final_executed_by_val"] = execs
    print(json.dumps(summary, indent=2))
    print(f"report: {args.report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
