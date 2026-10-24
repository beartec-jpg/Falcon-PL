#!/usr/bin/env python3
"""
Component measurement + load ramp for 6v+2a real-mesh.

Phases:
  0) Baseline component snapshot (idle)
  1) Function probes (ping RTT, account_info, submit single)
  2) Load ramp: increasing target submit rate with parallel workers
  3) Stop when: plateau (throughput not rising), error rate high, lag>0 sustained,
     or exec spread large — without requiring a freeze

Outputs JSON report under OUT dir.
"""
from __future__ import annotations

import json
import os
import statistics
import sys
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

F1 = os.environ.get("F1_IP", "192.0.2.1")
F2 = os.environ.get("F2_IP", "192.0.2.2")

NODES = {
    "val1": (F1, 18001),
    "val2": (F1, 18002),
    "val3": (F1, 18003),
    "val4": (F2, 18004),
    "val5": (F2, 18005),
    "val6": (F2, 18006),
    "arch1": (F1, 18011),
    "arch2": (F2, 18012),
}
VALS = ["val1", "val2", "val3", "val4", "val5", "val6"]
ARCHS = ["arch1", "arch2"]
N = len(VALS)
# Number of faucet accounts used by load (genesis funds 100 by default).
ACCOUNTS = max(2, int(os.environ.get("ACCOUNTS", os.environ.get("FAUCET_ACCOUNTS", "40"))))

# Ramp schedule: (label, target_submits_per_sec, hold_s, workers)
# workers = parallel submit threads (owner-sharded)
RAMP = [
    ("r01_warm", 10, 15, 4),
    ("r02_light", 25, 20, 8),
    ("r03_med", 50, 25, 12),
    ("r04_mid", 80, 25, 16),
    ("r05_high", 120, 30, 20),
    ("r06_push", 180, 30, 24),
    ("r07_hard", 250, 30, 32),
    ("r08_peak", 350, 30, 40),
    ("r09_crush", 500, 25, 48),
    ("r10_wall", 700, 20, 56),
]


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


def metrics(vid: str) -> dict:
    h, p = NODES[vid]
    info = ((rpc(h, p, "server_info").get("result") or {}).get("info") or {})
    return info.get("metrics") or {}


def account_shard(account: str) -> int:
    h = 0x811C9DC5
    for b in account.encode():
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h


def owner_idx(account: str) -> int:
    return account_shard(account) % N


def extract_component(m: dict) -> dict:
    """Pull the functional surface we care about from server_info metrics."""
    ps = m.get("pack_stall") or {}
    pull = m.get("pull") or {}
    mesh = m.get("mesh") or m.get("mesh_net") or {}
    mem = ps.get("mempool_status") or {}
    if not isinstance(mem, dict):
        mem = {}
    return {
        "profile": m.get("node_profile"),
        "role": m.get("role"),
        "runs_da": m.get("runs_da"),
        "runs_ledger": m.get("runs_ledger"),
        "executed_txs": m.get("executed_txs"),
        "soft_final_txs": m.get("soft_final_txs"),
        "certified": m.get("certified"),
        "ordered": m.get("ordered"),
        "ordered_lag": m.get("ordered_lag"),
        "batches_created": m.get("batches_created"),
        "known_batches": m.get("known_batches"),
        "uncertified": m.get("uncertified"),
        "mempool": m.get("mempool"),
        "p2p_peers": m.get("p2p_peers"),
        "pack_backpressured": m.get("pack_backpressured"),
        "equivocations": m.get("equivocations"),
        "rejected_batches": m.get("rejected_batches"),
        "rejected_sigs": m.get("rejected_sigs"),
        "rejected_tx_sigs": m.get("rejected_tx_sigs"),
        "bullshark_anchors": m.get("bullshark_anchors"),
        "max_cert_round": m.get("max_cert_round"),
        "tip_bft_round": m.get("tip_bft_round"),
        "ledger_commits": m.get("ledger_commits"),
        "hard_final": m.get("hard_final"),
        "admit_exec_lag": ps.get("admit_exec_lag", m.get("admit_exec_lag")),
        "local_admits_ok": ps.get("local_admits_ok"),
        "local_admits_owned": ps.get("local_admits_owned"),
        "local_admits_not_owned": ps.get("local_admits_not_owned"),
        "need_seed": ps.get("open_needs_body_seed"),
        "own_open_n": ps.get("own_open_n"),
        "uncertified_ps": ps.get("uncertified"),
        "foreign_sig_merged": ps.get("foreign_sig_merged"),
        "ingest_new_ok": ps.get("ingest_new_ok"),
        "ingest_merge_ok": ps.get("ingest_merge_ok"),
        "try_certify_ok": ps.get("try_certify_ok"),
        "mempool_total": mem.get("total", m.get("mempool")),
        "pull_want_sent": pull.get("want_sent") or pull.get("wants_sent"),
        "pull_body_recv": pull.get("body_recv") or pull.get("bodies_recv"),
        "mesh_connected": mesh.get("connected") or mesh.get("n_connected"),
        "state_root": (m.get("state_root") or "")[:16],
    }


