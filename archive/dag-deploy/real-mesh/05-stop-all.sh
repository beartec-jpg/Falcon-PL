#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
source "$ROOT/env.sh"

stop_ids_local() {
  local mesh="$MESH_ROOT_FALCON"
  local id
  for id in "$@"; do
    local pidf="$mesh/logs/${id}.pid"
    if [[ -f "$pidf" ]]; then
      kill "$(cat "$pidf")" 2>/dev/null || true
      rm -f "$pidf"
      echo "stopped local $id"
    fi
  done
}

stop_remote() {
  local host="$1" mesh="$2"
  shift 2
  local ids=("$@")
  ssh "$host" "bash -s" <<EOF
MESH="$mesh"
for id in ${ids[*]}; do
  pidf="\$MESH/logs/\$id.pid"
  if [[ -f "\$pidf" ]]; then
    kill "\$(cat "\$pidf")" 2>/dev/null || true
    rm -f "\$pidf"
    echo "stopped \$id on $host"
  fi
done
pkill -x falcon-dagd 2>/dev/null || true
true
EOF
}

stop_ids_local val1 val2 val3 arch1
# free local ports
for p in $VAL1_RPC $VAL2_RPC $VAL3_RPC $ARCH1_RPC $VAL1_P2P $VAL2_P2P $VAL3_P2P $ARCH1_P2P; do
  fuser -k "${p}/tcp" 2>/dev/null || true
done
pkill -x falcon-dagd 2>/dev/null || true

stop_remote host2 "$MESH_ROOT_FALCON" val4 val5 val6 arch2
ssh host2 "fuser -k ${VAL4_RPC}/tcp ${VAL5_RPC}/tcp ${VAL6_RPC}/tcp ${ARCH2_RPC}/tcp ${VAL4_P2P}/tcp ${VAL5_P2P}/tcp ${VAL6_P2P}/tcp ${ARCH2_P2P}/tcp 2>/dev/null || true; pkill -x falcon-dagd 2>/dev/null || true"

echo "stop-all done"
