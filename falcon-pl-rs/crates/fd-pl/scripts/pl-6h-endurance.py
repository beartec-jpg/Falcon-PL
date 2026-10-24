#!/usr/bin/env python3
"""
6-hour (or custom) logged endurance load for Falcon PL testnet.

Pattern (repeats until --hours elapses):
  quiet steady → ramp → elevated steady → cool → quiet …
Default cycle ~40 min, market-shaped (ETH/XRP busy + short spikes).

Logs (under --log-dir):
  endurance.csv     continuous samples (tip, mempool, multi-node)
  phases.jsonl      one line per phase start/end
  endurance.log     human-readable stdout mirror
  summary.json      written on exit

  python3 pl-6h-endurance.py \\
    --keys ~/falcon-pl-testnet-falcon/keys \\
    --hours 6 \\
    --log-dir ~/falcon-pl-testnet-falcon/logs/endurance-6h
"""
from __future__ import annotations

import argparse
import csv
import json
import socket
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def rpc_status(addr: str, timeout: float = 3.0) -> dict:
    host, port_s = addr.replace("http://", "").replace("https://", "").rsplit(":", 1)
    port = int(port_s)
    data = b'{"type":"status_req"}\n'
    try:
        with socket.create_connection((host, port), timeout=timeout) as s:
            s.sendall(data)
            s.settimeout(timeout)
            buf = b""
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    if not line.strip():
                        continue
                    try:
                        obj = json.loads(line.decode())
                    except Exception:
                        continue
                    if obj.get("type") == "hello":
                        continue
                    return obj.get("body") or obj
    except Exception as e:
        return {"_error": str(e)}
    return {}


def multi_status(rpcs: list[str]) -> list[dict]:
    out = []
    for a in rpcs:
        st = rpc_status(a)
        out.append(
            {
                "addr": a,
                "tip": st.get("tip_height"),
                "hash": (str(st.get("tip_hash") or ""))[:20],
                "mp": st.get("mempool"),
                "full": st.get("mempool_full"),
                "win": st.get("lottery_winner_next"),
                "err": st.get("_error"),
            }
        )
    return out


def tips_agree(nodes: list[dict]) -> bool:
    hs = {n["hash"] for n in nodes if n.get("hash")}
    return len(hs) <= 1


# Cycle: (name, rate_tps, minutes)
DEFAULT_CYCLE = [
    ("quiet_steady", 20.0, 12.0),
    ("ramp_up", 50.0, 4.0),
    ("elevated_steady", 80.0, 12.0),
    ("spike", 150.0, 5.0),
    ("cool", 40.0, 4.0),
    ("quiet_steady_2", 25.0, 3.0),
]
# ~40 minutes per cycle → ~9 cycles in 6h


