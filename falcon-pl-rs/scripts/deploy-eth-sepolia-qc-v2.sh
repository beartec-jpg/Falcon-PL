#!/usr/bin/env bash
# Deploy Groth16Verifier + FalconQcBridgeV2 on Sepolia from falcon-qc-v2-one vk.
# Requires v2 ceremony (ptau.json). Does not reuse v1 pk/vk.
# Constructor needs live v4 tip fields (inner_hash, claim_root, C(S+1) leaves).
set -euo pipefail

PL_ROOT="${PL_ROOT:-/home/falcon/falcon-pl-public-testnet-2300}"
REPO="${REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
SNARK_DIR="${SNARK_DIR:-$PL_ROOT/run/snark/falcon-qc-v2-one}"
SCRIPTS="$REPO/crates/fd-pl/scripts"
ETH_RPC="${ETH_RPC:-https://ethereum-sepolia-rpc.publicnode.com}"
PK_FILE="${ETH_PK_FILE:-$HOME/falcon-pl-vault-test/eth-wallets/deployer.privhex}"
DEPLOY_JSON="${DEPLOY_JSON:-$PL_ROOT/run/bridge/sepolia-qc-v2.json}"

export PATH="${HOME}/.foundry/bin:${PATH}"

if [[ "${FPL_PRODUCTION:-0}" != "1" ]]; then
  echo "FATAL: set FPL_PRODUCTION=1 after reviewing constructor args. Refusing broadcast." >&2
  exit 2
fi
if [[ ! -f "$SNARK_DIR/vk.sol.json" || ! -f "$SNARK_DIR/ptau.json" ]]; then
  echo "missing v2 ceremony at $SNARK_DIR — run falcon-qc-v2-setup-one-ptau.sh first" >&2
  exit 1
fi
if grep -q 'FplFalconQcCircuit<1>' "$SNARK_DIR/ptau.json" 2>/dev/null && ! grep -q FplFalconQcCircuitV2 "$SNARK_DIR/ptau.json"; then
  echo "FATAL: $SNARK_DIR/ptau.json is v1. Do not deploy FalconQcBridgeV2 with v1 vk." >&2
  exit 2
fi
if [[ ! -f "$PK_FILE" ]]; then
  echo "missing deployer key $PK_FILE" >&2
  exit 1
fi

python3 - "$PL_ROOT/bin/falcon-pl-ctl" "${FPL_RPC:-127.0.0.1:19301}" <<'PY' > /tmp/falcon-qc-v2-ctor.json
import json, subprocess, sys
ctl, addr = sys.argv[1], sys.argv[2]
raw = subprocess.check_output([ctl, "--addr", addr, "status"], text=True, timeout=30)
body = json.loads(raw[raw.find("{"):]).get("body") or {}
need = [
    "tip_height", "tip_hash", "inner_hash", "claim_root",
    "next_committee_root", "next_committee_leaves",
]
missing = [k for k in need if not body.get(k)]
if missing:
    raise SystemExit("v4 tip fields missing (qc_start_height not armed?): " + ",".join(missing))
leaves = body.get("next_committee_leaves") or []
if len(leaves) != 6:
    raise SystemExit(f"need 6 next_committee_leaves, got {len(leaves)}")
json.dump({
    "start_height": int(body["tip_height"]),
    "genesis_hash": body["tip_hash"],
    "inner_hash": body["inner_hash"],
    "claim_root": body["claim_root"],
    "committee_root": body["next_committee_root"],
    "leaves": leaves,
}, sys.stdout)
PY

export ETH_RPC SNARK_DIR
export PK_HEX
PK_HEX=$(tr -d '\n' < "$PK_FILE" | sed 's/^0x//')
export USDC="${SEPOLIA_USDC:-0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238}"
export ETH_LOCK="${ETH_LOCK_ID:-0xc1113d2059c5d73f45968910fadfff7d88a3c4a4d8a0f754f15faa495d2c467b}"
export USDC_LOCK="${USDC_LOCK_ID:-0xc8927e1f8b56b080b015ccf240c5d27c6f1525d52a6545a1c03c84326e483c1c}"
export NETWORK_ID="${FPL_NETWORK_ID:-2300}"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$SCRIPTS/FalconQcBridgeV2.sol" "$SCRIPTS/Groth16Verifier.sol" "$WORK/"
cd "$WORK"
forge init --force --no-git --quiet .
cat >> foundry.toml <<'TOML'
via_ir = true
optimizer = true
optimizer_runs = 200
TOML