def snapshot_all() -> dict:
    out = {}
    for vid in list(NODES):
        try:
            out[vid] = extract_component(metrics(vid))
        except Exception as e:
            out[vid] = {"error": str(e)}
    return out


def summarize_health(snap: dict) -> dict:
    execs = [snap[v].get("executed_txs") for v in VALS if isinstance(snap.get(v, {}).get("executed_txs"), int)]
    arch_e = [snap[a].get("executed_txs") for a in ARCHS if isinstance(snap.get(a, {}).get("executed_txs"), int)]
    lags = [snap[v].get("admit_exec_lag") for v in VALS if "error" not in snap.get(v, {})]
    mempools = [snap[v].get("mempool") or 0 for v in VALS if "error" not in snap.get(v, {})]
    uncert = [snap[v].get("uncertified") or 0 for v in VALS if "error" not in snap.get(v, {})]
    roots = {snap[v].get("state_root") for v in VALS if snap.get(v, {}).get("state_root")}
    return {
        "exec_min": min(execs) if execs else None,
        "exec_max": max(execs) if execs else None,
        "exec_spread": (max(execs) - min(execs)) if execs else None,
        "arch_exec": dict(zip(ARCHS, [snap[a].get("executed_txs") for a in ARCHS])),
        "lag_max": max((x or 0) for x in lags) if lags else None,
        "lag_any_nonzero": any((x or 0) > 0 for x in lags),
        "mempool_max": max(mempools) if mempools else 0,
        "uncert_max": max(uncert) if uncert else 0,
        "state_roots_agree": len(roots) <= 1,
        "nodes_up": sum(1 for v in NODES if "error" not in snap.get(v, {})),
        "vals_up": sum(1 for v in VALS if "error" not in snap.get(v, {})),
    }


