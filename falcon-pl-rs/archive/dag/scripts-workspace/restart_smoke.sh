#!/usr/bin/env bash
# Restart recovery: execute txs → snapshot → kill → restart → same state.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC="${RPC:-127.0.0.1:18301}"
PORT="${RPC##*:}"
DATA=$(mktemp -d /tmp/fd-restart.XXXXXX)
LOG="$DATA/node.log"
PID=""

cleanup() {
  [[ -n "${PID:-}" ]] && kill "$PID" 2>/dev/null || true
  smoke_kill_port "$PORT"
  wait 2>/dev/null || true
  rm -rf "$DATA" 2>/dev/null || true
}
trap cleanup EXIT

smoke_require_bin "$ROOT"
echo "data_dir=$DATA"
PID=$(smoke_start_node "$RPC" "$LOG" "$DATA")
smoke_wait_rpc "$RPC" 60 || { tail -80 "$LOG" || true; exit 1; }

post() { smoke_post "$RPC" "$1"; }

echo "submitting 5 txs..."
for seq in $(seq 1 5); do
  smoke_require_engine \
    "$(post "{\"method\":\"submit\",\"params\":[{\"account\":\"acct1\",\"sequence\":$seq,\"destination\":\"acct2\",\"amount\":1,\"fee\":10}]}")" \
    tesSUCCESS "payment seq=$seq"
done

# wait execute + snapshot
OK=0
for _ in $(seq 1 40); do
  INFO=$(post '{"method":"server_info","params":[{}]}')
  EXEC=$(echo "$INFO" | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['info']['metrics']['executed_txs'])" 2>/dev/null || echo 0)
  if [[ "$EXEC" -ge 5 ]]; then
    OK=1
    break
  fi
  sleep 0.15
done
[[ $OK -eq 1 ]] || { echo "FAIL: no exec"; cat "$LOG"; exit 1; }

SNAP=$(post '{"method":"snapshot","params":[{}]}')
echo "$SNAP" | python3 -c "import sys,json;d=json.load(sys.stdin); r=d.get('result',d); assert r.get('ok') is True, d; print('snapshot OK', r.get('path'))"
[[ -f "$DATA/snapshot.json" ]] || { echo "FAIL: missing snapshot.json"; exit 1; }

CAN0=$(post '{"method":"canonical","params":[{}]}')
TXS0=$(echo "$CAN0" | python3 -c "import sys,json; print(','.join(json.load(sys.stdin)['result']['txs']))")
BAL0=$(echo "$INFO" | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['info']['metrics']['executed_txs'])")

echo "killing node (executed=$BAL0)..."
kill "$PID"
wait "$PID" 2>/dev/null || true
PID=""
smoke_kill_port "$PORT"
sleep 0.3

echo "restarting from snapshot..."
PID=$(smoke_start_node "$RPC" "$LOG" "$DATA")
smoke_wait_rpc "$RPC" 60 || { tail -80 "$LOG" || true; exit 1; }
sleep 0.5

INFO2=$(post '{"method":"server_info","params":[{}]}')
EXEC2=$(echo "$INFO2" | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['info']['metrics']['executed_txs'])")
CAN2=$(post '{"method":"canonical","params":[{}]}')
TXS2=$(echo "$CAN2" | python3 -c "import sys,json; print(','.join(json.load(sys.stdin)['result']['txs']))")

if [[ "$EXEC2" -lt 5 ]]; then
  echo "FAIL: after restart executed=$EXEC2 expected >=5"
  cat "$LOG"
  exit 1
fi
if [[ "$TXS2" != "$TXS0" ]]; then
  echo "FAIL: executed tx set mismatch after restart"
  echo "before: $TXS0"
  echo "after:  $TXS2"
  exit 1
fi

smoke_require_engine \
  "$(post '{"method":"submit","params":[{"account":"acct1","sequence":6,"destination":"acct2","amount":1,"fee":10}]}')" \
  tesSUCCESS "payment after restart"
EXEC3=0
for _ in $(seq 1 30); do
  EXEC3=$(post '{"method":"server_info","params":[{}]}' \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['info']['metrics']['executed_txs'])" 2>/dev/null || echo 0)
  [[ "$EXEC3" -ge 6 ]] && break
  sleep 0.15
done
if [[ "$EXEC3" -lt 6 ]]; then
  echo "FAIL: could not continue after restart (executed=$EXEC3)"
  tail -40 "$LOG" || true
  exit 1
fi

echo "restart_smoke OK: recovered $EXEC2 txs, continued to $EXEC3"
echo "snapshot: $DATA/snapshot.json"
