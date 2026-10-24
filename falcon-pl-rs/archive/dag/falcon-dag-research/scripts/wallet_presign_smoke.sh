#!/usr/bin/env bash
# Client-signed txs: falcon-dag-wallet signs, node verifies with --no-auto-sign.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
DAGD="${DAGD:-$ROOT/target/release/falcon-dagd}"
WALLET="${WALLET:-$ROOT/target/release/falcon-dag-wallet}"
RPC="127.0.0.1:18401"
WORKDIR=$(mktemp -d /tmp/fd-wallet-smoke.XXXXXX)
PID=""

cleanup() {
  [[ -n "${PID:-}" ]] && kill "$PID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT

if [[ ! -x "$DAGD" || ! -x "$WALLET" ]]; then
  cargo build --release -p fd-node
  DAGD="$ROOT/target/release/falcon-dagd"
  WALLET="$ROOT/target/release/falcon-dag-wallet"
fi

fuser -k 18401/tcp 2>/dev/null || true
sleep 0.2

echo "workdir=$WORKDIR"
"$WALLET" gen-keys --dir "$WORKDIR/wallets" --accounts 5

# Node: require sigs, NO auto-sign, HMAC for speed (wallet can be hmac too)
# Use hmac scheme so we don't need Falcon node keys; wallet signs HMAC-dev.
"$DAGD" \
  --id val1 \
  --rpc "$RPC" \
  --unl val1 \
  --worker-count 1 \
  --crypto hmac-sha256-dev \
  --require-tx-sigs \
  --no-auto-sign \
  --batch-interval 0.1 \
  >"$WORKDIR/node.log" 2>&1 &
PID=$!

for _ in $(seq 1 40); do
  if curl -sf -m 1 -X POST "http://$RPC" -H 'Content-Type: application/json' \
    -d '{"method":"ping","params":[{}]}' | grep -q pong; then
    break
  fi
  sleep 0.1
done

# Unsigned submit must fail
BAD=$(curl -sf -X POST "http://$RPC" -H 'Content-Type: application/json' \
  -d '{"method":"submit","params":[{"account":"acct1","sequence":1,"destination":"acct2","amount":1,"fee":10}]}')
echo "$BAD" | grep -q tefBAD_SIGNATURE || {
  echo "FAIL: expected unsigned reject, got $BAD"
  exit 1
}
echo "unsigned rejected OK"

# Pre-sign 5 payments with HMAC wallet CLI
for seq in $(seq 1 5); do
  "$WALLET" sign \
    --scheme hmac-sha256-dev \
    --network-id 9002 \
    --account acct1 \
    --sequence "$seq" \
    --destination acct2 \
    --amount 1 \
    --fee 10 \
    --out "$WORKDIR/tx$seq.json"
  "$WALLET" verify --file "$WORKDIR/tx$seq.json" --network-id 9002
  BODY=$("$WALLET" submit-body --file "$WORKDIR/tx$seq.json")
  RES=$(curl -sf -X POST "http://$RPC" -H 'Content-Type: application/json' -d "$BODY")
  echo "$RES" | grep -q tesSUCCESS || {
    echo "FAIL submit seq=$seq: $RES"
    cat "$WORKDIR/node.log"
    exit 1
  }
done

# wait execute
OK=0
for _ in $(seq 1 40); do
  EXEC=$(curl -sf -X POST "http://$RPC" -H 'Content-Type: application/json' \
    -d '{"method":"server_info","params":[{}]}' \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['info']['metrics']['executed_txs'])")
  if [[ "$EXEC" -ge 5 ]]; then
    OK=1
    break
  fi
  sleep 0.15
done
[[ $OK -eq 1 ]] || { echo "FAIL executed=$EXEC"; exit 1; }

# Prometheus metrics
MET=$(curl -sf "http://$RPC/metrics")
echo "$MET" | grep -q falcon_dag_executed_txs || {
  echo "FAIL: missing prometheus metrics"
  echo "$MET"
  exit 1
}
echo "$MET" | grep -q 'falcon_dag_executed_txs{.*} 5' || echo "$MET" | grep falcon_dag_executed_txs

echo "wallet_presign_smoke OK (5 client-signed txs + /metrics)"