echo "==> deploy Groth16Verifier from $SNARK_DIR/vk.sol.json"
python3 "$REPO/scripts/gen-verifier-deploy-sol.py" "$SNARK_DIR/vk.sol.json" "$WORK/Groth16VerifierDeploy.sol"
DEPLOYER=$(PK_HEX="$PK_HEX" ETH_RPC="$ETH_RPC" WORK="$WORK" python3 <<'PY'
import os, subprocess, sys
pk = os.environ["PK_HEX"]
rpc = os.environ["ETH_RPC"]
work = os.environ["WORK"]
cmd = ["forge", "create", "--rpc-url", rpc, "--private-key", pk, "--broadcast",
       "Groth16VerifierDeploy.sol:Groth16VerifierDeploy"]
r = subprocess.run(cmd, capture_output=True, text=True, cwd=work)
if r.returncode != 0:
    sys.stderr.write(r.stderr or r.stdout)
    sys.exit(r.returncode)
deployed = None
for line in (r.stdout + r.stderr).splitlines():
    if "Deployed to:" in line:
        deployed = line.split()[-1]
        break
if not deployed:
    sys.stderr.write(r.stdout + r.stderr)
    sys.exit(1)
print(deployed)
PY
)
VERIFIER=$(cast call "$DEPLOYER" "verifier()(address)" --rpc-url "$ETH_RPC")
echo "verifier_deployer=$DEPLOYER"
echo "verifier=$VERIFIER"

echo "==> deploy FalconQcBridgeV2"
BRIDGE=$(VERIFIER="$VERIFIER" PK_HEX="$PK_HEX" ETH_RPC="$ETH_RPC" WORK="$WORK" python3 <<'PY'
import json, os, subprocess, sys
rpc = os.environ["ETH_RPC"]
pk = os.environ["PK_HEX"]
work = os.environ["WORK"]
verifier = os.environ["VERIFIER"]
ctor = json.loads(open("/tmp/falcon-qc-v2-ctor.json").read())
def b32(h):
    h = h.strip().lower().removeprefix("0x")
    if len(h) != 64:
        raise SystemExit(f"bad bytes32 {h!r}")
    return "0x" + h
args = [
    os.environ["USDC"],
    os.environ["ETH_LOCK"],
    os.environ["USDC_LOCK"],
    b32(ctor["genesis_hash"]),
    verifier,
    os.environ["NETWORK_ID"],
    str(ctor["start_height"]),
    b32(ctor["inner_hash"]),
    b32(ctor["claim_root"]),
    b32(ctor["committee_root"]),
    "[" + ",".join(b32(x) for x in ctor["leaves"]) + "]",
]
cmd = ["forge", "create", "--rpc-url", rpc, "--private-key", pk, "--broadcast",
       "FalconQcBridgeV2.sol:FalconQcBridgeV2", "--constructor-args"] + args
r = subprocess.run(cmd, capture_output=True, text=True, cwd=work)
if r.returncode != 0:
    sys.stderr.write(r.stderr or r.stdout)
    sys.exit(r.returncode)
for line in (r.stdout + r.stderr).splitlines():
    if "Deployed to:" in line:
        print(line.split()[-1])
        sys.exit(0)
sys.stderr.write(r.stdout + r.stderr)
sys.exit(1)
PY
)
echo "bridge=$BRIDGE"

python3 - "$DEPLOY_JSON" "$BRIDGE" "$VERIFIER" <<'PY'
import json, sys
from pathlib import Path
out, bridge, verifier = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
ctor = json.loads(Path("/tmp/falcon-qc-v2-ctor.json").read_text())
doc = {
    "network": "sepolia",
    "network_id": "2300",
    "bridge": bridge,
    "verifier": verifier,
    "circuit": "FplFalconQcCircuitV2",
    "usdc": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
    "start_height": ctor["start_height"],
    "genesis_hash": ctor["genesis_hash"],
    "note": "FalconQcBridgeV2 sequential lottery-6. Do not advertise until headers move. v1 pool stranded.",
}
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(json.dumps(doc, indent=2) + "\n")
print("wrote", out)
PY

echo "next: FPL_SEPOLIA_QC_V2_BRIDGE=$BRIDGE  (mint watches V2 deposits; dest-lock still in the union)"
