#!/usr/bin/env python3
"""Bridge operator health — one glance at production readiness."""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
if str(ROOT / "scripts") not in sys.path:
    sys.path.insert(0, str(ROOT / "scripts"))

from eth_bridge_config import (  # noqa: E402
    FALCON_QC_BRIDGE,
    GROTH16_VERIFIER,
    bridge_config_summary,
    parse_cast_uint,
)

CTL = ROOT / "bin/falcon-pl-ctl"
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
RPC = os.environ.get("ETH_RPC", "https://ethereum-sepolia-rpc.publicnode.com")
FPL_RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
SAMPLE_GAP = int(os.environ.get("FALCON_QC_SAMPLE_GAP", "48"))
V2 = os.environ.get("FALCON_QC_V2", "").lower() in ("1", "true", "yes")
if V2:
    # One-host V2 is lagged unwrap. Do not HARD-fail lag>256 (trains operators to ignore P0).
    LAG_HARD = int(os.environ.get("FPL_HEALTH_LAG_HARD", "0") or "0")  # 0 = disabled
    LAG_SLA = int(os.environ.get("FPL_HEALTH_LAG_SLA", "0") or "0")
else:
    LAG_HARD = int(os.environ.get("FPL_HEALTH_LAG_HARD", "2000"))
    LAG_SLA = int(os.environ.get("FPL_HEALTH_LAG_SLA", str(SAMPLE_GAP * 2)))
SUBMIT_STALE_SEC = int(os.environ.get("FPL_HEALTH_SUBMIT_STALE_SEC", "2700"))
SUBMIT_STATE = ROOT / "run/snark/falcon-qc-prover/submit-state.json"


def last_submit_age() -> float | None:
    try:
        st = json.loads(SUBMIT_STATE.read_text())
        ts = float(st.get("last_attempt") or 0)
        if ts <= 0:
            return None
        return max(0.0, time.time() - ts)
    except (OSError, json.JSONDecodeError, ValueError, TypeError):
        return None


def cast_call(to: str, sig: str, *args: str) -> str:
    cmd = [str(CAST), "call", to, sig, *args, "--rpc-url", RPC]
    return subprocess.check_output(cmd, text=True, timeout=30).strip()


def mesh_tip() -> int:
    out = subprocess.check_output([str(CTL), "--addr", FPL_RPC, "status"], text=True, timeout=30)
    body = json.loads(out[out.find("{") :]).get("body") or {}
    return int(body.get("tip_height") or 0)


def lc_lag(st: dict) -> int:
    lc = st.get("lc") or {}
    return int(lc.get("tip_lag") or 0)


def main() -> int:
    cfg = bridge_config_summary()
    print(json.dumps({"config": cfg}, indent=2))
    issues: list[str] = []

    if not FALCON_QC_BRIDGE:
        issues.append("no bridge address")
        print("FAIL", *issues)
        return 2

    eth_tip = parse_cast_uint(cast_call(FALCON_QC_BRIDGE, "fplTip()(uint64)"))
    on_chain_vk = cast_call(FALCON_QC_BRIDGE, "verifier()(address)").split()[0]
    mesh = mesh_tip()
    lag = max(0, mesh - eth_tip)

    out = subprocess.check_output([str(CTL), "--addr", FPL_RPC, "status"], text=True, timeout=30)
    st = json.loads(out[out.find("{") :]).get("body") or {}
    tip_lag = lc_lag(st)
    btc_mode = str(st.get("btc_exit_mode") or "")

    rails = {str(r.get("asset")): r for r in (st.get("rails") or []) if isinstance(r, dict)}
    open_wd = sum(int(r.get("open_withdrawals") or 0) for r in rails.values())

    print(f"bridge={FALCON_QC_BRIDGE}")
    print(f"eth_fpl_tip={eth_tip} mesh={mesh} header_lag={lag} sla={LAG_SLA} hard={LAG_HARD}")
    print(f"verifier on_chain={on_chain_vk} config={GROTH16_VERIFIER}")
    print(f"lc_tip_lag={tip_lag} btc_exit_mode={btc_mode} open_evm_withdrawals={open_wd}")

    if GROTH16_VERIFIER and on_chain_vk.lower() != GROTH16_VERIFIER.lower():
        issues.append("verifier mismatch (config vs on-chain bridge)")
    age = last_submit_age()
    if age is not None:
        print(f"last_header_submit_age_s={int(age)}")
    if V2:
        on_root = ""
        try:
            on_root = cast_call(FALCON_QC_BRIDGE, "currentCommitteeRoot()(bytes32)").split()[0]
        except subprocess.CalledProcessError:
            on_root = ""
        mesh_next = str(st.get("next_committee_root") or "")
        print(f"v2 currentCommitteeRoot={on_root} mesh_next_committee_root={mesh_next}")
        if lag == 0 and on_root and mesh_next:
            a = on_root.lower().removeprefix("0x")
            b = mesh_next.lower().removeprefix("0x")
            if a != b:
                issues.append("P0 committee-root mismatch (on-chain current vs mesh next at lag=0)")
        print(f"v2 lag_slope header_lag={lag} (HARD lag disabled; one-host = lagged unwrap)")
    elif lag > LAG_HARD:
        issues.append(f"header lag {lag} > hard {LAG_HARD}")
    elif lag > LAG_SLA and (age is None or age > SUBMIT_STALE_SEC):
        issues.append(
            f"header lag {lag} > sla {LAG_SLA} and last submit stale ({None if age is None else int(age)}s)"
        )
    if tip_lag > 3:
        issues.append(f"LC tip_lag {tip_lag} > 3")

    if V2:
        prover = subprocess.run(["pgrep", "-f", "pl-falcon-qc-v2-prover.py"], capture_output=True).returncode == 0
    else:
        prover = subprocess.run(["pgrep", "-f", "pl-falcon-qc-prover.py"], capture_output=True).returncode == 0
    relayer = subprocess.run(["pgrep", "-f", "pl-eth-lc-relayer.py"], capture_output=True).returncode == 0
    btc_hdr = subprocess.run(["pgrep", "-f", "pl-btc-header-submitter.py"], capture_output=True).returncode == 0
    print(f"qc_prover_running={prover} lc_relayer_running={relayer} btc_header_submitter={btc_hdr}")

    if not prover:
        issues.append("qc prover not running")
    if not relayer:
        issues.append("lc relayer not running")
    if btc_mode == "bitvm2" and not btc_hdr:
        issues.append("btc header submitter not running")

    if issues:
        print("NOT_READY:")
        for i in issues:
            print(f"  - {i}")
        return 1
    print("READY")
    return 0


if __name__ == "__main__":
    sys.exit(main())
