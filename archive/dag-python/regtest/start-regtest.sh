#!/usr/bin/env bash
set -euo pipefail
ROOTDIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOTDIR"
RUNTIME="${RUNTIME_DIR:-$ROOTDIR/data/regtest}"
mkdir -p "$RUNTIME/logs"
PIDS="$RUNTIME/pids.txt"
export PYTHONPATH="${ROOTDIR}/src${PYTHONPATH:+:$PYTHONPATH}"
# Prefer project venv (has falcon-python for Falcon-512 certs)
if [[ -x "$ROOTDIR/.venv/bin/python" ]]; then
  PYTHON="$ROOTDIR/.venv/bin/python"
else
  PYTHON="python3"
fi

log() { echo "[falcon-dag-regtest] $*"; }
die() { echo "ERROR: $*" >&2; exit 1; }

log "Python: $PYTHON"
"$PYTHON" "$ROOTDIR/regtest/generate_configs.py"
if [[ -f "$PIDS" ]]; then
  "$ROOTDIR/regtest/stop-regtest.sh" || true
fi
: > "$PIDS"

log "Starting 5 Falcon DAG validators (Narwhal DA + Falcon-512 certs)..."
for i in 1 2 3 4 5; do
  "$PYTHON" -m falcon_dag.node --config "$ROOTDIR/regtest/configs/val${i}.json" \
    >"$RUNTIME/logs/val${i}.log" 2>&1 &
  echo $! >> "$PIDS"
  log "  val${i} pid=$! rpc=$((18000+i))"
done

wait_rpc() {
  local port=$1 name=$2
  for _ in $(seq 1 40); do
    if curl -sf -X POST "http://127.0.0.1:${port}" \
      -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' >/dev/null 2>&1; then
      log "  $name ready :$port"
      return 0
    fi
    sleep 0.25
  done
  die "$name not ready"
}

for i in 1 2 3 4 5; do wait_rpc $((18000+i)) "val${i}"; done
log "Falcon DAG regtest up. RPC 18001–18005 network_id=9002"
curl -s -X POST http://127.0.0.1:18001 -H 'Content-Type: application/json' \
  -d '{"method":"server_info","params":[{}]}' | python3 -m json.tool
