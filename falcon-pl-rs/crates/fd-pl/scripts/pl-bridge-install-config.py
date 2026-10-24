#!/usr/bin/env python3
"""Install canonical Sepolia bridge config into $PL_ROOT/run/bridge/.

V3: FALCON_QC_V3_BRIDGE and FALCON_QC_V3_ANCHOR_HEIGHT are written empty (placeholder) unless
run/bridge/sepolia-qc-v3.json holds the deployed FalconQcBridgeV3 address and its anchor height
(the V2 fplTip the V3 constructor copied).

Writes:
  run/bridge/sepolia-qc.json      — machine-readable config (all operators read this)
  run/bridge/falcon-pl-bridge.env — systemd EnvironmentFile for every bridge unit

Run from deploy-eth-sepolia.sh or manually after deploy-falcon-qc-bridge.sh.
"""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PL_ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
PL_CONFIG = PL_ROOT / "config" / "sepolia-qc.json"
REPO_CONFIG = ROOT.parent / "config" / "sepolia-qc.json"
BRIDGE_DIR = PL_ROOT / "run/bridge"
JSON_OUT = BRIDGE_DIR / "sepolia-qc.json"
ENV_OUT = BRIDGE_DIR / "falcon-pl-bridge.env"
DEPLOY_JSON = PL_ROOT / "run/snark/falcon-qc-bridge-deploy.json"


def load_template() -> dict:
    for path in (JSON_OUT, DEPLOY_JSON, PL_CONFIG, REPO_CONFIG):
        if path.is_file():
            return json.loads(path.read_text())
    raise SystemExit(
        f"no bridge template (need one of {JSON_OUT}, {PL_CONFIG}, {REPO_CONFIG})"
    )


def merge_deploy(doc: dict) -> dict:
    if DEPLOY_JSON.is_file():
        dep = json.loads(DEPLOY_JSON.read_text())
        if dep.get("bridge"):
            doc["bridge"] = dep["bridge"]
        if dep.get("verifier"):
            doc["verifier"] = dep["verifier"]
            doc["groth16_verifier"] = dep["verifier"]
        if dep.get("deployed_at"):
            doc["deployed_at"] = dep["deployed_at"]
        if dep.get("start_height"):
            doc["start_height"] = dep["start_height"]
    return doc


V3_PLACEHOLDER_NOTE = (
    "# FalconQcBridgeV3: PLACEHOLDER, unset until V3 is deployed on Sepolia. Filled only from\n"
    "# run/bridge/sepolia-qc-v3.json {\"bridge\": \"0x...\", \"anchor_height\": N} written after the\n"
    "# deploy; never by hand. anchor_height = V2 fplTip at the V3 deploy; walletd opens on V2 only\n"
    "# while V2 fplTip <= it.\n"
    "# Empty means walletd keeps notes past the V2 header at 409 \"waiting for bridge header\".\n"
    "# FPL_SEPOLIA_QC_V3_BRIDGE (the seat/consensus name) is deliberately not written here."
)


def load_v3_bridge() -> tuple[str, str]:
    """(V3 bridge address, anchor height) from run/bridge/sepolia-qc-v3.json, or ("", "") when
    that file is absent (the default: not deployed). A present record that is unreadable, empty,
    has no bridge, a malformed address, or no positive integer anchor_height stops the install:
    only a missing file means "not deployed"."""
    v3_path = BRIDGE_DIR / "sepolia-qc-v3.json"
    try:
        text = v3_path.read_text()
    except FileNotFoundError:
        return "", ""
    except OSError as e:  # a directory, permissions, etc.: present but unusable
        raise SystemExit(f"{v3_path}: unreadable ({e})")
    try:
        raw = json.loads(text)
    except json.JSONDecodeError as e:
        raise SystemExit(f"{v3_path}: unreadable ({e})")
    addr = str((raw.get("bridge") if isinstance(raw, dict) else "") or "").strip()
    if not addr:
        raise SystemExit(f"{v3_path}: present but has no bridge address; remove it or fix the deploy record")
    h = addr.lower().removeprefix("0x")
    if not addr.lower().startswith("0x") or len(h) != 40 or any(c not in "0123456789abcdef" for c in h):
        raise SystemExit(f"{v3_path}: bridge is not a 20-byte 0x address")
    if h == "0" * 40:
        raise SystemExit(f"{v3_path}: bridge is the zero address; fix the deploy record")
    anchor = raw.get("anchor_height")
    if isinstance(anchor, bool) or not isinstance(anchor, int) or anchor < 1:
        raise SystemExit(
            f"{v3_path}: anchor_height must be the positive V2 fplTip the V3 constructor copied"
        )
    return addr, str(anchor)


