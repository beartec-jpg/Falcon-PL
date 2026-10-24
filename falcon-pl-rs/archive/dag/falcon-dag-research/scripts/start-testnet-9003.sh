#!/usr/bin/env bash
# Start Falcon DAG public-testnet profile (network_id 9003).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
MODE="${1:-docker}"
COMPOSE="$ROOT/deploy/testnet-9003/docker-compose.yml"
PIDFILE="$ROOT/deploy/testnet-9003/bare.pids"
LOGDIR="$ROOT/deploy/testnet-9003/logs"
BIN="$ROOT/target/release/falcon-dagd"
UNL="val1,val2,val3,val4,val5"
BASE_RPC=28001
BASE_P2P=29001

if [[ "$MODE" == "--bare" || "$MODE" == "bare" ]]; then
  if [[ ! -x "$BIN" ]]; then
    (cd "$ROOT" && cargo build --release -p fd-node)
  fi
  mkdir -p "$LOGDIR" "$ROOT/deploy/testnet-9003/data"
  : >"$PIDFILE"
  for i in 0 1 2 3 4; do
    id="val$((i+1))"
    rpc="127.0.0.1:$((BASE_RPC+i))"
    p2p="127.0.0.1:$((BASE_P2P+i))"
    peers=""
    for j in 0 1 2 3 4; do
      [[ $j -eq $i ]] && continue
      peers+="127.0.0.1:$((BASE_P2P+j)),"
    done
    peers=${peers%,}
    data="$ROOT/deploy/testnet-9003/data/$id"
    mkdir -p "$data"
    fuser -k $((BASE_RPC+i))/tcp 2>/dev/null || true
    fuser -k $((BASE_P2P+i))/tcp 2>/dev/null || true
    "$BIN" \
      --id "$id" \
      --rpc "$rpc" \
      --p2p "$p2p" \
      --peers "$peers" \
      --unl "$UNL" \
      --worker-index "$i" \
      --worker-count 5 \
      --network-id 9003 \
      --crypto hmac-sha256-dev \
      --require-tx-sigs \
      --auto-sign \
      --data-dir "$data" \
      --snapshot-interval 15 \
      --batch-interval 0.2 \
      >"$LOGDIR/$id.log" 2>&1 &
    echo $! >>"$PIDFILE"
    echo "started $id pid=$! rpc=$rpc"
  done
  echo "bare testnet-9003 up (pids in $PIDFILE)"
  echo "RPC: 28001-28005  P2P: 29001-29005  network_id=9003"
  exit 0
fi

# Docker mode
if ! command -v docker >/dev/null 2>&1; then
  echo "docker not found; use: $0 --bare"
  exit 1
fi
cd "$ROOT"
docker compose -f "$COMPOSE" up --build -d
echo "docker testnet-9003 up"
echo "RPC: http://127.0.0.1:28001 .. 28005  network_id=9003"
echo "stop: ./scripts/stop-testnet-9003.sh"
