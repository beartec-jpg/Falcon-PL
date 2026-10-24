#!/usr/bin/env python3
"""
Final break-it ramp for Falcon PL.

After (or instead of) the 6h cycle, step:
  200 ↑hold → cool, 300 ↑hold → cool, … 500, then 600..1000.

Each step: high hold (--hold-s) then cool at --cool-rate for --cool-s.
Logs break point when tip freezes, mempool_full, or submit_err spikes.

OOM: nodes use max_mempool_txs (default 50k) → reject before RAM blow-up.

  python3 pl-final-ramp.py \\
    --keys ~/falcon-pl-testnet-falcon/keys \\
    --seq-file ~/falcon-pl-testnet-falcon/logs/endurance-6h/wallet_seqs.json \\
    --log-dir ~/falcon-pl-testnet-falcon/logs/final-ramp
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

# Peak hold rates: 200..500 then 600..1000
DEFAULT_RATES = [200, 300, 400, 500, 600, 700, 800, 900, 1000]


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def rpc_status(addr: str, timeout: float = 3.0) -> dict:
    host, port_s = addr.replace("http://", "").replace("https://", "").rsplit(":", 1)
    port = int(port_s)
    try:
        with socket.create_connection((host, port), timeout=timeout) as s:
            s.sendall(b'{"type":"status_req"}\n')
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
                "max_mp": st.get("max_mempool_txs"),
                "win": st.get("lottery_winner_next"),
                "err": st.get("_error"),
            }
        )
    return out


def tips_agree(nodes: list[dict]) -> bool:
    hs = {n["hash"] for n in nodes if n.get("hash")}
    return len(hs) <= 1


def main() -> int:
    ap = argparse.ArgumentParser(description="PL final break-it ramp 200→1000")
    ap.add_argument("--keys", required=True)
    ap.add_argument("--scheme", default="falcon-512")
    ap.add_argument(
        "--rpc",
        default="127.0.0.1:19101,127.0.0.1:19102,192.0.2.2:19103",
    )
    ap.add_argument("--primary", default="127.0.0.1:19101")
    ap.add_argument("--wallets", type=int, default=16)
    ap.add_argument("--workers", type=int, default=16, help="higher for 500+ tx/s")
    ap.add_argument("--hold-s", type=float, default=120.0, help="seconds at peak rate")
    ap.add_argument("--cool-s", type=float, default=60.0, help="cool between peaks")
    ap.add_argument("--cool-rate", type=float, default=25.0)
    ap.add_argument("--sample-s", type=float, default=10.0)
    ap.add_argument("--bin", default="")
    ap.add_argument(
        "--rates",
        default=",".join(str(r) for r in DEFAULT_RATES),
        help="comma rates to probe",
    )
    ap.add_argument(
        "--log-dir",
        default=str(Path.home() / "falcon-pl-testnet-falcon" / "logs" / "final-ramp"),
    )
    ap.add_argument("--seq-file", default="")
    ap.add_argument(
        "--stop-on-break",
        action="store_true",
        help="stop after first rate that fails keep-up criteria",
    )
    args = ap.parse_args()

    rates = [float(x.strip()) for x in args.rates.split(",") if x.strip()]
    rpcs = [a.strip() for a in args.rpc.split(",") if a.strip()]
    rs = Path(__file__).resolve().parents[3]
    load_bin = args.bin or str(rs / "target" / "release" / "falcon-pl-load-net")
    if not Path(load_bin).is_file():
        print(f"missing {load_bin}", file=sys.stderr)
        return 2

    log_dir = Path(args.log_dir)
    log_dir.mkdir(parents=True, exist_ok=True)
    seq_file = args.seq_file or str(log_dir / "wallet_seqs.json")
    # Prefer continuing endurance sequences if present
    end_seq = Path.home() / "falcon-pl-testnet-falcon" / "logs" / "endurance-6h" / "wallet_seqs.json"
    if not Path(seq_file).exists() and end_seq.exists() and not args.seq_file:
        seq_file = str(end_seq)

    csv_path = log_dir / "ramp.csv"
    phases_path = log_dir / "phases.jsonl"
    human_log = log_dir / "ramp.log"
    summary_path = log_dir / "summary.json"
    (log_dir / "ramp.pid").write_text(str(__import__("os").getpid()) + "\n")

    def log(msg: str) -> None:
        line = f"{utc_now()} {msg}"
        print(line, flush=True)
        with human_log.open("a") as f:
            f.write(line + "\n")

    t0 = time.time()
    log("=== PL FINAL BREAK-IT RAMP ===")
    log(f"rates={rates} hold={args.hold_s}s cool={args.cool_s}s@{args.cool_rate}tx/s")
    log(f"workers={args.workers} seq_file={seq_file}")
    log(f"OOM guard: node max_mempool_txs (expect reject/full before process OOM)")
    log(f"log_dir={log_dir}")

    csv_f = csv_path.open("w", newline="")
    writer = csv.DictWriter(
        csv_f,
        fieldnames=[
            "utc",
            "elapsed_s",
            "phase",
            "target_rate",
            "tip",
            "mempool",
            "mempool_full",
            "max_mempool_txs",
            "tip_agree",
            "tips",
            "mps",
            "submit_ok",
            "submit_err",
        ],
    )
    writer.writeheader()
    csv_f.flush()

    state = {"phase": "init", "rate": 0.0, "ok": 0, "err": 0}
    stop_mon = threading.Event()

    def monitor():
        while not stop_mon.is_set():
            nodes = multi_status(rpcs)
            primary = next(
                (n for n in nodes if n["addr"] == args.primary),
                nodes[0] if nodes else {},
            )
            row = {
                "utc": utc_now(),
                "elapsed_s": f"{time.time() - t0:.1f}",
                "phase": state["phase"],
                "target_rate": state["rate"],
                "tip": primary.get("tip"),
                "mempool": primary.get("mp"),
                "mempool_full": primary.get("full"),
                "max_mempool_txs": primary.get("max_mp"),
                "tip_agree": tips_agree(nodes),
                "tips": "|".join(f"{n['addr']}={n['tip']}" for n in nodes),
                "mps": "|".join(f"{n['addr']}={n['mp']}" for n in nodes),
                "submit_ok": state["ok"],
                "submit_err": state["err"],
            }
            writer.writerow(row)
            csv_f.flush()
            log(
                f"sample phase={state['phase']} rate={state['rate']:.0f} "
                f"tip={primary.get('tip')} mp={primary.get('mp')} "
                f"full={primary.get('full')} agree={row['tip_agree']} "
                f"ok={state['ok']} err={state['err']}"
            )
            stop_mon.wait(args.sample_s)

    mon = threading.Thread(target=monitor, daemon=True)
    mon.start()

    results = []
    broke_at = None

    def run_load(rate: float, seconds: float, label: str) -> dict:
        state["phase"] = label
        state["rate"] = rate
        state["ok"] = 0
        state["err"] = 0
        # scale workers a bit with rate
        workers = max(args.workers, min(32, int(rate / 25) + 4))
        tip0 = rpc_status(args.primary).get("tip_height") or 0
        mp0 = rpc_status(args.primary).get("mempool") or 0
        cmd = [
            load_bin,
            "--keys",
            str(args.keys),
            "--scheme",
            args.scheme,
            "--rpc",
            args.primary,
            "--wallets",
            str(args.wallets),
            "--seconds",
            f"{seconds:.2f}",
            "--workers",
            str(workers),
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
        log(f"LOAD start {label} rate={rate:.0f} s={seconds:.0f} workers={workers}")
        proc = subprocess.run(cmd, capture_output=True, text=True)
        ok = err = 0
        for line in (proc.stdout or "").splitlines():
            if line.startswith("submit_ok="):
                parts = dict(p.split("=", 1) for p in line.split() if "=" in p)
                try:
                    ok = int(parts.get("submit_ok", "0"))
                    err = int(parts.get("submit_err", "0"))
                except ValueError:
                    pass
            if "seq-file" in line.lower() or "mempool full" in line.lower():
                log(f"  load: {line.strip()}")
        state["ok"] = ok
        state["err"] = err
        # short settle samples
        time.sleep(min(15.0, args.cool_s / 2 if args.cool_s > 0 else 15.0))
        st = rpc_status(args.primary)
        nodes = multi_status(rpcs)
        tip1 = int(st.get("tip_height") or 0)
        mp1 = int(st.get("mempool") or 0)
        full = bool(st.get("mempool_full"))
        delta_tip = tip1 - int(tip0 or 0)
        # keep-up heuristic: tip moved, or mempool not exploding vs submit
        keep_up = delta_tip > 0 and mp1 < 40_000 and not full
        # soft fail if many submit errors relative to ok
        err_ratio = err / max(ok + err, 1)
        broken = (err_ratio > 0.25 and err > 50) or (mp1 >= 45_000) or (
            delta_tip == 0 and ok > rate * seconds * 0.3
        )
        rec = {
            "utc": utc_now(),
            "label": label,
            "rate": rate,
            "seconds": seconds,
            "submit_ok": ok,
            "submit_err": err,
            "err_ratio": err_ratio,
            "tip0": tip0,
            "tip1": tip1,
            "delta_tip": delta_tip,
            "mp0": mp0,
            "mp1": mp1,
            "mempool_full": full,
            "tip_agree": tips_agree(nodes),
            "keep_up": keep_up,
            "broken": broken,
            "rc": proc.returncode,
        }
        with phases_path.open("a") as pf:
            pf.write(json.dumps(rec) + "\n")
        log(
            f"LOAD end   {label} rate={rate:.0f} ok={ok} err={err} "
            f"tip {tip0}→{tip1} (Δ{delta_tip}) mp={mp1} full={full} "
            f"keep_up={keep_up} broken={broken}"
        )
        if proc.returncode != 0:
            log(f"  load-net rc={proc.returncode} {(proc.stderr or '')[-300:]}")
        return rec

    try:
        for rate in rates:
            # UP / hold at peak
            rec = run_load(rate, args.hold_s, f"peak_{int(rate)}")
            results.append(rec)
            if rec["broken"]:
                broke_at = rate
                log(f"BREAK detected at rate={rate:.0f} tx/s")
                if args.stop_on_break:
                    break
            # DOWN / cool
            if args.cool_s > 0:
                cool = run_load(args.cool_rate, args.cool_s, f"cool_after_{int(rate)}")
                results.append(cool)
    except KeyboardInterrupt:
        log("interrupted")
    finally:
        stop_mon.set()
        mon.join(timeout=5)
        csv_f.close()
        summary = {
            "started": utc_now(),
            "elapsed_s": time.time() - t0,
            "rates": rates,
            "broke_at": broke_at,
            "results": results,
            "final_nodes": multi_status(rpcs),
            "note": "OOM protected by max_mempool_txs reject; broken=keep-up/err/full heuristics",
        }
        summary_path.write_text(json.dumps(summary, indent=2) + "\n")
        log(
            f"=== RAMP DONE elapsed={summary['elapsed_s']/60:.1f}m "
            f"broke_at={broke_at} steps={len(results)}"
        )
        log(f"summary: {summary_path}")
        # compact table
        log("rate | ok | err | Δtip | mp_end | full | broken")
        for r in results:
            if not str(r["label"]).startswith("peak_"):
                continue
            log(
                f"{r['rate']:>5.0f} | {r['submit_ok']:>6} | {r['submit_err']:>5} | "
                f"{r['delta_tip']:>5} | {r['mp1']:>6} | {r['mempool_full']} | {r['broken']}"
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
