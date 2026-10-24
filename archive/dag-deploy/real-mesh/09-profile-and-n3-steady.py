#!/usr/bin/env python3
"""
Continue program after scale-out FAIL:

A) Profile N=6 multipack under med load — where do CPU / pipeline counters go?
B) Lock N=3 multipack and run multi-client local-only load + short soak

Writes OUT/PROFILE_N6.json, OUT/N3_STEADY.json, OUT/CONTINUE_REPORT.md
"""
from __future__ import annotations

import importlib.util
import json
import os
import statistics
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parent
OUT = Path(os.environ.get("OUT", f"/tmp/continue-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)

# load scale helpers without running main
_spec = importlib.util.spec_from_file_location("scale_n", ROOT / "07-scale-n-curve.py")
sc = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
_spec.loader.exec_module(sc)

SSH_F2 = sc.SSH_F2
F1, F2 = sc.F1, sc.F2
print(f"OUT={OUT} F1={F1} F2={F2} SSH={SSH_F2}", flush=True)


def ps_snapshot(host_local: bool) -> list[dict]:
    cmd = "ps -C falcon-dagd -o pid,rss,pcpu,etime,cmd --no-headers"
    if host_local:
        out = subprocess.check_output(cmd, shell=True, text=True)
    else:
        out = subprocess.check_output(
            ["ssh", "-o", "ConnectTimeout=8", SSH_F2, cmd], text=True
        )
    rows = []
    for line in out.splitlines():
        parts = line.split(None, 4)
        if len(parts) < 5:
            continue
        pid, rss, pcpu, etime, cmd = parts
        nid = "?"
        if "--id" in cmd:
            try:
                nid = cmd.split("--id", 1)[1].split()[0]
            except Exception:
                pass
        rows.append(
            {
                "pid": int(pid),
                "rss_mi": round(int(rss) / 1024, 1),
                "pcpu": float(pcpu),
                "etime": etime,
                "id": nid,
                "host": "f1" if host_local else "f2",
            }
        )
    return rows


def metric_pipeline(m: dict) -> dict:
    ps = m.get("pack_stall") or {}
    pull = m.get("pull") or {}
    return {
        "executed_txs": m.get("executed_txs"),
        "certified": m.get("certified"),
        "batches_created": m.get("batches_created"),
        "ordered": m.get("ordered"),
        "uncertified": m.get("uncertified"),
        "mempool": m.get("mempool"),
        "p2p_peers": m.get("p2p_peers"),
        "lag": ps.get("admit_exec_lag", m.get("admit_exec_lag")),
        "local_admits_ok": ps.get("local_admits_ok"),
        "foreign_sig_merged": ps.get("foreign_sig_merged"),
        "ingest_new_ok": ps.get("ingest_new_ok"),
        "ingest_merge_ok": ps.get("ingest_merge_ok"),
        "try_certify_attempts": ps.get("try_certify_attempts"),
        "try_certify_ok": ps.get("try_certify_ok"),
        "own_open_n": ps.get("own_open_n"),
        "need_seed": ps.get("open_needs_body_seed"),
        "pull": pull,
        "pack_skip": m.get("pack_skip_reasons"),
    }


def delta_num(a: dict, b: dict, keys: list[str]) -> dict:
    out = {}
    for k in keys:
        if isinstance(a.get(k), (int, float)) and isinstance(b.get(k), (int, float)):
            out[k] = b[k] - a[k]
    return out


def profile_n6() -> dict:
    """Bring up N=6 multipack, load, sample process+pipeline counters."""
    print("\n===== A) PROFILE N=6 multipack =====", flush=True)
    sc.SINGLE_PACKER = False
    n = 6
    sc.stop_all()
    sc.wipe_data()
    sc.ensure_keys(n)
    sc.sh(f"rsync -az --delete {sc.MESH}/keys/ {SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/wallets/ {SSH_F2}:{sc.MESH}/wallets/", check=False)
    sc.sh(f"rsync -az {sc.BIN} {SSH_F2}:{sc.MESH}/bin/", check=False)
    sc.start_mesh(n, sc.NETWORK_BASE + 6)
    time.sleep(2)

    bank = sc.SeqBank(n)
    before_ps = {"f1": ps_snapshot(True), "f2": ps_snapshot(False)}
    before_snap = sc.snap_vals(n)
    before_pipe = {}
    for i in range(1, n + 1):
        nid = sc.val_id(i)
        h = sc.host_for_val(n, i)
        m = sc.metrics(h, sc.VAL_RPC[i])
        before_pipe[nid] = metric_pipeline(m)

    samples = []
    hold = 30.0
    rate = 50
    workers = 16
    t0 = time.time()
    stop_at = t0 + hold
    counts = {"ok": 0, "busy": 0, "err": 0}
    lock = threading.Lock()
    accts = [f"acct{i}" for i in range(1, 41)]

    def one():
        acct, s = bank.reserve_lowest(accts)
        oi = bank.owner_idx(acct)
        vi = oi + 1
        h = sc.host_for_val(n, vi)
        dest = "acct40" if acct != "acct40" else "acct39"
        try:
            d = sc.rpc(
                h,
                sc.VAL_RPC[vi],
                "submit",
                {"account": acct, "sequence": s, "destination": dest, "amount": 1, "fee": 10},
                timeout=3.0,
            )
            code = (d.get("result") or {}).get("engine_result")
            with lock:
                if code == "tesSUCCESS":
                    counts["ok"] += 1
                elif code == "terBUSY":
                    counts["busy"] += 1
                    bank.rollback(acct, s)
                else:
                    counts["err"] += 1
                    bank.resync(acct) if code in ("tecSEQ_PAST", "tefALREADY", "tefPAST_SEQ") else bank.rollback(acct, s)
        except Exception:
            bank.rollback(acct, s)
            with lock:
                counts["err"] += 1

    def sampler():
        while time.time() < stop_at:
            try:
                samples.append(
                    {
                        "t": round(time.time() - t0, 1),
                        "ps": {"f1": ps_snapshot(True), "f2": ps_snapshot(False)},
                        "health": sc.health(n, sc.snap_vals(n)),
                    }
                )
            except Exception as e:
                samples.append({"t": round(time.time() - t0, 1), "error": str(e)})
            time.sleep(5)

    th = threading.Thread(target=sampler, daemon=True)
    th.start()
    with ThreadPoolExecutor(max_workers=workers) as ex:
        while time.time() < stop_at:
            sec0 = time.time()
            futs = [ex.submit(one) for _ in range(rate)]
            for f in as_completed(futs):
                _ = f.result()
            left = 1.0 - (time.time() - sec0)
            if left > 0:
                time.sleep(left)
    time.sleep(2)
    th.join(timeout=1)

    after_ps = {"f1": ps_snapshot(True), "f2": ps_snapshot(False)}
    after_snap = sc.snap_vals(n)
    after_pipe = {}
    for i in range(1, n + 1):
        nid = sc.val_id(i)
        h = sc.host_for_val(n, i)
        m = sc.metrics(h, sc.VAL_RPC[i])
        after_pipe[nid] = metric_pipeline(m)

    keys = [
        "executed_txs",
        "certified",
        "batches_created",
        "ordered",
        "local_admits_ok",
        "foreign_sig_merged",
        "ingest_new_ok",
        "ingest_merge_ok",
        "try_certify_attempts",
        "try_certify_ok",
    ]
    pipe_delta = {nid: delta_num(before_pipe[nid], after_pipe[nid], keys) for nid in before_pipe}

    # rank avg cpu during samples
    cpu_by_id: dict[str, list[float]] = {}
    for s in samples:
        for host in ("f1", "f2"):
            for row in (s.get("ps") or {}).get(host) or []:
                cpu_by_id.setdefault(row["id"], []).append(row["pcpu"])
    cpu_mean = {k: round(statistics.mean(v), 1) for k, v in cpu_by_id.items() if v}

    # optional perf on hottest local pid for 8s (best effort)
    perf_note = None
    hot_local = None
    if after_ps["f1"]:
        hot_local = max(after_ps["f1"], key=lambda r: r["pcpu"])
        try:
            perf_out = OUT / "perf_val_hot.txt"
            subprocess.run(
                [
                    "bash",
                    "-lc",
                    f"timeout 8 perf record -g -p {hot_local['pid']} -o {OUT}/perf.data 2>/dev/null; "
                    f"perf report -i {OUT}/perf.data --stdio --no-children 2>/dev/null | head -40 > {perf_out}",
                ],
                check=False,
                timeout=20,
            )
            if perf_out.exists():
                perf_note = perf_out.read_text()[:2000]
        except Exception as e:
            perf_note = f"perf failed: {e}"

    elapsed = time.time() - t0
    execs = []
    for i in range(1, n + 1):
        b = before_snap.get(sc.val_id(i), {}).get("exec")
        a = after_snap.get(sc.val_id(i), {}).get("exec")
        if isinstance(b, int) and isinstance(a, int):
            execs.append(a - b)
    report = {
        "n": 6,
        "mode": "multipack",
        "load": {"rate": rate, "hold": hold, "workers": workers},
        "counts": counts,
        "elapsed_s": round(elapsed, 2),
        "exec_delta_min": min(execs) if execs else None,
        "exec_delta_max": max(execs) if execs else None,
        "exec_tps_min": round(min(execs) / elapsed, 2) if execs else None,
        "cpu_mean_by_id": cpu_mean,
        "rss_after": {
            "f1": after_ps["f1"],
            "f2": after_ps["f2"],
            "f1_total_mi": round(sum(r["rss_mi"] for r in after_ps["f1"]), 1),
            "f2_total_mi": round(sum(r["rss_mi"] for r in after_ps["f2"]), 1),
        },
        "pipeline_delta": pipe_delta,
        "health_after": sc.health(n, after_snap),
        "samples": samples,
        "perf_hot": hot_local,
        "perf_report_head": perf_note,
        "interpretation_hints": interpret_pipeline(pipe_delta, cpu_mean, counts, execs, elapsed),
    }
    (OUT / "PROFILE_N6.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"profile_summary": {
        "exec_tps_min": report["exec_tps_min"],
        "cpu_mean_by_id": cpu_mean,
        "rss_totals": {"f1": report["rss_after"]["f1_total_mi"], "f2": report["rss_after"]["f2_total_mi"]},
        "hints": report["interpretation_hints"],
        "pipe_val1": pipe_delta.get("val1"),
        "pipe_val4": pipe_delta.get("val4"),
    }}, indent=2), flush=True)
    sc.stop_all()
    return report


