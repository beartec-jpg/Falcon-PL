#!/usr/bin/env python3
"""
Ladder / step load test for Falcon PL testnet.

Ramps steady submit rate while sampling tip + mempool so you can see
when drain keeps up vs when backlog starts building.

Default steps (tx/s):  25 → 50 → 25 → 75 → 50 → 100 → 50
Each step: hold for --step-seconds, sample every --sample-s.

  python3 pl-ladder-load.py \\
    --rpc 127.0.0.1:19101,127.0.0.1:19102,192.0.2.2:19103 \\
    --keys ~/falcon-pl-testnet-falcon/keys \\
    --scheme falcon-512 \\
    --bin ../target/release/falcon-pl-load-net

Uses falcon-pl-load-net for real Falcon (or hmac) signing.
"""
from __future__ import annotations

import argparse
import json
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path


def rpc_status(addr: str, timeout: float = 3.0) -> dict:
    host, port_s = addr.replace("http://", "").replace("https://", "").rsplit(":", 1)
    port = int(port_s)
    data = b'{"type":"status_req"}\n'
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
    return {}


def main() -> int:
    ap = argparse.ArgumentParser(description="PL ladder/step load test")
    ap.add_argument(
        "--rpc",
        default="127.0.0.1:19101,127.0.0.1:19102,192.0.2.2:19103",
    )
    ap.add_argument("--keys", required=True)
    ap.add_argument("--scheme", default="falcon-512")
    ap.add_argument(
        "--bin",
        default="",
        help="path to falcon-pl-load-net (default: sibling target/release)",
    )
    ap.add_argument("--wallets", type=int, default=16)
    ap.add_argument(
        "--steps",
        default="25,50,25,75,50,100,50",
        help="comma-separated rates tx/s",
    )
    ap.add_argument("--step-seconds", type=float, default=20.0)
    ap.add_argument("--sample-s", type=float, default=2.0)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument(
        "--cooldown",
        type=float,
        default=8.0,
        help="seconds between steps to observe drain",
    )
    ap.add_argument("--primary", default="127.0.0.1:19101")
    args = ap.parse_args()

    rates = [float(x.strip()) for x in args.steps.split(",") if x.strip()]
    rs = Path(__file__).resolve().parents[3]  # falcon-dag-rs
    load_bin = args.bin or str(rs / "target" / "release" / "falcon-pl-load-net")
    if not Path(load_bin).is_file():
        print(f"missing {load_bin} — cargo build -p fd-pl --release", file=sys.stderr)
        return 2

    print("=== PL ladder / step load ===")
    print(f"scheme={args.scheme} steps={rates} hold={args.step_seconds}s cooldown={args.cooldown}s")
    print(f"rpc={args.rpc} wallets={args.wallets}")
    print()

    rows = []  # summary per step
    t_global = time.time()

    for si, rate in enumerate(rates):
        st0 = rpc_status(args.primary)
        tip0 = int(st0.get("tip_height") or 0)
        mp0 = int(st0.get("mempool") or 0)
        print(f"--- step {si+1}/{len(rates)}: {rate:.0f} tx/s for {args.step_seconds:.0f}s ---")
        print(f"    start tip={tip0} mempool={mp0}")

        samples = []
        stop_mon = threading.Event()

        def monitor():
            while not stop_mon.is_set():
                st = rpc_status(args.primary)
                tip = int(st.get("tip_height") or 0)
                mp = int(st.get("mempool") or 0)
                soft = st.get("tip_soft")
                hard = st.get("tip_hard")
                t = time.time() - t_global
                samples.append((t, tip, mp, soft, hard))
                print(
                    f"    t={t:6.1f}s  tip={tip:4d}  mempool={mp:6d}  "
                    f"Δtip={tip - tip0:3d}  soft={soft} hard={hard}",
                    flush=True,
                )
                stop_mon.wait(args.sample_s)

        mon = threading.Thread(target=monitor, daemon=True)
        mon.start()

        cmd = [
            load_bin,
            "--keys",
            str(args.keys),
            "--scheme",
            args.scheme,
            "--rpc",
            args.rpc,
            "--wallets",
            str(args.wallets),
            "--seconds",
            str(args.step_seconds),
            "--workers",
            str(args.workers),
            "--rate",
            str(rate),
            "--drain-wait",
            "0",  # we handle cooldown ourselves
            "--amount",
            "1",
            "--fee",
            "1",
        ]
        t0 = time.time()
        proc = subprocess.run(cmd, capture_output=True, text=True)
        load_elapsed = time.time() - t0
        stop_mon.set()
        mon.join(timeout=2)

        # parse submit line from load-net stdout
        submit_ok = None
        submit_tps = None
        for line in (proc.stdout or "").splitlines():
            if line.startswith("submit_ok="):
                # submit_ok=801 submit_err=0 elapsed_s=20.01 submit_tps=40.0
                parts = dict(
                    p.split("=", 1) for p in line.split() if "=" in p
                )
                try:
                    submit_ok = int(parts.get("submit_ok", "0"))
                    submit_tps = float(parts.get("submit_tps", "0"))
                except ValueError:
                    pass

        st1 = rpc_status(args.primary)
        tip1 = int(st1.get("tip_height") or 0)
        mp1 = int(st1.get("mempool") or 0)

        # cooldown: watch drain
        print(f"    cooldown {args.cooldown:.0f}s (watch drain)…")
        cool_samples = []
        cool_end = time.time() + args.cooldown
        while time.time() < cool_end:
            st = rpc_status(args.primary)
            tip = int(st.get("tip_height") or 0)
            mp = int(st.get("mempool") or 0)
            cool_samples.append((time.time() - t_global, tip, mp))
            print(
                f"    cool t={time.time()-t_global:6.1f}s  tip={tip:4d}  mempool={mp:6d}",
                flush=True,
            )
            time.sleep(min(args.sample_s, max(0.5, cool_end - time.time())))

        stc = rpc_status(args.primary)
        tipc = int(stc.get("tip_height") or 0)
        mpc = int(stc.get("mempool") or 0)

        # classify: building if mempool rose during step and didn't clear in cooldown
        building = mp1 > mp0 + 20 and mpc > 50
        draining = mpc < mp1 * 0.5 or mpc < 30
        stable = abs(mp1 - mp0) <= 30 and mpc <= max(mp1, 30)

        if building and not draining:
            phase = "BUILDUP"
        elif draining and mp1 > mp0 + 10:
            phase = "DRAINING"
        elif mpc <= 20 and tipc > tip0:
            phase = "KEEPING_UP"
        else:
            phase = "MIXED"

        row = {
            "step": si + 1,
            "rate": rate,
            "submit_ok": submit_ok,
            "submit_tps": submit_tps,
            "tip0": tip0,
            "tip_end_step": tip1,
            "tip_end_cool": tipc,
            "mp0": mp0,
            "mp_end_step": mp1,
            "mp_end_cool": mpc,
            "delta_tip_step": tip1 - tip0,
            "delta_tip_cool": tipc - tip1,
            "phase": phase,
            "load_rc": proc.returncode,
        }
        rows.append(row)
        print(
            f"    → phase={phase} tip {tip0}→{tip1}→{tipc}  "
            f"mempool {mp0}→{mp1}→{mpc}  submit_tps={submit_tps}"
        )
        if proc.returncode != 0:
            print((proc.stderr or "")[-500:], file=sys.stderr)
        print()

    # summary table
    print("=== LADDER SUMMARY ===")
    print(
        f"{'step':>4} {'rate':>6} {'sub_tps':>8} {'tipΔ':>6} {'mp_end':>8} "
        f"{'mp_cool':>8} {'coolΔtip':>8} {'phase':<12}"
    )
    first_buildup = None
    for r in rows:
        print(
            f"{r['step']:>4} {r['rate']:>6.0f} "
            f"{(r['submit_tps'] or 0):>8.1f} "
            f"{r['delta_tip_step']:>6} {r['mp_end_step']:>8} "
            f"{r['mp_end_cool']:>8} {r['delta_tip_cool']:>8} "
            f"{r['phase']:<12}"
        )
        if r["phase"] == "BUILDUP" and first_buildup is None:
            first_buildup = r["rate"]

    print()
    # multi-node agreement
    tips = []
    for addr in args.rpc.split(","):
        addr = addr.strip()
        if not addr:
            continue
        st = rpc_status(addr)
        tips.append(
            (
                addr,
                st.get("tip_height"),
                str(st.get("tip_hash") or "")[:16],
                st.get("mempool"),
            )
        )
    print("final multi-node:")
    for t in tips:
        print(f"  {t[0]}: tip={t[1]} hash={t[2]} mp={t[3]}")
    hashes = {t[2] for t in tips if t[2]}
    print("tip_agree:", "YES" if len(hashes) <= 1 else "NO")

    if first_buildup is not None:
        print(
            f"\nBacklog BUILDUP first observed at ~{first_buildup:.0f} tx/s "
            "(mempool rose during step and did not clear in cooldown)."
        )
    else:
        print(
            "\nNo sustained BUILDUP phase in this ladder — drain kept up "
            "within cooldown for all tested rates."
        )

    # write JSON report
    out = Path("/tmp/pl-ladder-report.json")
    out.write_text(json.dumps({"steps": rows, "final_tips": tips}, indent=2) + "\n")
    print(f"report: {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
