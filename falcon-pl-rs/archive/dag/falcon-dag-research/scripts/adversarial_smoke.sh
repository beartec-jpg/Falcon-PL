#!/usr/bin/env bash
# Adversarial multinode pack under signed-tx rules:
#  1) Double-spend: same account/seq, two destinations → exactly one executes everywhere
#  2) Partition: kill val5, progress on majority, rejoin + catchup → agreement
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
WALLET="${WALLET:-$ROOT/target/release/falcon-dag-wallet}"
BASE_RPC=18500
BASE_P2P=19500
N=5
UNL="val1,val2,val3,val4,val5"
PIDS=()
LOGDIR=$(mktemp -d /tmp/fd-adv.XXXXXX)

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT

if [[ ! -x "$BIN" || ! -x "$WALLET" ]]; then
  cargo build --release -p fd-node
  BIN="$ROOT/target/release/falcon-dagd"
  WALLET="$ROOT/target/release/falcon-dag-wallet"
fi

rpc() { echo "127.0.0.1:$((BASE_RPC + $1))"; }
p2p() { echo "127.0.0.1:$((BASE_P2P + $1))"; }

for i in $(seq 0 $((N-1))); do
  fuser -k $((BASE_RPC+i))/tcp 2>/dev/null || true
  fuser -k $((BASE_P2P+i))/tcp 2>/dev/null || true
done
sleep 0.2

start_node() {
  local i=$1
  local id="val$((i+1))"
  local peers=""
  for j in $(seq 0 $((N-1))); do
    [[ $j -eq $i ]] && continue
    peers+="$(p2p $j),"
  done
  peers=${peers%,}
  "$BIN" \
    --id "$id" \
    --rpc "$(rpc $i)" \
    --p2p "$(p2p $i)" \
    --peers "$peers" \
    --unl "$UNL" \
    --worker-index "$i" \
    --worker-count "$N" \
    --network-id 9002 \
    --crypto hmac-sha256-dev \
    --require-tx-sigs \
    --auto-sign \
    --batch-interval 0.12 \
    >"$LOGDIR/$id.log" 2>&1 &
  PIDS+=($!)
}

echo "=== adversarial_smoke logdir=$LOGDIR ==="
for i in $(seq 0 $((N-1))); do start_node "$i"; done

for i in $(seq 0 $((N-1))); do
  for _ in $(seq 1 50); do
    if curl -sf -m 1 -X POST "http://$(rpc $i)" -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' | grep -q pong; then
      break
    fi
    sleep 0.1
  done
done
sleep 1.2

# ---------- 1) Double-spend ----------
echo "--- double-spend: seq=1 to acct2 vs acct3 ---"
"$WALLET" sign --scheme hmac-sha256-dev --network-id 9002 \
  --account acct1 --sequence 1 --destination acct2 --amount 50 --fee 10 \
  --out "$LOGDIR/ds_a.json"
"$WALLET" sign --scheme hmac-sha256-dev --network-id 9002 \
  --account acct1 --sequence 1 --destination acct3 --amount 50 --fee 10 \
  --out "$LOGDIR/ds_b.json"

BODY_A=$("$WALLET" submit-body --file "$LOGDIR/ds_a.json")
BODY_B=$("$WALLET" submit-body --file "$LOGDIR/ds_b.json")
curl -sf -m 5 -X POST "http://$(rpc 0)" -H 'Content-Type: application/json' -d "$BODY_A" >/dev/null || true
curl -sf -m 5 -X POST "http://$(rpc 1)" -H 'Content-Type: application/json' -d "$BODY_B" >/dev/null || true

TXA=$(python3 -c "import json; print(json.load(open('$LOGDIR/ds_a.json'))['tx_id'])")
TXB=$(python3 -c "import json; print(json.load(open('$LOGDIR/ds_b.json'))['tx_id'])")

curl -sf -m 5 -X POST "http://$(rpc 0)" -H 'Content-Type: application/json' \
  -d '{"method":"submit","params":[{"account":"acct1","sequence":2,"destination":"acct2","amount":1,"fee":10}]}' >/dev/null || true

