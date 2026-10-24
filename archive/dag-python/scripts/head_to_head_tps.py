#!/usr/bin/env python3
"""Head-to-head TPS: Falcon DAG (Narwhal+Falcon-512) vs Falcon C++ RPCA (xrpld).

Measures local-machine throughput under controlled loads. Not a WAN/mainnet claim.

Usage:
  # Falcon DAG only (always works if falcon-python installed)
  python scripts/head_to_head_tps.py --dag-only

  # Full: needs Falcon regtest on 5005–5009 (start via qXRP scripts)
  python scripts/head_to_head_tps.py --duration 20 --txs 500

  # Start Falcon regtest first (from qXRP repo):
  #   ./scripts/start-regtest.sh /home/scott/qXRP/.build/xrpld /home/scott/qXRP/data/h2h-regtest &
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from falcon_dag.config import ProtocolConfig
from falcon_dag.crypto import falcon_available
from falcon_dag.sim import NetworkSim


def rpc(url: str, method: str, params: dict | None = None, timeout: float = 10.0) -> dict:
    body = json.dumps({"method": method, "params": [params or {}]}).encode()
    req = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def port_up(port: int) -> bool:
    try:
        rpc(f"http://127.0.0.1:{port}", "server_info" if port < 10000 else "ping")
        return True
    except Exception:
        try:
            rpc(f"http://127.0.0.1:{port}", "ping")
            return True
        except Exception:
            return False


# ---------------------------------------------------------------------------
# Falcon DAG measurements
# ---------------------------------------------------------------------------


def bench_dag_sim(n_txs: int, n_validators: int = 5, scheme: str = "falcon-512") -> dict:
    if scheme.startswith("falcon") and not falcon_available():
        scheme = "hmac-sha256-dev"
    cfg = ProtocolConfig(
        target_batch_interval_s=0.0,
        hard_finality_h=3,
        max_batch_txs=128,
        worker_sharding=True,
        shard_orphan_age_s=0.0,
        require_sig_verify=True,
        crypto_scheme=scheme,
    )
    t0 = time.perf_counter()
    sim = NetworkSim(n=n_validators, config=cfg)
    sim.flood(n_txs, n_accounts=min(20, max(5, n_validators * 2)))
    for _ in range(max(40, n_txs // 20 + 20)):
        if all(len(n.engine.mempool) == 0 for n in sim.nodes):
            for __ in range(3):
                sim.step(force=True)
                sim.deliver_all()
            break
        sim.step(force=True)
        sim.deliver_all()
    elapsed = time.perf_counter() - t0
    exec_n = len(sim.nodes[0].engine.execution.executed_txs)
    m = sim.nodes[0].engine.dag.metrics()
    return {
        "system": "falcon-dag",
        "mode": "in-process-sim",
        "crypto": sim.keyring.scheme,
        "validators": n_validators,
        "txs_requested": n_txs,
        "txs_executed": exec_n,
        "elapsed_s": round(elapsed, 4),
        "tps": round(exec_n / elapsed, 2) if elapsed else 0,
        "agree": sim.all_executed_match(),
        "certified": m.get("certified"),
        "avg_sig_bytes": m.get("avg_sig_bytes"),
        "est_cert_bytes_4sigs": m.get("est_cert_bytes_4sigs"),
        "notes": "Python prototype; not C++. Includes Falcon-512 cert sign/verify when scheme=falcon-512.",
    }


def bench_dag_live(n_txs: int = 200, base_port: int = 18001) -> dict:
    ports = [base_port + i for i in range(5)]
    if not port_up(ports[0]):
        return {"system": "falcon-dag", "mode": "live-regtest", "error": "not running on 18001"}

    info0 = rpc(f"http://127.0.0.1:{ports[0]}", "server_info")["result"]["info"]
    scheme = info0.get("metrics", {}).get("da", {}).get("crypto_scheme", "?")

    # Use accounts 1-10 with advancing sequences from current — use high offset accounts
    seq = {f"acct{i}": 1 for i in range(11, 21)}
    # Fast-forward sequences until submit succeeds once per account
    for a in list(seq.keys()):
        for s in range(1, 500):
            try:
                r = rpc(
                    f"http://127.0.0.1:{ports[0]}",
                    "submit",
                    {
                        "account": a,
                        "sequence": s,
                        "destination": "acct11",
                        "amount": 1,
                        "fee": 10,
                    },
                )
                if r.get("result", {}).get("engine_result") == "tesSUCCESS":
                    seq[a] = s + 1
                    break
            except Exception:
                break
            seq[a] = s + 1

    t0 = time.perf_counter()
    ok = 0
    for i in range(n_txs):
        src = f"acct{11 + (i % 10)}"
        dst = f"acct{11 + ((i + 1) % 10)}"
        s = seq[src]
        try:
            r = rpc(
                f"http://127.0.0.1:{ports[i % 5]}",
                "submit",
                {
                    "account": src,
                    "sequence": s,
                    "destination": dst,
                    "amount": 1,
                    "fee": 10,
                },
            )
            if r.get("result", {}).get("engine_result") == "tesSUCCESS":
                seq[src] = s + 1
                ok += 1
            else:
                seq[src] = s + 1
        except Exception:
            pass
    # Wait for certify+order
    deadline = time.time() + 15
    while time.time() < deadline:
        try:
            c = rpc(f"http://127.0.0.1:{ports[0]}", "canonical")["result"]
            if len(c.get("txs", [])) >= ok * 0.8:
                break
        except Exception:
            pass
        time.sleep(0.3)
    elapsed = time.perf_counter() - t0
    canons = []
    for p in ports:
        try:
            canons.append(len(rpc(f"http://127.0.0.1:{p}", "canonical")["result"].get("txs", [])))
        except Exception:
            canons.append(-1)
    info = rpc(f"http://127.0.0.1:{ports[0]}", "server_info")["result"]["info"]
    m = info.get("metrics", {})
    return {
        "system": "falcon-dag",
        "mode": "live-regtest-5node",
        "crypto": scheme,
        "submitted_ok": ok,
        "executed_reported": m.get("execution", {}).get("executed_txs"),
        "canonical_lens": canons,
        "elapsed_s": round(elapsed, 4),
        "tps_submit_to_settle": round(ok / elapsed, 2) if elapsed else 0,
        "architecture": info.get("architecture"),
        "notes": "5 Python processes, Falcon-512 certs, localhost.",
    }


# ---------------------------------------------------------------------------
# Falcon C++ RPCA (xrpld) measurements
# ---------------------------------------------------------------------------


def falcon_server_info(port: int = 5005) -> dict | None:
    try:
        return rpc(f"http://127.0.0.1:{port}", "server_info")["result"]["info"]
    except Exception:
        return None


def bench_falcon_cpp_ledger_rate(duration_s: float = 15.0, port: int = 5005) -> dict:
    """Measure validated ledger close rate + estimate from seq delta (no tx flood)."""
    info = falcon_server_info(port)
    if not info:
        return {
            "system": "falcon-rpca-cpp",
            "mode": "ledger-rate",
            "error": f"xrpld not reachable on {port}",
            "hint": "Start: cd /home/scott/qXRP && ./scripts/start-regtest.sh .build/xrpld data/h2h-regtest",
        }
    seq0 = info.get("validated_ledger", {}).get("seq") or info.get("validated_ledger", {}).get(
        "ledger_index"
    )
    if seq0 is None:
        # try complete_ledgers parse
        seq0 = 0
    t0 = time.perf_counter()
    time.sleep(duration_s)
    info2 = falcon_server_info(port) or {}
    seq1 = info2.get("validated_ledger", {}).get("seq") or info2.get(
        "validated_ledger", {}
    ).get("ledger_index", seq0)
    elapsed = time.perf_counter() - t0
    delta = int(seq1) - int(seq0) if seq1 and seq0 else 0
    ledgers_per_s = delta / elapsed if elapsed else 0
    # Rough capacity if target_txn_in_ledger ~1000 and ~3s close from config comments
    return {
        "system": "falcon-rpca-cpp",
        "mode": "ledger-close-rate",
        "server_state": info2.get("server_state"),
        "build_version": info2.get("build_version"),
        "pubkey_node": (info2.get("pubkey_node") or "")[:20],
        "seq_start": seq0,
        "seq_end": seq1,
        "ledgers_closed": delta,
        "elapsed_s": round(elapsed, 3),
        "ledgers_per_s": round(ledgers_per_s, 3),
        "est_seconds_per_ledger": round(1 / ledgers_per_s, 3) if ledgers_per_s else None,
        "notes": "Idle close rate only — not max TPS. Use load-test for flood.",
    }


def bench_falcon_cpp_loadtest(
    duration: int = 20,
    accounts: int = 15,
    qxrp_root: Path | None = None,
) -> dict:
    """Shell out to qXRP scripts/load-test.py if available and network is up."""
    if not falcon_server_info(5005):
        return {
            "system": "falcon-rpca-cpp",
            "mode": "load-test",
            "error": "xrpld not on :5005",
        }
    qxrp_root = qxrp_root or Path("/home/scott/qXRP")
    script = qxrp_root / "scripts" / "load-test.py"
    if not script.is_file():
        return {"system": "falcon-rpca-cpp", "mode": "load-test", "error": f"missing {script}"}

    cmd = [
        sys.executable,
        str(script),
        "--accounts",
        str(accounts),
        "--duration",
        str(duration),
        "--no-sysmon",
    ]
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(qxrp_root),
            capture_output=True,
            text=True,
            timeout=duration + 180,
        )
    except subprocess.TimeoutExpired as e:
        return {
            "system": "falcon-rpca-cpp",
            "mode": "load-test",
            "error": "timeout",
            "stdout_tail": (e.stdout or "")[-2000:] if isinstance(e.stdout, str) else "",
        }
    elapsed = time.perf_counter() - t0
    out = (proc.stdout or "") + "\n" + (proc.stderr or "")
    # Parse common patterns from load-test output
    parsed: dict = {
        "system": "falcon-rpca-cpp",
        "mode": "load-test",
        "returncode": proc.returncode,
        "elapsed_wall_s": round(elapsed, 3),
        "stdout_tail": out[-3000:],
    }
    for line in out.splitlines():
        low = line.lower()
        if "tps" in low or "throughput" in low or "tx/s" in low or "transactions" in low:
            if "parsed_lines" not in parsed:
                parsed["parsed_lines"] = []
            if len(parsed["parsed_lines"]) < 30:
                parsed["parsed_lines"].append(line.strip())
        # numeric tps =
        if "tps" in low and any(c.isdigit() for c in line):
            import re

            m = re.search(r"([0-9]+(?:\.[0-9]+)?)\s*(?:tps|tx/s)", low)
            if m:
                parsed["tps_matched"] = float(m.group(1))
    return parsed


def try_start_falcon_regtest(
    binary: str = "/home/scott/qXRP/.build/xrpld",
    runtime: str = "/home/scott/qXRP/data/h2h-regtest",
    wait_s: float = 90.0,
) -> dict:
    """Best-effort start of Falcon 5-node regtest (non-blocking wait for RPC)."""
    if falcon_server_info(5005):
        return {"started": False, "reason": "already_up"}
    qxrp = Path("/home/scott/qXRP")
    script = qxrp / "scripts" / "start-regtest.sh"
    if not Path(binary).is_file():
        return {"started": False, "error": f"binary missing: {binary}"}
    if not script.is_file():
        return {"started": False, "error": f"script missing: {script}"}
    # Launch in background
    log_path = Path(runtime) / "h2h-start.log"
    Path(runtime).mkdir(parents=True, exist_ok=True)
    with open(log_path, "w") as logf:
        proc = subprocess.Popen(
            ["bash", str(script), binary, runtime],
            cwd=str(qxrp),
            stdout=logf,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    t0 = time.time()
    while time.time() - t0 < wait_s:
        if falcon_server_info(5005):
            return {
                "started": True,
                "pid": proc.pid,
                "wait_s": round(time.time() - t0, 1),
                "log": str(log_path),
            }
        time.sleep(2)
    return {
        "started": False,
        "error": "timeout waiting for :5005",
        "pid": proc.pid,
        "log": str(log_path),
        "log_tail": log_path.read_text()[-2000:] if log_path.is_file() else "",
    }


def main() -> None:
    ap = argparse.ArgumentParser(description="Head-to-head TPS Falcon DAG vs Falcon RPCA")
    ap.add_argument("--txs", type=int, default=500, help="tx count for DAG sim / live")
    ap.add_argument("--duration", type=int, default=20, help="Falcon load-test duration s")
    ap.add_argument("--dag-only", action="store_true")
    ap.add_argument("--start-falcon", action="store_true", help="try start qXRP regtest")
    ap.add_argument(
        "--out",
        default=str(ROOT / "docs" / "HEAD_TO_HEAD_TPS.json"),
    )
    args = ap.parse_args()

    report: dict = {
        "title": "Falcon DAG vs Falcon C++ RPCA — local TPS head-to-head",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "caveats": [
            "Localhost only; not WAN.",
            "Falcon DAG is a Python research prototype.",
            "Falcon C++ is production-shaped xrpld with Falcon-512 tx sigs.",
            "Numbers are directional for architecture comparison, not marketing claims.",
        ],
        "results": [],
    }

    print("=== 1) Falcon DAG in-process (Falcon-512 certs) ===")
    r1 = bench_dag_sim(args.txs, scheme="falcon-512")
    report["results"].append(r1)
    print(json.dumps(r1, indent=2))

    print("\n=== 2) Falcon DAG in-process (HMAC baseline for crypto cost delta) ===")
    r1b = bench_dag_sim(min(args.txs, 500), scheme="hmac-sha256-dev")
    report["results"].append(r1b)
    print(json.dumps({k: r1b[k] for k in ("crypto", "tps", "txs_executed", "elapsed_s")}, indent=2))

    print("\n=== 3) Falcon DAG live regtest ===")
    r2 = bench_dag_live(min(args.txs, 200))
    report["results"].append(r2)
    print(json.dumps(r2, indent=2))

    if not args.dag_only:
        if args.start_falcon or not falcon_server_info(5005):
            print("\n=== 4a) Starting Falcon C++ regtest (best-effort) ===")
            st = try_start_falcon_regtest()
            report["falcon_start"] = st
            print(json.dumps(st, indent=2)[:2000])

        print("\n=== 4b) Falcon C++ idle ledger rate ===")
        r3 = bench_falcon_cpp_ledger_rate(duration_s=12.0)
        report["results"].append(r3)
        print(json.dumps(r3, indent=2))

        print("\n=== 4c) Falcon C++ load-test (if up) ===")
        r4 = bench_falcon_cpp_loadtest(duration=args.duration, accounts=15)
        report["results"].append(r4)
        # Print compact
        compact = {k: v for k, v in r4.items() if k != "stdout_tail"}
        print(json.dumps(compact, indent=2))
        if r4.get("stdout_tail"):
            print("--- load-test output tail ---")
            print(r4["stdout_tail"][-1500:])

    # Summary comparison table
    summary = {
        "dag_sim_falcon512_tps": r1.get("tps"),
        "dag_sim_hmac_tps": r1b.get("tps"),
        "dag_live_tps": r2.get("tps_submit_to_settle"),
        "falcon_cpp_ledger_s": None,
        "falcon_cpp_load_tps": None,
        "interpretation": [],
    }
    for r in report["results"]:
        if r.get("system") == "falcon-rpca-cpp" and r.get("est_seconds_per_ledger"):
            summary["falcon_cpp_ledger_s"] = r["est_seconds_per_ledger"]
        if r.get("system") == "falcon-rpca-cpp" and r.get("tps_matched") is not None:
            summary["falcon_cpp_load_tps"] = r["tps_matched"]

    summary["interpretation"].append(
        "DAG sim TPS is algorithmic upper bound in Python with real Falcon-512 cert crypto."
    )
    summary["interpretation"].append(
        "DAG live TPS reflects multi-process Python + HTTP; C++ Falcon will dominate raw TPS if both healthy."
    )
    summary["interpretation"].append(
        "Architecture win for DAG is cert amortization under large Falcon-512 sigs, not Python beating xrpld."
    )
    if summary["dag_sim_falcon512_tps"] and summary["dag_sim_hmac_tps"]:
        ratio = summary["dag_sim_hmac_tps"] / max(summary["dag_sim_falcon512_tps"], 0.01)
        summary["hmac_vs_falcon512_sim_speedup"] = round(ratio, 2)
        summary["interpretation"].append(
            f"HMAC-dev sim ~{ratio:.1f}× faster than Falcon-512 certs in this prototype (crypto cost)."
        )

    report["summary"] = summary
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n")
    print("\n=== SUMMARY ===")
    print(json.dumps(summary, indent=2))
    print(f"\nWrote {out}")


if __name__ == "__main__":
    main()