def probe_functions() -> dict:
    """Measure actual function surfaces: RPC latency, admit path, read path."""
    results = {"ping_rtt_ms": {}, "account_info_ms": {}, "submit_once": {}}

    # ping / server_info RTT per node
    for vid, (h, p) in NODES.items():
        samples = []
        for _ in range(5):
            t0 = time.perf_counter()
            try:
                rpc(h, p, "server_info")
                samples.append((time.perf_counter() - t0) * 1000)
            except Exception as e:
                results["ping_rtt_ms"][vid] = {"error": str(e)}
                break
        else:
            results["ping_rtt_ms"][vid] = {
                "p50": round(statistics.median(samples), 2),
                "p95": round(sorted(samples)[int(0.95 * (len(samples) - 1))], 2),
                "mean": round(statistics.mean(samples), 2),
            }

    # account_info on each val
    for vid in VALS:
        h, p = NODES[vid]
        samples = []
        ok = True
        for _ in range(3):
            t0 = time.perf_counter()
            try:
                rpc(h, p, "account_info", {"account": "acct1"})
                samples.append((time.perf_counter() - t0) * 1000)
            except Exception as e:
                results["account_info_ms"][vid] = {"error": str(e)}
                ok = False
                break
        if ok:
            results["account_info_ms"][vid] = {
                "mean": round(statistics.mean(samples), 2),
                "p50": round(statistics.median(samples), 2),
            }

    # single submit per owner lane
    seq_lock = threading.Lock()
    seq = {}
    for a in range(1, ACCOUNTS + 1):
        acct = f"acct{a}"
        # seed from any val
        seq[acct] = 1
        for vid in VALS:
            try:
                r = (rpc(*NODES[vid], "account_info", {"account": acct}).get("result") or {})
                if "sequence" in r:
                    seq[acct] = int(r["sequence"])
                    break
            except Exception:
                pass

    for vid in VALS:
        # find an account owned by this val
        acct = None
        for a in range(1, ACCOUNTS + 1):
            c = f"acct{a}"
            if owner_idx(c) == VALS.index(vid):
                acct = c
                break
        if not acct:
            continue
        h, p = NODES[vid]
        with seq_lock:
            s = seq[acct]
        dest = f"acct{ACCOUNTS}" if acct != f"acct{ACCOUNTS}" else f"acct{max(1, ACCOUNTS - 1)}"
        t0 = time.perf_counter()
        try:
            d = rpc(
                h,
                p,
                "submit",
                {
                    "account": acct,
                    "sequence": s,
                    "destination": dest,
                    "amount": 1,
                    "fee": 10,
                },
            )
            code = (d.get("result") or {}).get("engine_result")
            ms = (time.perf_counter() - t0) * 1000
            if code == "tesSUCCESS":
                with seq_lock:
                    seq[acct] = s + 1
            results["submit_once"][vid] = {"code": code, "ms": round(ms, 2), "account": acct}
        except Exception as e:
            results["submit_once"][vid] = {"error": str(e)}

    # wait briefly for exec of probe txs
    time.sleep(2)
    results["post_probe_health"] = summarize_health(snapshot_all())
    sample_keys = [f"acct{i}" for i in (1, 2, 10, min(40, ACCOUNTS), ACCOUNTS)]
    results["seq_after_probe"] = {k: seq[k] for k in sample_keys if k in seq}
    results["accounts"] = ACCOUNTS
    return results, seq


class SeqBank:
    def __init__(self, initial: dict):
        self._lock = threading.Lock()
        self._seq = dict(initial)

    def reserve_lowest(self, accts: list[str]) -> tuple[str, int]:
        """Atomically pick lowest-seq account and advance local counter (optimistic)."""
        with self._lock:
            acct = min(accts, key=lambda a: self._seq[a])
            s = self._seq[acct]
            self._seq[acct] = s + 1
            return acct, s

    def rollback(self, acct: str, used: int):
        """Undo reserve if submit failed before chain advanced."""
        with self._lock:
            # only rewind if we still own the next slot
            if self._seq.get(acct) == used + 1:
                self._seq[acct] = used

    def resync(self, acct: str):
        for vid in VALS:
            try:
                r = (rpc(*NODES[vid], "account_info", {"account": acct}, timeout=2.0).get("result") or {})
                if "sequence" in r:
                    with self._lock:
                        self._seq[acct] = int(r["sequence"])
                    return
            except Exception:
                continue

    def snapshot(self) -> dict:
        with self._lock:
            return dict(self._seq)