def main() -> int:
    ap = argparse.ArgumentParser(description="PL multi-hour endurance load")
    ap.add_argument("--keys", required=True)
    ap.add_argument("--scheme", default="falcon-512")
    ap.add_argument(
        "--rpc",
        default="127.0.0.1:19101,127.0.0.1:19102,192.0.2.2:19103",
    )
    ap.add_argument("--primary", default="127.0.0.1:19101")
    ap.add_argument("--hours", type=float, default=6.0)
    ap.add_argument("--wallets", type=int, default=16)
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--sample-s", type=float, default=15.0)
    ap.add_argument("--bin", default="")
    ap.add_argument(
        "--log-dir",
        default=str(Path.home() / "falcon-pl-testnet-falcon" / "logs" / "endurance-6h"),
    )
    ap.add_argument(
        "--cycle-scale",
        type=float,
        default=1.0,
        help="multiply each phase duration (1.0 = default ~40m cycle)",
    )
    ap.add_argument(
        "--seq-file",
        default="",
        help="wallet sequence checkpoint (default: <log-dir>/wallet_seqs.json)",
    )
    args = ap.parse_args()

    rpcs = [a.strip() for a in args.rpc.split(",") if a.strip()]
    rs = Path(__file__).resolve().parents[3]
    load_bin = args.bin or str(rs / "target" / "release" / "falcon-pl-load-net")
    if not Path(load_bin).is_file():
        print(f"missing {load_bin}", file=sys.stderr)
        return 2

    log_dir = Path(args.log_dir)
    log_dir.mkdir(parents=True, exist_ok=True)
    seq_file = args.seq_file or str(log_dir / "wallet_seqs.json")
    csv_path = log_dir / "endurance.csv"
    phases_path = log_dir / "phases.jsonl"
    human_log = log_dir / "endurance.log"
    summary_path = log_dir / "summary.json"
    pid_path = log_dir / "endurance.pid"
    pid_path.write_text(str(__import__("os").getpid()) + "\n")

    def log(msg: str) -> None:
        line = f"{utc_now()} {msg}"
        print(line, flush=True)
        with human_log.open("a") as f:
            f.write(line + "\n")

    cycle = [
        (n, r, m * args.cycle_scale) for n, r, m in DEFAULT_CYCLE
    ]
    cycle_min = sum(m for _, _, m in cycle)
    total_s = args.hours * 3600.0
    t0 = time.time()
    deadline = t0 + total_s

    log("=== PL 6h endurance start ===")
    log(f"scheme={args.scheme} hours={args.hours} cycle≈{cycle_min:.0f}m")
    log(f"rpc={args.rpc} wallets={args.wallets} bin={load_bin}")
    log(f"log_dir={log_dir}")
    log(f"seq_file={seq_file} (persists wallet sequences across chunks)")
    log(f"phases={[(n, r, m) for n, r, m in cycle]}")

    csv_f = csv_path.open("w", newline="")
    writer = csv.DictWriter(
        csv_f,
        fieldnames=[
            "utc",
            "elapsed_s",
            "cycle",
            "phase",
            "target_rate",
            "tip",
            "mempool",
            "mempool_full",
            "winner",
            "tip_agree",
            "tips",
            "mps",
            "submit_ok_phase",
            "submit_err_phase",
        ],
    )
    writer.writeheader()
    csv_f.flush()

    sample_lock = threading.Lock()
    sample_state = {
        "cycle": 0,
        "phase": "init",
        "rate": 0.0,
        "submit_ok": 0,
        "submit_err": 0,
    }
    stop_mon = threading.Event()

    def monitor():
        while not stop_mon.is_set():
            elapsed = time.time() - t0
            if elapsed >= total_s:
                break
            nodes = multi_status(rpcs)
            primary = next((n for n in nodes if n["addr"] == args.primary), nodes[0] if nodes else {})
            with sample_lock:
                st = dict(sample_state)
            row = {
                "utc": utc_now(),
                "elapsed_s": f"{elapsed:.1f}",
                "cycle": st["cycle"],
                "phase": st["phase"],
                "target_rate": st["rate"],
                "tip": primary.get("tip"),
                "mempool": primary.get("mp"),
                "mempool_full": primary.get("full"),
                "winner": primary.get("win"),
                "tip_agree": tips_agree(nodes),
                "tips": "|".join(f"{n['addr']}={n['tip']}" for n in nodes),
                "mps": "|".join(f"{n['addr']}={n['mp']}" for n in nodes),
                "submit_ok_phase": st["submit_ok"],
                "submit_err_phase": st["submit_err"],
            }
            writer.writerow(row)
            csv_f.flush()
            log(
                f"sample t={elapsed/60:.1f}m cycle={st['cycle']} phase={st['phase']} "
                f"rate={st['rate']:.0f} tip={primary.get('tip')} mp={primary.get('mp')} "
                f"full={primary.get('full')} agree={row['tip_agree']} "
                f"sub_ok={st['submit_ok']} sub_err={st['submit_err']}"
            )
            stop_mon.wait(args.sample_s)

    mon = threading.Thread(target=monitor, daemon=True)
    mon.start()

    phase_records = []
    cycle_n = 0
    total_submit_ok = 0
    total_submit_err = 0
    freeze_events = 0
    disagree_events = 0
    last_tip = None
    last_tip_t = time.time()

    try:
        while time.time() < deadline:
            cycle_n += 1
            for phase_name, rate, minutes in cycle:
                remaining = deadline - time.time()
                if remaining <= 1:
                    break
                hold_s = min(minutes * 60.0, remaining)
                # load-net in chunks so a long phase can still be interrupted by deadline
                chunk = min(hold_s, 120.0)  # 2-min load chunks
                phase_start = time.time()
                phase_ok = 0
                phase_err = 0
                with sample_lock:
                    sample_state.update(
                        {
                            "cycle": cycle_n,
                            "phase": phase_name,
                            "rate": rate,
                            "submit_ok": 0,
                            "submit_err": 0,
                        }
                    )
                ev = {
                    "utc": utc_now(),
                    "event": "phase_start",
                    "cycle": cycle_n,
                    "phase": phase_name,
                    "rate": rate,
                    "hold_s": hold_s,
                    "elapsed_s": phase_start - t0,
                }
                with phases_path.open("a") as pf:
                    pf.write(json.dumps(ev) + "\n")
                log(
                    f"PHASE start cycle={cycle_n} {phase_name} rate={rate:.0f} tx/s "
                    f"hold={hold_s/60:.1f}m remaining={remaining/3600:.2f}h"
                )

                left = hold_s
                while left > 1 and time.time() < deadline:
                    this_chunk = min(chunk, left, deadline - time.time())
                    if this_chunk < 1:
                        break
                    cmd = [
                        load_bin,
                        "--keys",
                        str(args.keys),
                        "--scheme",
                        args.scheme,
                        "--rpc",
                        args.primary,  # single RPC; gossip fills global mempool
                        "--wallets",
                        str(args.wallets),
                        "--seconds",
                        f"{this_chunk:.2f}",
                        "--workers",
                        str(args.workers),
                        "--rate",
                        str(rate),
                        "--drain-wait",
                        "0",
                        "--amount",
                        "1",
                        "--fee",
                        "1",
                        "--seq-file",
                        seq_file,
                    ]
                    proc = subprocess.run(cmd, capture_output=True, text=True)
                    for line in (proc.stdout or "").splitlines():
                        if line.startswith("submit_ok="):
                            parts = dict(p.split("=", 1) for p in line.split() if "=" in p)
                            try:
                                phase_ok += int(parts.get("submit_ok", "0"))
                                phase_err += int(parts.get("submit_err", "0"))
                            except ValueError:
                                pass
                        if "mempool full" in line.lower() or "submit_err" in line:
                            pass
                    if proc.returncode != 0:
                        log(f"  load-net rc={proc.returncode} stderr={(proc.stderr or '')[-200:]}")
                    with sample_lock:
                        sample_state["submit_ok"] = phase_ok
                        sample_state["submit_err"] = phase_err

                    # freeze / agree checks
                    nodes = multi_status(rpcs)
                    tip = nodes[0].get("tip") if nodes else None
                    now = time.time()
                    if tip is not None and last_tip is not None:
                        if tip == last_tip and rate >= 20 and (now - last_tip_t) > 90:
                            freeze_events += 1
                            log(f"  WARN possible tip freeze tip={tip} for >90s under rate={rate}")
                            last_tip_t = now  # re-arm
                        if tip != last_tip:
                            last_tip = tip
                            last_tip_t = now
                    elif tip is not None:
                        last_tip = tip
                        last_tip_t = now
                    if not tips_agree(nodes):
                        disagree_events += 1
                        log(f"  WARN tip disagree: {nodes}")

                    left = hold_s - (time.time() - phase_start)

                total_submit_ok += phase_ok
                total_submit_err += phase_err
                st = rpc_status(args.primary)
                pend = {
                    "utc": utc_now(),
                    "event": "phase_end",
                    "cycle": cycle_n,
                    "phase": phase_name,
                    "rate": rate,
                    "hold_s": hold_s,
                    "elapsed_s": time.time() - t0,
                    "submit_ok": phase_ok,
                    "submit_err": phase_err,
                    "tip": st.get("tip_height"),
                    "mempool": st.get("mempool"),
                    "mempool_full": st.get("mempool_full"),
                }
                phase_records.append(pend)
                with phases_path.open("a") as pf:
                    pf.write(json.dumps(pend) + "\n")
                log(
                    f"PHASE end   cycle={cycle_n} {phase_name} ok={phase_ok} err={phase_err} "
                    f"tip={st.get('tip_height')} mp={st.get('mempool')}"
                )

            if time.time() >= deadline:
                break

    except KeyboardInterrupt:
        log("interrupted by user")
    finally:
        stop_mon.set()
        mon.join(timeout=5)
        csv_f.close()

        final_nodes = multi_status(rpcs)
        summary = {
            "started": utc_now(),
            "elapsed_s": time.time() - t0,
            "hours_requested": args.hours,
            "cycles_completed": cycle_n,
            "total_submit_ok": total_submit_ok,
            "total_submit_err": total_submit_err,
            "freeze_warnings": freeze_events,
            "disagree_warnings": disagree_events,
            "final_nodes": final_nodes,
            "tip_agree": tips_agree(final_nodes),
            "phases": phase_records,
            "log_dir": str(log_dir),
        }
        summary_path.write_text(json.dumps(summary, indent=2) + "\n")
        log(
            f"=== DONE elapsed={summary['elapsed_s']/3600:.2f}h cycles={cycle_n} "
            f"submit_ok={total_submit_ok} err={total_submit_err} "
            f"freeze_warn={freeze_events} disagree_warn={disagree_events} "
            f"agree={summary['tip_agree']}"
        )
        log(f"summary: {summary_path}")
        log(f"csv: {csv_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
