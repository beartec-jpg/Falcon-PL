#!/usr/bin/env python3
"""Production bridge e2e supervisor — runs until both rails succeed.

Automatically waits for bridge readiness, retries failed e2es, and triggers
QC sig collection when header lag stalls.

Uses $PL_ROOT/run/bridge/falcon-pl-bridge.env.

  --eth-only   wait for header lag, then pl-eth-sepolia-e2e.py (retry loop)
  --btc-only   pl-bitvm2-e2e.py (retry loop)
  --all        both (default: BTC runs without waiting for ETH)
  --once       single attempt per rail (no retry loop)
"""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
ENV_FILE = ROOT / "run/bridge/falcon-pl-bridge.env"
LOG = ROOT / "logs/e2e-wait.log"
CTL = ROOT / "bin/falcon-pl-ctl"
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
V2 = os.environ.get("FALCON_QC_V2", "").lower() in ("1", "true", "yes")
CATCHUP = ROOT / (
    "scripts/pl-falcon-qc-v2-catchup.py" if V2 else "scripts/pl-falcon-qc-catchup.py"
)
if V2:
    raw_gap = os.environ.get("FALCON_QC_SAMPLE_GAP")
    if raw_gap is not None and int(raw_gap or "0") not in (0, 1):
        raise SystemExit("FALCON_QC_V2 forbids FALCON_QC_SAMPLE_GAP not in {0,1}")
    # Sequential headers: do not treat a SAMPLE_GAP window as "ready".
    MAX_LAG = int(os.environ.get("FPL_E2E_MAX_LAG", "0") or "0")
else:
    MAX_LAG = int(os.environ.get("FPL_E2E_MAX_LAG", str(int(os.environ.get("FALCON_QC_SAMPLE_GAP", "48")) * 8)))
POLL = int(os.environ.get("FPL_E2E_WAIT_POLL", "60"))
RETRY_SEC = int(os.environ.get("FPL_E2E_RETRY_SEC", "300"))
CATCHUP_STALL = int(os.environ.get("FPL_E2E_CATCHUP_STALL", "5"))


def load_env() -> dict[str, str]:
    out: dict[str, str] = {}
    if ENV_FILE.is_file():
        for line in ENV_FILE.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def log(msg: str) -> None:
    LOG.parent.mkdir(parents=True, exist_ok=True)
    with LOG.open("a") as f:
        f.write(f"{time.strftime('%Y-%m-%dT%H:%M:%S')} {msg}\n")
    print(msg, flush=True)


def mesh_tip(rpc: str) -> int:
    import json

    out = subprocess.check_output([str(CTL), "--addr", rpc, "status"], text=True, timeout=30)
    body = json.loads(out[out.find("{") :]).get("body") or {}
    return int(body.get("tip_height") or 0)


def eth_tip(bridge: str, rpc: str) -> int:
    out = subprocess.check_output(
        [str(CAST), "call", bridge, "fplTip()(uint64)", "--rpc-url", rpc],
        text=True,
        timeout=30,
    ).strip()
    return int(out.split()[0])


def trigger_catchup() -> None:
    if not CATCHUP.is_file():
        return
    log("trigger qc sig collect (lag stall)")
    subprocess.run(
        [sys.executable, str(CATCHUP), "collect"],
        cwd=str(ROOT),
        env={**os.environ, "PL_ROOT": str(ROOT)},
        timeout=600,
        check=False,
    )


def wait_eth_ready(bridge: str, rpc: str, fpl_rpc: str) -> None:
    log(f"wait eth bridge={bridge} max_lag={MAX_LAG} v2={V2}")
    stall = 0
    last_lag = -1
    while True:
        eth = eth_tip(bridge, rpc)
        mesh = mesh_tip(fpl_rpc)
        lag = max(0, mesh - eth)
        log(f"eth={eth} mesh={mesh} lag={lag}")
        if V2:
            # Headers moving is enough to start e2e; sequential wait is inside the e2e.
            if eth > 0:
                return
        elif lag <= MAX_LAG:
            return
        if lag == last_lag:
            stall += 1
        else:
            stall = 0
            last_lag = lag
        if stall >= CATCHUP_STALL:
            trigger_catchup()
            stall = 0
        time.sleep(POLL)


def run_script(name: str, ok_token: str, env: dict[str, str]) -> bool:
    run_env = os.environ.copy()
    run_env.update(env)
    run_env["FPL_PRODUCTION"] = "1"
    run_env["PL_ROOT"] = str(ROOT)
    log(f"starting {name}")
    r = subprocess.run([sys.executable, str(ROOT / "scripts" / name)], cwd=str(ROOT), env=run_env)
    if r.returncode == 0:
        log(ok_token)
        return True
    log(f"FAIL {name} exit={r.returncode}")
    return False


def run_until_ok(name: str, ok_token: str, env: dict[str, str], *, once: bool) -> bool:
    while True:
        if run_script(name, ok_token, env):
            return True
        if once:
            return False
        log(f"retry {name} in {RETRY_SEC}s")
        time.sleep(RETRY_SEC)
        trigger_catchup()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--eth-only", action="store_true")
    ap.add_argument("--btc-only", action="store_true")
    ap.add_argument("--all", action="store_true", help="run both; BTC does not wait for ETH")
    ap.add_argument("--once", action="store_true", help="single attempt per rail (no retry)")
    args = ap.parse_args()
    if not (args.eth_only or args.btc_only or args.all):
        args.all = True

    cfg = load_env()
    bridge = cfg.get("FALCON_QC_BRIDGE", "")
    rpc = cfg.get("ETH_RPC", "")
    fpl_rpc = cfg.get("FPL_RPC", "127.0.0.1:19301")
    if not cfg:
        log(f"missing {ENV_FILE}")
        return 2
    os.environ.update(cfg)

    ok = True
    if args.btc_only:
        ok = run_until_ok("pl-bitvm2-e2e.py", "BITVM2_E2E_OK", cfg, once=args.once) and ok
    elif args.eth_only:
        if V2:
            cfg = {**cfg, "FALCON_QC_V2": "1"}
            os.environ["FALCON_QC_V2"] = "1"
        wait_eth_ready(bridge, rpc, fpl_rpc)
        ok = run_until_ok("pl-eth-sepolia-e2e.py", "ETH_USDC_SEPOLIA_E2E_OK", cfg, once=args.once) and ok
    else:
        import threading

        btc_result: list[bool] = []

        def btc_job() -> None:
            btc_result.append(
                run_until_ok("pl-bitvm2-e2e.py", "BITVM2_E2E_OK", cfg, once=args.once)
            )

        t = threading.Thread(target=btc_job, daemon=True)
        t.start()
        wait_eth_ready(bridge, rpc, fpl_rpc)
        ok = run_until_ok("pl-eth-sepolia-e2e.py", "ETH_USDC_SEPOLIA_E2E_OK", cfg, once=args.once) and ok
        t.join()
        ok = (btc_result[0] if btc_result else False) and ok

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