def run_load_phase(label: str, rate: int, hold: float, workers: int, bank: SeqBank) -> dict:
    """Parallel owner-routed load for `hold` seconds targeting `rate` submits/s."""
    stop_at = time.time() + hold
    counts = {"ok": 0, "busy": 0, "err": 0}
    codes: dict[str, int] = {}
    by_node = {v: 0 for v in VALS}
    lock = threading.Lock()
    accts = [f"acct{i}" for i in range(1, ACCOUNTS + 1)]
    dest_hi = f"acct{ACCOUNTS}"
    dest_lo = f"acct{max(1, ACCOUNTS - 1)}"

    def one_submit():
        acct, s = bank.reserve_lowest(accts)
        oi = owner_idx(acct)
        vid = VALS[oi]
        h, p = NODES[vid]
        dest = dest_hi if acct != dest_hi else dest_lo
        try:
            d = rpc(
                h,
                p,
                "submit",
                {"account": acct, "sequence": s, "destination": dest, "amount": 1, "fee": 10},
                timeout=3.0,
            )
            code = (d.get("result") or {}).get("engine_result") or "null"
            with lock:
                if code == "tesSUCCESS":
                    counts["ok"] += 1
                    by_node[vid] += 1
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
                name = type(e).__name__
                codes[name] = codes.get(name, 0) + 1

    t0 = time.time()
    before = snapshot_all()
    before_h = summarize_health(before)
    mid_samples = []

    # pacing: launch `rate` submits per second using worker pool
    with ThreadPoolExecutor(max_workers=workers) as ex:
        while time.time() < stop_at:
            sec0 = time.time()
            futs = [ex.submit(one_submit) for _ in range(rate)]
            for f in as_completed(futs):
                _ = f.result()
            # mid health every ~5s worth
            if len(mid_samples) == 0 or (time.time() - t0) > 5 * (len(mid_samples) + 1):
                try:
                    mid_samples.append({"t": round(time.time() - t0, 1), "health": summarize_health(snapshot_all()), "counts": dict(counts)})
                except Exception:
                    pass
            # pace to ~1s cadence (if work finished early, sleep)
            elapsed = time.time() - sec0
            if elapsed < 1.0:
                time.sleep(1.0 - elapsed)

    # settle a moment for exec catch-up of in-flight
    time.sleep(2.0)
    after = snapshot_all()
    after_h = summarize_health(after)
    elapsed = time.time() - t0

    def delta_field(field: str):
        out = {}
        for v in VALS + ARCHS:
            a, b = before.get(v, {}), after.get(v, {})
            if isinstance(a.get(field), (int, float)) and isinstance(b.get(field), (int, float)):
                out[v] = b[field] - a[field]
        return out

    exec_delta = delta_field("executed_txs")
    cert_delta = delta_field("certified")
    batch_delta = delta_field("batches_created")
    admit_delta = delta_field("local_admits_ok")
    val_exec_d = [exec_delta.get(v, 0) for v in VALS]
    min_d = min(val_exec_d) if val_exec_d else 0
    max_d = max(val_exec_d) if val_exec_d else 0

    submit_tps = counts["ok"] / elapsed if elapsed > 0 else 0
    exec_tps = min_d / elapsed if elapsed > 0 else 0
    err_rate = counts["err"] / max(1, counts["ok"] + counts["err"] + counts["busy"])

    phase = {
        "label": label,
        "target_rate": rate,
        "hold_s": hold,
        "workers": workers,
        "elapsed_s": round(elapsed, 2),
        "ok": counts["ok"],
        "busy": counts["busy"],
        "err": counts["err"],
        "err_rate": round(err_rate, 4),
        "err_codes": codes,
        "submits_by_owner": by_node,
        "submit_tps": round(submit_tps, 2),
        "exec_tps_min": round(exec_tps, 2),
        "exec_delta": exec_delta,
        "exec_delta_min": min_d,
        "exec_delta_max": max_d,
        "exec_delta_spread": max_d - min_d,
        "cert_delta": cert_delta,
        "batch_delta": batch_delta,
        "admit_delta": admit_delta,
        "health_before": before_h,
        "health_after": after_h,
        "lag_after": after_h.get("lag_max"),
        "spread_after": after_h.get("exec_spread"),
        "mempool_max_after": after_h.get("mempool_max"),
        "mid_samples": mid_samples[-6:],
        "component_after_sample": {v: after.get(v) for v in ("val1", "val4", "arch1", "arch2")},
    }

    # quality flags
    phase["flags"] = {
        "healthy_lag": not after_h.get("lag_any_nonzero"),
        "low_err": err_rate < 0.05,
        "exec_progress": min_d >= max(1, int(counts["ok"] * 0.3)),
        "spread_ok": (after_h.get("exec_spread") or 0) <= max(30, int(0.1 * (after_h.get("exec_min") or 1))),
        "all_vals_up": after_h.get("vals_up") == 6,
        "arch_tracking": all(
            isinstance(after.get(a, {}).get("executed_txs"), int)
            and abs(after[a]["executed_txs"] - (after_h.get("exec_min") or 0)) <= 40
            for a in ARCHS
        ),
    }
    phase["pass"] = all(phase["flags"].values())
    return phase