OK=0
WINNER=""
for _ in $(seq 1 60); do
  RES=$(python3 -c "
import json, urllib.request
txa, txb = '$TXA', '$TXB'
base = $BASE_RPC
winners = []
for i in range($N):
    req = urllib.request.Request(
        f'http://127.0.0.1:{base+i}',
        data=b'{\"method\":\"canonical\",\"params\":[{}]}',
        headers={'Content-Type':'application/json'}, method='POST')
    try:
        with urllib.request.urlopen(req, timeout=3) as r:
            txs = json.load(r)['result']['txs']
    except Exception:
        print('WAIT'); raise SystemExit
    ha, hb = txa in txs, txb in txs
    if ha + hb != 1:
        print('WAIT'); raise SystemExit
    winners.append('A' if ha else 'B')
if len(set(winners)) == 1 and len(winners) == $N:
    print('OK', winners[0])
else:
    print('WAIT')
" 2>/dev/null || echo WAIT)
  if [[ "$RES" == OK* ]]; then
    OK=1
    WINNER=${RES#OK }
    echo "double-spend OK: all validators executed only winner=$WINNER"
    break
  fi
  sleep 0.25
done
if [[ $OK -ne 1 ]]; then
  echo "FAIL: double-spend did not converge to single winner"
  for i in $(seq 0 $((N-1))); do
    curl -sf -m 3 -X POST "http://$(rpc $i)" -H 'Content-Type: application/json' \
      -d '{"method":"canonical","params":[{}]}' \
      | python3 -c "import sys,json;print('val$((i+1))',json.load(sys.stdin)['result']['txs'][:5])" || true
  done
  exit 1
fi

# ---------- 2) Partition ----------
echo "--- partition: kill val5, progress on majority, rejoin ---"
# kill last started val5 (pid at end of PIDS)
kill "${PIDS[$((N-1))]}" 2>/dev/null || true
wait "${PIDS[$((N-1))]}" 2>/dev/null || true
PIDS=("${PIDS[@]:0:$((N-1))}")
sleep 0.5

for seq in 3 4 5 6; do
  curl -sf -m 5 -X POST "http://$(rpc 0)" -H 'Content-Type: application/json' \
    -d "{\"method\":\"submit\",\"params\":[{\"account\":\"acct1\",\"sequence\":$seq,\"destination\":\"acct2\",\"amount\":1,\"fee\":10}]}" >/dev/null || true
done

MAJ_OK=0
for _ in $(seq 1 80); do
  GOOD=1
  for i in 0 1 2 3; do
    EXEC=$(curl -sf -m 3 -X POST "http://$(rpc $i)" -H 'Content-Type: application/json' \
      -d '{"method":"server_info","params":[{}]}' \
      | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['info']['metrics']['executed_txs'])" 2>/dev/null || echo 0)
    if [[ "${EXEC:-0}" -lt 5 ]]; then GOOD=0; break; fi
  done
  if [[ $GOOD -eq 1 ]]; then MAJ_OK=1; break; fi
  sleep 0.2
done
[[ $MAJ_OK -eq 1 ]] || { echo "FAIL: majority did not progress during partition"; exit 1; }
echo "majority progressed while val5 partitioned"

start_node 4
for _ in $(seq 1 50); do
  if curl -sf -m 1 -X POST "http://$(rpc 4)" -H 'Content-Type: application/json' \
    -d '{"method":"ping","params":[{}]}' | grep -q pong; then
    break
  fi
  sleep 0.1
done
sleep 1.0
for i in 0 1 2 3; do
  curl -sf -m 5 -X POST "http://$(rpc $i)" -H 'Content-Type: application/json' \
    -d '{"method":"catchup","params":[{}]}' >/dev/null || true
done
curl -sf -m 5 -X POST "http://$(rpc 0)" -H 'Content-Type: application/json' \
  -d '{"method":"submit","params":[{"account":"acct1","sequence":7,"destination":"acct2","amount":1,"fee":10}]}' >/dev/null || true

REJOIN_OK=0
for _ in $(seq 1 100); do
  RES=$(python3 -c "
import json, urllib.request
base = $BASE_RPC
sets = []
for i in range($N):
    req = urllib.request.Request(
        f'http://127.0.0.1:{base+i}',
        data=b'{\"method\":\"canonical\",\"params\":[{}]}',
        headers={'Content-Type':'application/json'}, method='POST')
    try:
        with urllib.request.urlopen(req, timeout=3) as r:
            txs = tuple(json.load(r)['result']['txs'])
    except Exception:
        print('WAIT'); raise SystemExit
    sets.append(txs)
if len(sets[0]) >= 5 and all(s == sets[0] for s in sets):
    print('OK', len(sets[0]))
else:
    print('WAIT')
" 2>/dev/null || echo WAIT)
  if [[ "$RES" == OK* ]]; then
    REJOIN_OK=1
    echo "rejoin OK: all 5 validators agree on ${RES#OK } executed txs"
    break
  fi
  sleep 0.25
done
if [[ $REJOIN_OK -ne 1 ]]; then
  echo "FAIL: val5 did not rejoin to agreement"
  for i in $(seq 0 4); do
    curl -sf -m 3 -X POST "http://$(rpc $i)" -H 'Content-Type: application/json' \
      -d '{"method":"server_info","params":[{}]}' \
      | python3 -c "import sys,json;m=json.load(sys.stdin)['result']['info']['metrics'];print(m['validator_id'],'exec',m['executed_txs'],'cert',m['certified'])" || true
  done
  exit 1
fi

echo "adversarial_smoke OK (double-spend + partition/rejoin)"