def interpret_pipeline(pipe_delta, cpu_mean, counts, execs, elapsed) -> list[str]:
    hints = []
    # average deltas across vals
    def avg_key(k):
        vals = [d[k] for d in pipe_delta.values() if k in d]
        return statistics.mean(vals) if vals else 0

    admits = avg_key("local_admits_ok")
    batches = avg_key("batches_created")
    certs = avg_key("certified")
    try_att = avg_key("try_certify_attempts")
    try_ok = avg_key("try_certify_ok")
    foreign = avg_key("foreign_sig_merged")
    exec_d = min(execs) if execs else 0

    if admits > 0 and batches < admits / 20:
        hints.append("PACK_BOUND: few batches_created vs admits — packing/signing not keeping up with admit.")
    if try_att > 0 and try_ok < try_att * 0.05:
        hints.append("CERT_BOUND: try_certify_attempts >> try_certify_ok — quorum/cert not completing.")
    if foreign > certs * 2 and certs > 0:
        hints.append("SIG_GOSSIP_HEAVY: foreign_sig_merged much larger than certs — sig merge work high.")
    if exec_d < counts["ok"] * 0.5:
        hints.append("EXEC_LAGGING_ADMITS: executed min delta << successful submits (pipeline backlog).")
    if cpu_mean:
        hot = max(cpu_mean.items(), key=lambda x: x[1])
        hints.append(f"HOTTEST_PROC: {hot[0]} avg {hot[1]}% CPU during load.")
    if not hints:
        hints.append("No strong single bottleneck from counters; inspect perf report.")
    return hints