def write_env(doc: dict, v3: tuple[str, str] | None = None) -> None:
    fallbacks = doc.get("eth_rpc_fallbacks") or []
    if isinstance(fallbacks, list):
        fb = ",".join(str(u) for u in fallbacks if str(u).strip())
    else:
        fb = str(fallbacks)
    destlock = "0xdBF6855b00B78c047A729A21E13bfE5f4C991C05"
    v2_doc = {}
    v2_path = BRIDGE_DIR / "sepolia-qc-v2.json"
    if v2_path.is_file():
        try:
            raw = json.loads(v2_path.read_text())
            if isinstance(raw, dict):
                v2_doc = raw
        except (OSError, json.JSONDecodeError):
            v2_doc = {}
    v2_bridge = str(v2_doc.get("bridge") or "").strip()
    if v3 is None:
        v3 = load_v3_bridge()
    v3_bridge, v3_anchor = v3
    lines = [
        f"PL_ROOT={PL_ROOT}",
        f"FALCON_QC_BRIDGE={doc['bridge']}",
        f"SEPOLIA_BRIDGE={destlock}",
        f"FPL_SEPOLIA_QC_BRIDGE={doc['bridge']}",
        f"FALCON_DESTLOCK={destlock}",
        f"FALCON_QC_V2_BRIDGE={v2_bridge}",
        f"FPL_SEPOLIA_QC_V2_BRIDGE={v2_bridge}",
        V3_PLACEHOLDER_NOTE,
        f"FALCON_QC_V3_BRIDGE={v3_bridge}",
        f"FALCON_QC_V3_ANCHOR_HEIGHT={v3_anchor}",
        f"SEPOLIA_VERIFIER={doc.get('verifier', '')}",
        f"SEPOLIA_USDC={doc.get('usdc', '')}",
        f"FPL_NETWORK_ID={doc.get('network_id', '2300')}",
        f"ETH_RPC={doc.get('eth_rpc', '')}",
        f"BEACON_RPC={doc.get('beacon_rpc', '')}",
        f"ETH_RPC_FALLBACKS={fb}",
        f"FPL_CTL={PL_ROOT}/bin/falcon-pl-ctl",
        f"FPL_SNARK={PL_ROOT}/bin/falcon-pl-snark",
        f"FPL_RPC=127.0.0.1:19301",
        f"FPL_KEYS={PL_ROOT}/keys",
    ]
    ENV_OUT.write_text("\n".join(lines) + "\n")
    os.chmod(ENV_OUT, 0o644)


def main() -> int:
    doc = merge_deploy(load_template())
    if not doc.get("bridge"):
        print("bridge address missing in config", file=sys.stderr)
        return 2
    # Validate the V3 record before writing anything, so a bad record leaves both outputs
    # untouched instead of a new sepolia-qc.json next to a stale env file.
    v3 = load_v3_bridge()
    BRIDGE_DIR.mkdir(parents=True, exist_ok=True)
    JSON_OUT.write_text(json.dumps(doc, indent=2) + "\n")
    write_env(doc, v3)
    print(f"installed {JSON_OUT}")
    print(f"installed {ENV_OUT}")
    print(f"bridge={doc['bridge']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
