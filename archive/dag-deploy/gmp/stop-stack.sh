#!/usr/bin/env bash
# Stop lab stack (nodes, gmpd, facade). Does not kill watchdog unless --all.
set -euo pipefail
SSH_F2="${SSH_F2:-100.64.0.3}"
MESH="${MESH_ROOT:-$HOME/falcon-dag-mesh}"
echo "stop stack…"

kill_pidfile() {
  local f="$1"
  if [[ -f "$f" ]]; then
    local pid
    pid=$(cat "$f" 2>/dev/null || true)
    if [[ -n "${pid:-}" ]] && [[ -d "/proc/$pid" ]]; then
      kill "$pid" 2>/dev/null || true
      sleep 0.25
      if [[ -d "/proc/$pid" ]]; then
        kill -9 "$pid" 2>/dev/null || true
      fi
    fi
    rm -f "$f"
  fi
}

for name in facade gmp0 gmp1 gmp2 faucet; do
  kill_pidfile "$MESH/logs/${name}.pid"
done

# Local node pidfiles
shopt -s nullglob
for f in "$MESH"/logs/val*.pid "$MESH"/logs/arch*.pid; do
  kill_pidfile "$f"
done
shopt -u nullglob

if [[ "${1:-}" == "--all" ]]; then
  kill_pidfile "$MESH/logs/watchdog.pid"
  pkill -f "deploy/gmp/watchdog.sh" 2>/dev/null || true
  rm -f "$MESH/stack/watchdog.lock" "$MESH/stack/watchdog.heartbeat" 2>/dev/null || true
fi

pkill -x falcon-dagd 2>/dev/null || true
pkill -x falcon-gmpd 2>/dev/null || true
pkill -x falcon-gmp-facade 2>/dev/null || true
pkill -x falcon-gcpd 2>/dev/null || true
for p in 18499 18500 18510 18520 18501; do
  fuser -k "${p}/tcp" 2>/dev/null || true
done
ssh -o ConnectTimeout=8 -o BatchMode=yes "$SSH_F2" \
  'pkill -x falcon-dagd 2>/dev/null || true; for f in '"$MESH"'/logs/*.pid; do rm -f "$f"; done' 2>/dev/null || true

if [[ -d "$MESH/stack" ]]; then
  echo "{\"stopped_at\": $(date +%s)}" >"$MESH/stack/state.stopped.json" 2>/dev/null || true
fi
echo "stopped"
