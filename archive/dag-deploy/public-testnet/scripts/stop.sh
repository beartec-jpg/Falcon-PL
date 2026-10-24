#!/usr/bin/env bash
# Stop public testnet (stack + faucet + optional watchdog with --all)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
GMP="$ROOT/deploy/gmp"
MESH="${MESH_ROOT:-$HOME/falcon-dag-mesh}"

# faucet
if [[ -f "$MESH/logs/faucet.pid" ]]; then
  pid=$(cat "$MESH/logs/faucet.pid" || true)
  if [[ -n "${pid:-}" && -d "/proc/$pid" ]]; then
    kill "$pid" 2>/dev/null || true
    sleep 0.2
    kill -9 "$pid" 2>/dev/null || true
  fi
  rm -f "$MESH/logs/faucet.pid"
fi
fuser -k 18550/tcp 2>/dev/null || true

if [[ "${1:-}" == "--all" ]]; then
  bash "$GMP/stop-stack.sh" --all
else
  bash "$GMP/stop-stack.sh"
fi
echo "public-testnet stopped"