def multi_client_n3() -> dict:
    """N=3 multipack: multi-client local-only vs single owner-router client."""
    print("\n===== B) N=3 multipack multi-client + soak =====", flush=True)
    sc.SINGLE_PACKER = False
    n = 3
    sc.stop_all()
    sc.wipe_data()
    sc.ensure_keys(n)
    sc.sh(f"rsync -az --delete {sc.MESH}/keys/ {SSH_F2}:{sc.MESH}/keys/", check=False)
    sc.sh(f"rsync -az --delete {sc.MESH}/wallets/ {SSH_F2}:{sc.MESH}/wallets/", check=False)
    sc.start_mesh(n, sc.NETWORK_BASE + 3)
    time.sleep(2)

    results = {}

    # --- mode single hub client (owner-route from one process) ---
    bank = sc.SeqBank(n)
    results["single_client"] = sc.run_ladder_phase(n, "single_client_med", 40, 25, 12, bank)
    print(
        f"  single_client: exec_tps_min={results['single_client']['exec_tps_min']} "
        f"tight={results['single_client']['pass_tight']}",
        flush=True,
    )
    time.sleep(3)

    # --- multi-client: one worker pool per val, only that val's accounts ---
    bank2 = sc.SeqBank(n)
    results["multi_client"] = run_multi_client(n, rate_per_val=15, hold=25.0, bank=bank2)
    print(
        f"  multi_client: exec_tps_min={results['multi_client']['exec_tps_min']} "
        f"submit_tps={results['multi_client']['submit_tps']}",
        flush=True,
    )
    time.sleep(3)

    # --- soak at conservative rate ---
    bank3 = sc.SeqBank(n)
    results["soak"] = sc.run_ladder_phase(n, "soak_light", 20, 60, 8, bank3)
    print(
        f"  soak 60s@20: exec_tps_min={results['soak']['exec_tps_min']} "
        f"spread={results['soak']['spread_after']} lag={results['soak']['lag_after']}",
        flush=True,
    )

    # final health after settle
    time.sleep(8)
    final = sc.snap_vals(n)
    results["final_health"] = sc.health(n, final)
    results["final_snap"] = final
    results["ps_final"] = {"f1": ps_snapshot(True), "f2": ps_snapshot(False)}

    (OUT / "N3_STEADY.json").write_text(json.dumps(results, indent=2) + "\n")
    # leave N=3 mesh running for user? stop for cleanliness
    # sc.stop_all()  # leave up as operational default
    print("  leaving N=3 multipack mesh RUNNING", flush=True)
    return results


