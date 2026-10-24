#!/usr/bin/env bash
set -euo pipefail
ROOTDIR="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME="${RUNTIME_DIR:-$ROOTDIR/data/regtest}"
PIDS="$RUNTIME/pids.txt"
log() { echo "[falcon-dag-regtest] $*"; }
if [[ ! -f "$PIDS" ]]; then
  log "No pids file."
  exit 0
fi
while IFS= read -r pid; do
  [[ -z "$pid" ]] && continue
  kill "$pid" 2>/dev/null || true
done < "$PIDS"
sleep 0.3
while IFS= read -r pid; do
  [[ -z "$pid" ]] && continue
  kill -9 "$pid" 2>/dev/null || true
done < "$PIDS"
rm -f "$PIDS"
log "Stopped."
