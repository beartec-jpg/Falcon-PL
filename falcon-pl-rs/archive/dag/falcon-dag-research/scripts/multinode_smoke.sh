#!/usr/bin/env bash
# 5-validator local P2P mesh smoke test.
# CRYPTO=hmac-sha256-dev (default) or CRYPTO=falcon-512
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
BASE_RPC=18200
BASE_P2P=19200
N=5
UNL="val1,val2,val3,val4,val5"
CRYPTO="${CRYPTO:-hmac-sha256-dev}"
PIDS=()
LOGDIR=$(mktemp -d /tmp/fd-multinode.XXXXXX)
KEYS=""

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
  cargo build --release -p fd-node
  BIN="$ROOT/target/release/falcon-dagd"
fi

if [[ "$CRYPTO" == falcon* ]]; then
  KEYS="$LOGDIR/keys"
  "$BIN" --gen-keys --keys-dir "$KEYS" --unl "$UNL"
  echo "generated Falcon-512 keys in $KEYS"
fi

for i in $(seq 0 $((N-1))); do
  fuser -k $((BASE_RPC+i))/tcp 2>/dev/null || true
  fuser -k $((BASE_P2P+i))/tcp 2>/dev/null || true
done
sleep 0.2

echo "starting $N validators crypto=$CRYPTO in $LOGDIR"
for i in $(seq 0 $((N-1))); do
  id="val$((i+1))"
  rpc="127.0.0.1:$((BASE_RPC+i))"
  p2p="127.0.0.1:$((BASE_P2P+i))"
  peers=""
  for j in $(seq 0 $((N-1))); do
    [[ $j -eq $i ]] && continue
    peers+="127.0.0.1:$((BASE_P2P+j)),"
  done
  peers=${peers%,}
  EXTRA=()
  if [[ -n "$KEYS" ]]; then
    EXTRA+=(--keys-dir "$KEYS")
  fi
  "$BIN" \
    --id "$id" \
    --rpc "$rpc" \
    --p2p "$p2p" \
    --peers "$peers" \
    --unl "$UNL" \
    --worker-index "$i" \
    --worker-count "$N" \
    --network-id 9002 \
    --batch-interval 0.05 \
    --max-batch-txs 64 \
    --unl-threshold 0.6 \
    --require-tx-sigs \
    --auto-sign \
    --crypto "$CRYPTO" \
    "${EXTRA[@]}" \
    >"$LOGDIR/$id.log" 2>&1 &
  PIDS+=($!)
done

for i in $(seq 0 $((N-1))); do
  rpc="127.0.0.1:$((BASE_RPC+i))"
  for _ in $(seq 1 50); do
    if curl -sf -X POST "http://$rpc" -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' | grep -q pong; then
      break
    fi
    sleep 0.15
  done
done

sleep 1.5

echo "submitting 10 txs to val1..."
for seq in $(seq 1 10); do
  curl -sf -X POST "http://127.0.0.1:$BASE_RPC" -H 'Content-Type: application/json' \
    -d "{\"method\":\"submit\",\"params\":[{\"account\":\"acct1\",\"sequence\":$seq,\"destination\":\"acct2\",\"amount\":1,\"fee\":10}]}" \
    | grep -q tesSUCCESS || true
done

echo "waiting for execution + agreement..."
OK=0
SCHEME=""
TXS0=""
for _ in $(seq 1 120); do
  ALL_OK=1
  TXS0=""
  for i in $(seq 0 $((N-1))); do
    rpc="127.0.0.1:$((BASE_RPC+i))"
    INFO=$(curl -sf -X POST "http://$rpc" -H 'Content-Type: application/json' \
      -d '{"method":"server_info","params":[{}]}' || true)
    EXEC=$(echo "$INFO" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('result',{}).get('info',{}).get('metrics',{}).get('executed_txs',0))" 2>/dev/null || echo 0)
    if [[ $i -eq 0 ]]; then
      SCHEME=$(echo "$INFO" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('result',{}).get('info',{}).get('metrics',{}).get('crypto_scheme',''))" 2>/dev/null || echo "")
    fi
    if [[ "${EXEC:-0}" -lt 10 ]]; then
      ALL_OK=0
      break
    fi
    CAN=$(curl -sf -X POST "http://$rpc" -H 'Content-Type: application/json' \
      -d '{"method":"canonical","params":[{}]}' || true)
    TXS=$(echo "$CAN" | python3 -c "import sys,json; print(','.join(json.load(sys.stdin)['result']['txs']))" 2>/dev/null || echo "")
    if [[ -z "$TXS0" ]]; then
      TXS0="$TXS"
    elif [[ "$TXS" != "$TXS0" ]]; then
      ALL_OK=0
      break
    fi
  done
  if [[ $ALL_OK -eq 1 && -n "$TXS0" ]]; then
    OK=1
    echo "all $N validators executed>=10 scheme=$SCHEME"
    break
  fi
  sleep 0.25
done

if [[ $OK -ne 1 ]]; then
  echo "FAIL: validators did not agree on 10 executed txs"
  tail -30 "$LOGDIR/val1.log" || true
  tail -15 "$LOGDIR/val2.log" || true
  exit 1
fi

if [[ "$CRYPTO" == falcon* && "$SCHEME" != falcon-512 ]]; then
  echo "FAIL: expected falcon-512 scheme, got $SCHEME"
  exit 1
fi

NTX=$(echo "$TXS0" | awk -F',' '{print NF}')
echo "multinode_smoke OK: $N validators agree on $NTX executed txs scheme=$SCHEME"
echo "logs: $LOGDIR"