def run_multi_client(n: int, rate_per_val: int, hold: float, bank: sc.SeqBank) -> dict:
    """Each val has dedicated clients that only submit owner accounts to that val."""
    stop_at = time.time() + hold
    counts = {"ok": 0, "busy": 0, "err": 0}
    by_node = {sc.val_id(i): 0 for i in range(1, n + 1)}
    lock = threading.Lock()
    accts_by_val = {i: [] for i in range(1, n + 1)}
    for a in range(1, 41):
        acct = f"acct{a}"
        oi = bank.owner_idx(acct)
        accts_by_val[oi + 1].append(acct)

    before = sc.snap_vals(n)
    t0 = time.time()

    def worker(vi: int):
        accts = accts_by_val[vi] or [f"acct{vi}"]
        h = sc.host_for_val(n, vi)
        port = sc.VAL_RPC[vi]
        while time.time() < stop_at:
            sec0 = time.time()
            for _ in range(rate_per_val):
                if time.time() >= stop_at:
                    break
                # reserve among this val's accounts only
                with bank._lock:
                    acct = min(accts, key=lambda a: bank._seq[a])
                    s = bank._seq[acct]
                    bank._seq[acct] = s + 1
                dest = "acct40" if acct != "acct40" else "acct39"
                try:
                    d = sc.rpc(
                        h,
                        port,
                        "submit",
                        {"account": acct, "sequence": s, "destination": dest, "amount": 1, "fee": 10},
                        timeout=3.0,
                    )
                    code = (d.get("result") or {}).get("engine_result")
                    with lock:
                        if code == "tesSUCCESS":
                            counts["ok"] += 1
                            by_node[sc.val_id(vi)] += 1
                        elif code == "terBUSY":
                            counts["busy"] += 1
                            bank.rollback(acct, s)
                        else:
                            counts["err"] += 1
                            if code in ("tecSEQ_PAST", "tefALREADY", "tefPAST_SEQ"):
                                bank.resync(acct)
                            else:
                                bank.rollback(acct, s)
                except Exception:
                    bank.rollback(acct, s)
                    with lock:
                        counts["err"] += 1
            left = 1.0 - (time.time() - sec0)
            if left > 0:
                time.sleep(left)

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(1, n + 1)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    time.sleep(2)
    after = sc.snap_vals(n)
    elapsed = time.time() - t0
    deltas = []
    for i in range(1, n + 1):
        b = before.get(sc.val_id(i), {}).get("exec")
        a = after.get(sc.val_id(i), {}).get("exec")
        if isinstance(b, int) and isinstance(a, int):
            deltas.append(a - b)
    min_d = min(deltas) if deltas else 0
    return {
        "label": "multi_client_local",
        "rate_per_val": rate_per_val,
        "hold_s": hold,
        "elapsed_s": round(elapsed, 2),
        "ok": counts["ok"],
        "busy": counts["busy"],
        "err": counts["err"],
        "submits_by_owner": by_node,
        "submit_tps": round(counts["ok"] / elapsed, 2) if elapsed else 0,
        "exec_delta_min": min_d,
        "exec_tps_min": round(min_d / elapsed, 2) if elapsed else 0,
        "health_after": sc.health(n, after),
        "spread_after": sc.health(n, after).get("exec_spread"),
        "lag_after": sc.health(n, after).get("lag_max"),
        "pass_tight": (
            counts["err"] == 0
            and (sc.health(n, after).get("lag_max") or 0) == 0
            and (sc.health(n, after).get("exec_spread") or 0) <= 30
            and min_d >= max(1, int(counts["ok"] * 0.35))
        ),
    }


