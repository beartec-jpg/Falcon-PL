#!/usr/bin/env bash
# Prove real DA / ledger split:
#  - val1,val2: --role da-worker  (pack/sign/cert, executed must stay 0)
#  - val3,val4,val5: --role order-exec (no pack; order certs + execute)
# Submit to a DA worker; balances appear on an order-exec node.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
BASE_RPC=29200
BASE_P2P=29300
UNL="val1,val2,val3,val4,val5"
LOGDIR=$(mktemp -d /tmp/fd-narwhal-split.XXXXXX)
PIDS=()

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  for i in 0 1 2 3 4; do
    fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
    fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
  (cd "$ROOT" && cargo build --release -p fd-node)
  BIN="$ROOT/target/release/falcon-dagd"
fi

# roles: 0,1 = da-worker; 2,3,4 = order-exec
roles=(da-worker da-worker order-exec order-exec order-exec)

for i in 0 1 2 3 4; do
  fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
  fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
done
sleep 0.2

echo "logdir=$LOGDIR"
for i in 0 1 2 3 4; do
  id="val$((i+1))"
  role="${roles[$i]}"
  peers=""
  for j in 0 1 2 3 4; do
    [[ $j -eq $i ]] && continue
    peers+="127.0.0.1:$((BASE_P2P+j)),"
  done
  peers=${peers%,}
  "$BIN" \
    --id "$id" \
    --role "$role" \
    --rpc "127.0.0.1:$((BASE_RPC+i))" \
    --p2p "127.0.0.1:$((BASE_P2P+i))" \
    --peers "$peers" \
    --unl "$UNL" \
    --worker-index "$i" \
    --worker-count 5 \
    --network-id 9003 \
    --crypto hmac-sha256-dev \
    --require-tx-sigs \
    --auto-sign \
    --batch-interval 0.05 \
    --max-batch-txs 128 \
    --unl-threshold 0.6 \
    --data-dir "$LOGDIR/$id" \
    >"$LOGDIR/$id.log" 2>&1 &
  PIDS+=($!)
  echo "started $id role=$role rpc=$((BASE_RPC+i))"
done

for i in 0 1 2 3 4; do
  for _ in $(seq 1 80); do
    if curl -sf -m1 -X POST "http://127.0.0.1:$((BASE_RPC+i))" \
      -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' 2>/dev/null | grep -q pong; then
      break
    fi
    sleep 0.1
  done
done

post() {
  curl -sf -m 8 -X POST "http://127.0.0.1:$1" -H 'Content-Type: application/json' -d "$2"
}

# Submit payments to DA worker val1
for seq in $(seq 1 20); do
  post $BASE_RPC "{\"method\":\"submit\",\"params\":[{\"account\":\"acct1\",\"sequence\":$seq,\"destination\":\"acct2\",\"amount\":1,\"fee\":10}]}" \
    | python3 -c "import sys,json;d=json.load(sys.stdin); c=(d.get('result') or d).get('engine_result'); assert c=='tesSUCCESS', d"
done
echo "submitted 20 txs to da-worker val1"

# Wait for cert + ledger (Bullshark/tip-BFT; packing may leave some txs uncommitted)
ok=0
for _ in $(seq 1 150); do
  EXEC=$(post $((BASE_RPC+2)) '{"method":"server_info","params":[{}]}' \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['info']['metrics'].get('executed_txs',0))" 2>/dev/null || echo 0)
  if [[ "${EXEC:-0}" -ge 3 ]]; then ok=1; break; fi
  sleep 0.15
done
[[ $ok -eq 1 ]] || { echo "FAIL: order-exec did not execute"; tail -30 "$LOGDIR/val3.log"; exit 1; }

# DA workers must not execute
for i in 0 1; do
  M=$(post $((BASE_RPC+i)) '{"method":"server_info","params":[{}]}')
  echo "$M" | python3 -c "
import sys,json
m=json.load(sys.stdin)['result']['info']['metrics']
assert m.get('role')=='da-worker', m
assert m.get('runs_da') is True
assert m.get('runs_ledger') is False
assert int(m.get('executed_txs') or 0)==0, ('da executed!', m)
assert int(m.get('batches_created') or 0)>=1 or int(m.get('certified') or 0)>=1, m
print('da-worker val', ${i}+1, 'OK batches', m.get('batches_created'), 'certs', m.get('certified'), 'exec', m.get('executed_txs'))
"
done

# Order-exec must execute and must not pack
for i in 2 3 4; do
  M=$(post $((BASE_RPC+i)) '{"method":"server_info","params":[{}]}')
  echo "$M" | python3 -c "
import sys,json
m=json.load(sys.stdin)['result']['info']['metrics']
assert m.get('role')=='order-exec', m
assert m.get('runs_da') is False
assert m.get('runs_ledger') is True
assert int(m.get('batches_created') or 0)==0, ('order-exec packed!', m)
assert int(m.get('executed_txs') or 0)>=3, m
arch = m.get('architecture') or ''
assert 'bullshark' in arch or 'order-exec' in arch or 'split' in arch, m
print('order-exec val', ${i}+1, 'OK exec', m.get('executed_txs'), 'ordered', m.get('ordered'),
      'bullshark', m.get('bullshark_anchors'), 'tip_commits', m.get('tip_commits'))
"
done

BAL=$(post $((BASE_RPC+2)) '{"method":"account_info","params":[{"account":"acct2"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('FDAG',0))")
python3 -c "assert int('$BAL')>=1000000000, 'acct2 should have received payments'; print('acct2 FDAG', '$BAL')"

echo "narwhal_split_smoke OK — DA workers pack/cert; order-exec orders certs and executes; no cross-contamination"
