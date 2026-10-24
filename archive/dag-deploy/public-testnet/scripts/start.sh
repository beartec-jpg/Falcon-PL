#!/usr/bin/env bash
# Start falcon-dag-testnet-1 (public play testnet R2).
# Dual-shard GMP stack + facade + faucet + watchdog.
#
#   export SSH_F2=100.64.0.3
#   bash deploy/public-testnet/scripts/start.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
GMP="$ROOT/deploy/gmp"
PT="$ROOT/deploy/public-testnet"
MESH="${MESH_ROOT:-$HOME/falcon-dag-mesh}"

# shellcheck disable=SC1091
if [[ -f "$GMP/stack.env.default" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$GMP/stack.env.default"
  set +a
fi

export SSH_F2="${SSH_F2:-100.64.0.3}"
export F2_IP="${F2_IP:-192.0.2.2}"
export STACK_SHARDS="${STACK_SHARDS:-2}"
# Public testnet network ids: s0=9003, s1=9004
# Force 9003 unless operator explicitly set PUBLIC_TESTNET_NID_BASE
export NETWORK_ID_BASE="${PUBLIC_TESTNET_NID_BASE:-9003}"
export FACADE_URL="${FACADE_URL:-http://127.0.0.1:18499}"
export TESTNET_GENESIS="$PT/genesis/network.json"
export FAUCET_URL="${FAUCET_URL:-http://127.0.0.1:18550}"

mkdir -p "$MESH/logs" "$MESH/stack"

echo "== falcon-dag-testnet-1 start (network_id_base=$NETWORK_ID_BASE) =="

# Ensure public genesis is present in mesh for clients
mkdir -p "$MESH/testnet"
cp -f "$PT/genesis/network.json" "$MESH/testnet/network.json"
cp -f "$PT/genesis/unl_public.json" "$MESH/testnet/unl_public.json"
cp -f "$PT/genesis/bootstrap.template.json" "$MESH/testnet/bootstrap.template.json"

# Write live bootstrap with detected IPs
F1_IP="${F1_IP:-$(ip -4 -br addr 2>/dev/null | awk '/^lo /{next} /tailscale/{next} /docker/{next} /br-/{next} {print; exit}' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' | head -1)}"
F1_IP="${F1_IP:-127.0.0.1}"
export F1_IP
python3 - <<PY
import json, os
from pathlib import Path
mesh = Path(os.environ["MESH_ROOT"] if "MESH_ROOT" in os.environ else Path.home()/"falcon-dag-mesh")
# MESH set via shell
mesh = Path("$MESH")
f1, f2 = "$F1_IP", "$F2_IP"
tpl = json.loads((mesh/"testnet"/"bootstrap.template.json").read_text())
raw = json.dumps(tpl).replace("HOST_F2", f2).replace("HOST", f1)
boot = json.loads(raw)
boot["facade"] = f"http://{f1}:18499"
boot["faucet"] = f"http://{f1}:18550"
boot["public_rpc_s0"] = f"http://{f1}:18001"
boot["public_rpc_s1"] = f"http://{f1}:18004"
boot["f1_ip"] = f1
boot["f2_ip"] = f2
boot["network_id_base"] = int("$NETWORK_ID_BASE")
(mesh/"testnet"/"bootstrap.json").write_text(json.dumps(boot, indent=2)+"\n")
print("wrote", mesh/"testnet"/"bootstrap.json")
PY

# Boot dual-shard stack (pass network id via env into boot_mesh)
echo "== mesh + gmp + facade =="
# Patch: start-stack calls boot_mesh without network-id-base flag; export for boot_mesh default
export NETWORK_ID_BASE
# start-stack currently hardcodes boot_mesh --shards only; inject via env NETWORK_ID_BASE (boot_mesh reads it)
bash "$GMP/start-stack.sh" --shards "$STACK_SHARDS"

# Faucet
echo "== faucet =="
if [[ -f "$MESH/logs/faucet.pid" ]]; then
  old=$(cat "$MESH/logs/faucet.pid" || true)
  if [[ -n "${old:-}" && -d "/proc/$old" ]]; then
    kill "$old" 2>/dev/null || true
    sleep 0.3
  fi
fi
nohup python3 "$PT/scripts/faucetd.py" >>"$MESH/logs/faucet.log" 2>&1 &
echo $! >"$MESH/logs/faucet.pid"
echo "  faucet pid=$(cat "$MESH/logs/faucet.pid") :18550"

# Watchdog (clear stale lock if dead)
echo "== watchdog =="
if [[ -f "$MESH/logs/watchdog.pid" ]]; then
  wpid=$(cat "$MESH/logs/watchdog.pid" 2>/dev/null || true)
  if [[ -z "${wpid:-}" || ! -d "/proc/$wpid" ]]; then
    rm -f "$MESH/stack/watchdog.lock" "$MESH/logs/watchdog.pid"
  fi
else
  rm -f "$MESH/stack/watchdog.lock"
fi
bash "$GMP/watchdog.sh" --daemon

# Identity file for status
python3 - <<PY
import json, time
from pathlib import Path
mesh = Path("$MESH")
ident = {
    "network_name": "falcon-dag-testnet-1",
    "tier": "public-play-testnet-R2",
    "started_at": time.time(),
    "network_id_base": int("$NETWORK_ID_BASE"),
    "facade": "http://$F1_IP:18499",
    "faucet": "http://$F1_IP:18550",
    "public_rpc": ["http://$F1_IP:18001", "http://$F1_IP:18004"],
    "crypto": "falcon-512",
    "no_real_money": True,
}
(mesh/"testnet"/"identity.json").write_text(json.dumps(ident, indent=2)+"\n")
print(json.dumps(ident, indent=2))
PY

echo
echo "== readiness =="
bash "$PT/scripts/readiness.sh" || true
echo
echo "Public entrypoints (this host):"
echo "  Facade:  http://$F1_IP:18499"
echo "  Faucet:  http://$F1_IP:18550"
echo "  RPC s0:  http://$F1_IP:18001"
echo "  RPC s1:  http://$F1_IP:18004"
echo "Docs: $PT/PUBLIC_TESTNET.md"