def write_report(prof: dict, n3: dict):
    lines = [
        "# Continue report — profile N=6 + N=3 steady",
        "",
        f"OUT: `{OUT}`",
        f"LAN F1={F1} F2={F2}",
        "",
        "## A) N=6 multipack bottleneck profile",
        "",
        f"- load: {prof['load']}",
        f"- submits ok/busy/err: {prof['counts']}",
        f"- **exec_tps_min: {prof['exec_tps_min']}** (delta min {prof['exec_delta_min']})",
        f"- CPU mean by id: `{prof['cpu_mean_by_id']}`",
        f"- RSS total MiB f1/f2: {prof['rss_after']['f1_total_mi']} / {prof['rss_after']['f2_total_mi']}",
        f"- health after: `{prof['health_after']}`",
        "",
        "### Pipeline deltas (selected)",
        "",
    ]
    for nid in ("val1", "val2", "val3", "val4", "val5", "val6"):
        d = prof["pipeline_delta"].get(nid, {})
        lines.append(
            f"- **{nid}**: admits={d.get('local_admits_ok')} batches={d.get('batches_created')} "
            f"cert={d.get('certified')} try_cert={d.get('try_certify_attempts')}/{d.get('try_certify_ok')} "
            f"foreign_sig={d.get('foreign_sig_merged')} exec={d.get('executed_txs')}"
        )
    lines += ["", "### Hints", ""]
    for h in prof["interpretation_hints"]:
        lines.append(f"- {h}")
    if prof.get("perf_report_head"):
        lines += ["", "### perf head (hottest local proc)", "```", str(prof["perf_report_head"])[:1500], "```"]

    sc_ = n3["single_client"]
    mc = n3["multi_client"]
    sk = n3["soak"]
    lines += [
        "",
        "## B) N=3 multipack steady / multi-client",
        "",
        "| mode | submit_tps | exec_tps_min | spread | lag | tight/pass |",
        "|------|------------|--------------|--------|-----|------------|",
        f"| single owner-router client @40 | {sc_['submit_tps']} | {sc_['exec_tps_min']} | {sc_['spread_after']} | {sc_['lag_after']} | {sc_['pass_tight']} |",
        f"| multi-client local (15/val) | {mc['submit_tps']} | {mc['exec_tps_min']} | {mc['spread_after']} | {mc['lag_after']} | {mc['pass_tight']} |",
        f"| soak 60s @20 | {sk['submit_tps']} | {sk['exec_tps_min']} | {sk['spread_after']} | {sk['lag_after']} | {sk['pass_tight']} |",
        "",
        f"Final health: `{n3['final_health']}`",
        "",
        "## What this means next",
        "",
    ]
    hints = prof["interpretation_hints"]
    if any("CERT_BOUND" in h for h in hints):
        lines.append("- **Code focus: certificate completion path** (try_certify success rate, sig quorum).")
    if any("PACK_BOUND" in h for h in hints):
        lines.append("- **Code focus: pack/sign rate** vs admit (Falcon-512 pack loop).")
    if any("SIG_GOSSIP" in h for h in hints):
        lines.append("- **Code focus: foreign sig merge / P2P pull efficiency.**")
    lines += [
        "- Keep **ops default N=3 multipack** until Phase A curve turns upward.",
        "- Multi-client should be the default load model for user-growth tests.",
        "",
        "## Mesh left running",
        "- **N=3 multipack** intentionally left up after section B for continued use.",
        "",
    ]
    text = "\n".join(lines) + "\n"
    (OUT / "CONTINUE_REPORT.md").write_text(text)
    (ROOT / "CONTINUE_REPORT.md").write_text(text)
    print(text, flush=True)


def main():
    prof = profile_n6()
    n3 = multi_client_n3()
    write_report(prof, n3)
    print(f"Done. Reports in {OUT}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