def should_stop(phases: list[dict]) -> tuple[bool, str]:
    if len(phases) < 2:
        return False, ""
    last = phases[-1]
    prev = phases[-2]
    # hard stop conditions
    if last["health_after"].get("vals_up", 0) < 6:
        return True, "validator_down"
    if last.get("lag_after", 0) and last["lag_after"] > 0 and last["err_rate"] > 0.1:
        return True, "lag_and_errors"
    if last["err_rate"] > 0.25 and last["submit_tps"] < prev["submit_tps"] * 0.7:
        return True, "error_collapse"
    # plateau: two steps up in target rate but exec_tps not improving >10%
    if last["target_rate"] > prev["target_rate"] and last["exec_tps_min"] < prev["exec_tps_min"] * 1.1:
        # only stop if we're also not absorbing (high err or busy)
        if last["err_rate"] > 0.08 or last["busy"] > last["ok"] * 0.1:
            return True, "throughput_plateau_with_pressure"
        # soft plateau: three consecutive non-improving
        if len(phases) >= 3:
            a, b, c = phases[-3], phases[-2], phases[-1]
            if c["exec_tps_min"] <= b["exec_tps_min"] * 1.05 and b["exec_tps_min"] <= a["exec_tps_min"] * 1.05:
                if c["target_rate"] > a["target_rate"]:
                    return True, "sustained_exec_tps_plateau"
    return False, ""


