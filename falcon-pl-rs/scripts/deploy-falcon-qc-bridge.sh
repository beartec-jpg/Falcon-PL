#!/usr/bin/env bash
# Deploy Groth16Verifier + FalconQcBridge on Sepolia from falcon-qc-one vk.sol.json.
# Run ON host1 after: FPL_PTAU=... bash scripts/falcon-qc-setup-one-ptau.sh
# (Do not --test-seed for protocol-controlled keys.)
set -euo pipefail

PL_ROOT="${PL_ROOT:-/home/falcon/falcon-pl-public-testnet-2300}"
REPO="${REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
SNARK_DIR="${SNARK_DIR:-$PL_ROOT/run/snark/falcon-qc-one}"
SCRIPTS="$REPO/crates/fd-pl/scripts"
ETH_RPC="${ETH_RPC:-https://ethereum-sepolia-rpc.publicnode.com}"
PK_FILE="${ETH_PK_FILE:-$HOME/falcon-pl-vault-test/eth-wallets/deployer.privhex}"
DEPLOY_JSON="${DEPLOY_JSON:-$PL_ROOT/run/snark/falcon-qc-bridge-deploy.json}"

export PATH="${HOME}/.foundry/bin:${PATH}"

if [[ ! -f "$SNARK_DIR/vk.sol.json" ]]; then
  echo "missing $SNARK_DIR/vk.sol.json — run falcon-qc-setup-one-ptau.sh first" >&2
  exit 1
fi
if [[ "${FPL_PRODUCTION:-0}" == "1" && "${FPL_ALLOW_TEST_SEED:-0}" != "1" && ! -f "$SNARK_DIR/ptau.json" ]]; then
  echo "FATAL: FPL_PRODUCTION=1 refuses a Groth16 vk without ptau.json (test-seed). Run falcon-qc-setup-one-ptau.sh" >&2
  exit 2
fi
if [[ ! -f "$PK_FILE" ]]; then
  echo "missing deployer key $PK_FILE" >&2
  exit 1
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$SCRIPTS/Groth16Verifier.sol" "$SCRIPTS/FalconQcBridge.sol" "$WORK/"
cd "$WORK"
forge init --force --no-git --quiet .
cat >> foundry.toml <<'TOML'

via_ir = true
optimizer = true
optimizer_runs = 200
TOML

export ETH_RPC SNARK_DIR WORK
export PK_HEX
PK_HEX=$(tr -d '\n' < "$PK_FILE" | sed 's/^0x//')
export USDC="${SEPOLIA_USDC:-0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238}"
export ETH_LOCK="${ETH_LOCK_ID:-0xc1113d2059c5d73f45968910fadfff7d88a3c4a4d8a0f754f15faa495d2c467b}"
export USDC_LOCK="${USDC_LOCK_ID:-0xc8927e1f8b56b080b015ccf240c5d27c6f1525d52a6545a1c03c84326e483c1c}"
export GENESIS="${FPL_GENESIS_HASH:-0x219fb7ab81c2207fc61fa5b21c6acb38214ff61e799f201ba98a79bce785f8d4}"
export KEY_ROOT="${FALCON_KEY_ROOT:-0x04a9ad1908569884ff307f63291c592af16fe345fb471c22104d0c0717bcfd8a}"
export NETWORK_ID="${FPL_NETWORK_ID:-2300}"
if [[ -z "${FPL_BRIDGE_START_HEIGHT:-}" ]]; then
  FPL_BRIDGE_START_HEIGHT=$("$PL_ROOT/bin/falcon-pl-ctl" --addr "${FPL_RPC:-127.0.0.1:19301}" status 2>/dev/null | python3 -c "
import sys, json
t = sys.stdin.read()
i = t.find('{')
b = json.loads(t[i:])
b = b.get('body') or b
print(int(b.get('tip_height') or 0))
" 2>/dev/null || echo "0")
  echo "auto FPL_BRIDGE_START_HEIGHT=$FPL_BRIDGE_START_HEIGHT (mesh tip)"
fi
export START_HEIGHT="${FPL_BRIDGE_START_HEIGHT:-224476}"
export N_PROOFS="${FALCON_QC_THRESHOLD:-4}"

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
VERIFIER=$(export PATH="$HOME/.foundry/bin:$PATH"; cast call "$DEPLOYER" "verifier()(address)" --rpc-url "$ETH_RPC")
echo "verifier_deployer=$DEPLOYER"
echo "verifier=$VERIFIER"

echo "==> deploy FalconQcBridge"
BRIDGE=$(VERIFIER="$VERIFIER" python3 <<'PY'
import os, subprocess, sys
rpc = os.environ["ETH_RPC"]
pk = os.environ["PK_HEX"]
work = os.environ["WORK"]
verifier = os.environ["VERIFIER"]
args = [
    os.environ["USDC"], os.environ["ETH_LOCK"], os.environ["USDC_LOCK"],
    os.environ["GENESIS"], verifier, os.environ["KEY_ROOT"],
    os.environ["N_PROOFS"], os.environ["NETWORK_ID"], os.environ["START_HEIGHT"],
]
cmd = ["forge", "create", "--rpc-url", rpc, "--private-key", pk, "--broadcast",
       "FalconQcBridge.sol:FalconQcBridge", "--constructor-args"] + args
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

mkdir -p "$(dirname "$DEPLOY_JSON")"
SNARK_DIR="$SNARK_DIR" python3 - "$DEPLOY_JSON" "$VERIFIER" "$BRIDGE" <<'PY'
import json, os, sys, time
path, verifier, bridge = sys.argv[1:4]
doc = {
    "deployed_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "network": "sepolia",
    "verifier": verifier,
    "bridge": bridge,
    "start_height": int(os.environ.get("START_HEIGHT", "0") or "0"),
    "vk_dir": os.environ.get("SNARK_DIR", ""),
    "note": "verifier vk matches falcon-qc-one pk.bin",
}
open(path, "w").write(json.dumps(doc, indent=2) + "\n")
print(json.dumps(doc, indent=2))
PY

if [[ -x "$PL_ROOT/scripts/pl-bridge-install-config.py" ]]; then
  PL_ROOT="$PL_ROOT" python3 "$PL_ROOT/scripts/pl-bridge-install-config.py"
elif [[ -x "$SCRIPTS/pl-bridge-install-config.py" ]]; then
  PL_ROOT="$PL_ROOT" python3 "$SCRIPTS/pl-bridge-install-config.py"
fi

cat <<EOF

Update runtime:
  FALCON_QC_BRIDGE=$BRIDGE
  SEPOLIA_VERIFIER=$VERIFIER

Fund ethPool/usdcPool on the new bridge, restart qc prover, re-run e2e.

EOF