def main():
    out = Path(os.environ.get("OUT", f"/tmp/real-mesh-measure-{int(time.time())}"))
    out.mkdir(parents=True, exist_ok=True)
    print(f"OUT={out}", flush=True)

    report = {
        "suite": "6v2a_component_measure_ramp",
        "topology": {
            "vals": VALS,
            "archs": ARCHS,
            "hosts": {"host1": ["val1", "val2", "val3", "arch1"], "host2": ["val4", "val5", "val6", "arch2"]},
            "transport": "LAN",
            "accounts": ACCOUNTS,
        },
        "phases": [],
    }
    print(f"ACCOUNTS={ACCOUNTS}", flush=True)

    print("=== PHASE0 baseline components ===", flush=True)
    base = snapshot_all()
    report["baseline"] = base
    report["baseline_health"] = summarize_health(base)
    print(json.dumps(report["baseline_health"], indent=2), flush=True)
    (out / "baseline.json").write_text(json.dumps(base, indent=2) + "\n")

    print("=== PHASE1 function probes ===", flush=True)
    probes, seq0 = probe_functions()
    report["function_probes"] = probes
    print(json.dumps({
        "ping_sample": probes["ping_rtt_ms"].get("val1"),
        "ping_cross_host": probes["ping_rtt_ms"].get("val4"),
        "submit_sample": probes["submit_once"],
        "post_health": probes["post_probe_health"],
    }, indent=2), flush=True)
    (out / "probes.json").write_text(json.dumps(probes, indent=2) + "\n")

    bank = SeqBank(seq0)
    # re-seed fully
    for a in range(1, ACCOUNTS + 1):
        bank.resync(f"acct{a}")

    print("=== PHASE2 load ramp ===", flush=True)
    stop_reason = None
    for label, rate, hold, workers in RAMP:
        print(f"\n--- {label} target={rate}/s hold={hold}s workers={workers} ---", flush=True)
        phase = run_load_phase(label, rate, hold, workers, bank)
        report["phases"].append(phase)
        print(
            f"  ok={phase['ok']} err={phase['err']} busy={phase['busy']} "
            f"submit_tps={phase['submit_tps']} exec_tps_min={phase['exec_tps_min']} "
            f"spread={phase['spread_after']} lag={phase['lag_after']} "
            f"err_rate={phase['err_rate']} pass={phase['pass']} flags={phase['flags']}",
            flush=True,
        )
        (out / f"phase_{label}.json").write_text(json.dumps(phase, indent=2) + "\n")
        stop, reason = should_stop(report["phases"])
        if stop:
            stop_reason = reason
            print(f"STOP ramp: {reason}", flush=True)
            break
        # brief cool-down so we measure recovery too
        time.sleep(3)
        cool = summarize_health(snapshot_all())
        phase["cooldown_health"] = cool
        print(f"  cooldown exec_spread={cool.get('exec_spread')} lag={cool.get('lag_max')}", flush=True)

    print("=== PHASE3 final settle ===", flush=True)
    time.sleep(10)
    final = snapshot_all()
    report["final"] = final
    report["final_health"] = summarize_health(final)
    report["stop_reason"] = stop_reason

    # Analysis summary
    best = max(report["phases"], key=lambda p: p.get("exec_tps_min") or 0) if report["phases"] else None
    last_pass = None
    for p in report["phases"]:
        if p.get("pass"):
            last_pass = p
    analysis = {
        "best_exec_tps_phase": None if not best else {
            "label": best["label"],
            "exec_tps_min": best["exec_tps_min"],
            "submit_tps": best["submit_tps"],
            "target_rate": best["target_rate"],
        },
        "last_full_pass_phase": None if not last_pass else {
            "label": last_pass["label"],
            "exec_tps_min": last_pass["exec_tps_min"],
            "target_rate": last_pass["target_rate"],
        },
        "stop_reason": stop_reason,
        "final_health": report["final_health"],
        "component_notes": {},
    }

    # Component assessment from deltas across phases
    if report["phases"]:
        p0, pL = report["phases"][0], report["phases"][-1]
        analysis["component_notes"] = {
            "admit_path": "owner-route parallel submits; check admit_delta vs ok",
            "pack_path": {
                "batch_delta_first": p0.get("batch_delta"),
                "batch_delta_last": pL.get("batch_delta"),
            },
            "order_exec": {
                "exec_tps_best": analysis["best_exec_tps_phase"],
                "ordered_lag_final": {v: final.get(v, {}).get("ordered_lag") for v in VALS},
            },
            "p2p": {
                "peers_final": {v: final.get(v, {}).get("p2p_peers") for v in NODES},
            },
            "archival": {
                "runs_da": {a: final.get(a, {}).get("runs_da") for a in ARCHS},
                "runs_ledger": {a: final.get(a, {}).get("runs_ledger") for a in ARCHS},
                "exec": {a: final.get(a, {}).get("executed_txs") for a in ARCHS},
                "batches_created": {a: final.get(a, {}).get("batches_created") for a in ARCHS},
            },
            "rpc_rtt": {
                "local_val": probes["ping_rtt_ms"].get("val1"),
                "remote_val": probes["ping_rtt_ms"].get("val4"),
                "local_arch": probes["ping_rtt_ms"].get("arch1"),
            },
        }
    report["analysis"] = analysis

    (out / "REPORT.json").write_text(json.dumps(report, indent=2) + "\n")

    # Human markdown
    lines = [
        "# 6v+2a measurement + ramp report",
        "",
        f"OUT: `{out}`",
        f"Stop reason: **{stop_reason or 'completed full ramp'}**",
        "",
        "## Baseline health",
        "```",
        json.dumps(report["baseline_health"], indent=2),
        "```",
        "",
        "## Function probes (RTT / submit)",
        f"- val1 server_info: {probes['ping_rtt_ms'].get('val1')}",
        f"- val4 (cross-host) server_info: {probes['ping_rtt_ms'].get('val4')}",
        f"- arch1: {probes['ping_rtt_ms'].get('arch1')}",
        "",
        "## Ramp phases",
        "",
        "| Phase | target/s | workers | ok | err | submit_tps | exec_tps_min | spread | lag | pass |",
        "|-------|----------|---------|----|-----|------------|--------------|--------|-----|------|",
    ]
    for p in report["phases"]:
        lines.append(
            f"| {p['label']} | {p['target_rate']} | {p['workers']} | {p['ok']} | {p['err']} | "
            f"{p['submit_tps']} | {p['exec_tps_min']} | {p['spread_after']} | {p['lag_after']} | {p['pass']} |"
        )
    lines += [
        "",
        "## Best / last pass",
        f"- Best exec TPS: {analysis.get('best_exec_tps_phase')}",
        f"- Last full pass: {analysis.get('last_full_pass_phase')}",
        "",
        "## Final health",
        "```",
        json.dumps(report["final_health"], indent=2),
        "```",
        "",
    ]
    (out / "REPORT.md").write_text("\n".join(lines) + "\n")
    print("\n" + "\n".join(lines), flush=True)
    print(f"\nWrote {out}/REPORT.json and REPORT.md", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
