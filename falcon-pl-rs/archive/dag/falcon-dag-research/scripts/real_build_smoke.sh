#!/usr/bin/env bash
# Phase 34: R1 real-build attempt — private 5-node network_id=9003.
# Starts bare testnet, checks multi-validator liveness, payments, sealed Issue,
# synthetic SPV deposit/withdraw/claim_root, one-node restart recovery.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC1="127.0.0.1:28001"
RPC2="127.0.0.1:28002"
NET=9003
REPORT="${REPORT:-/tmp/fd-real-build-report.json}"
START="$ROOT/scripts/start-testnet-9003.sh"
STOP="$ROOT/scripts/stop-testnet-9003.sh"

cleanup() {
  "$STOP" --bare 2>/dev/null || "$STOP" 2>/dev/null || true
  # bare stop may leave ports; hard clear
  for p in 28001 28002 28003 28004 28005 29001 29002 29003 29004 29005; do
    smoke_kill_port "$p" 2>/dev/null || true
  done
}
trap cleanup EXIT

smoke_require_bin "$ROOT"

echo "=== R1 real build: start bare testnet-9003 ==="
# clean prior state for reproducible attempt
rm -rf "$ROOT/deploy/testnet-9003/data" "$ROOT/deploy/testnet-9003/logs" \
  "$ROOT/deploy/testnet-9003/bare.pids" 2>/dev/null || true
"$START" --bare
sleep 1

for rpc in "$RPC1" "$RPC2" 127.0.0.1:28003 127.0.0.1:28004 127.0.0.1:28005; do
  smoke_wait_rpc "$rpc" 80 || { echo "FAIL: $rpc not ready"; exit 1; }
  echo "ping OK $rpc"
done

post() { smoke_post "$1" "$2"; }

# --- network identity ---
INFO=$(post "$RPC1" '{"method":"server_info","params":[{}]}')
echo "$INFO" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']['info']
assert d.get('network_id')==$NET or d.get('metrics',{}).get('network_id')==$NET, d
bv=d.get('build_version') or d.get('metrics',{}).get('build_version')
print('server_info OK network_id=$NET build', bv)
"

# --- multi-node payment (auto-sign faucet) ---
smoke_require_engine \
  "$(post "$RPC1" '{"method":"submit","params":[{"account":"acct1","sequence":1,"destination":"acct2","amount":7,"fee":10}]}')" \
  tesSUCCESS "payment on val1"
# wait for execution / propagation
OK=0
for _ in $(seq 1 40); do
  EXEC=$(post "$RPC1" '{"method":"server_info","params":[{}]}' \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['info']['metrics']['executed_txs'])" 2>/dev/null || echo 0)
  if [[ "${EXEC:-0}" -ge 1 ]]; then OK=1; break; fi
  sleep 0.2
done
[[ $OK -eq 1 ]] || { echo "FAIL: payment not executed on val1"; exit 1; }
echo "payment executed on val1 (executed_txs>=1)"

# peer should also progress (best-effort; allow lag)
sleep 1
EXEC2=$(post "$RPC2" '{"method":"server_info","params":[{}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['info']['metrics']['executed_txs'])" 2>/dev/null || echo 0)
echo "val2 executed_txs=$EXEC2 (propagation lag OK if >=0)"

# --- free Issue of rail blocked ---
ISS=$(post "$RPC1" '{"method":"submit","params":[{"kind":"issue","account":"acct1","sequence":2,"asset":"BTC","amount":1,"destination":"acct1","fee":1}]}')
smoke_require_engine "$ISS" tecPROTOCOL "issue BTC blocked"

# --- synthetic SPV deposit + withdraw + claim_root ---
python3 "$ROOT/scripts/rail_agent.py" --rpc "http://$RPC1" --rail BTC --indexer synthetic \
  --amount 12000 --recipient acct3
sleep 0.6
BAL=$(post "$RPC1" '{"method":"account_info","params":[{"account":"acct3"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BTC',0))")
python3 -c "assert int('$BAL')==12000, '$BAL'; print('rail mint OK', '$BAL')"

SEQ=$(post "$RPC1" '{"method":"account_info","params":[{"account":"acct3"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['sequence'])")
smoke_require_engine \
  "$(post "$RPC1" "{\"method\":\"submit\",\"params\":[{\"kind\":\"rail_withdraw\",\"account\":\"acct3\",\"sequence\":$SEQ,\"asset\":\"BTC\",\"amount\":3000,\"external_to\":\"bc1q_real_build\",\"fee\":1}]}")" \
  tesSUCCESS "rail_withdraw"
sleep 0.5
RI=$(post "$RPC1" '{"method":"rail_info","params":[{"rail":"BTC"}]}')
echo "$RI" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
assert d.get('total_minted')==12000, d
assert d.get('total_burned')==3000, d
assert d.get('claim_root_matches') is True, d
assert d.get('federated_mint') is False, d
print('rail claim_root OK', d['claim_root'][:16], 'minted', d['total_minted'], 'burned', d['total_burned'])
"
ROOT_CLAIM=$(echo "$RI" | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['claim_root'])")

# --- restart one validator from snapshot (val1 data dir) ---
DATA1="$ROOT/deploy/testnet-9003/data/val1"
[[ -f "$DATA1/snapshot.json" ]] || {
  post "$RPC1" '{"method":"snapshot","params":[{}]}' >/dev/null || true
  sleep 0.5
}
# force snapshot
SNAP=$(post "$RPC1" '{"method":"snapshot","params":[{}]}')
echo "$SNAP" | python3 -c "import sys,json;d=json.load(sys.stdin); r=d.get('result',d); assert r.get('ok') is True, d; print('snapshot OK')"

# kill only val1, leave others
PIDFILE="$ROOT/deploy/testnet-9003/bare.pids"
if [[ -f "$PIDFILE" ]]; then
  VAL1_PID=$(head -1 "$PIDFILE")
  kill "$VAL1_PID" 2>/dev/null || true
  wait "$VAL1_PID" 2>/dev/null || true
fi
smoke_kill_port 28001
smoke_kill_port 29001
sleep 0.4

# restart val1 only
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
"$BIN" \
  --id val1 --rpc 127.0.0.1:28001 --p2p 127.0.0.1:29001 \
  --peers 127.0.0.1:29002,127.0.0.1:29003,127.0.0.1:29004,127.0.0.1:29005 \
  --unl val1,val2,val3,val4,val5 \
  --worker-index 0 --worker-count 5 --network-id 9003 \
  --crypto hmac-sha256-dev --require-tx-sigs --auto-sign \
  --data-dir "$DATA1" --snapshot-interval 15 --batch-interval 0.2 \
  >"$ROOT/deploy/testnet-9003/logs/val1-restart.log" 2>&1 &
echo $! >>"$PIDFILE"
smoke_wait_rpc "$RPC1" 80 || { tail -40 "$ROOT/deploy/testnet-9003/logs/val1-restart.log"; exit 1; }
sleep 0.5

BAL_R=$(post "$RPC1" '{"method":"account_info","params":[{"account":"acct3"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BTC',0))")
python3 -c "assert int('$BAL_R')==9000, 'bal after val1 restart $BAL_R'; print('val1 restart balance OK', '$BAL_R')"
RI2=$(post "$RPC1" '{"method":"rail_info","params":[{"rail":"BTC"}]}')
echo "$RI2" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
assert d.get('claim_root')=='$ROOT_CLAIM', (d.get('claim_root'), '$ROOT_CLAIM')
assert d.get('claim_root_matches') is True
print('val1 restart claim_root OK')
"

# write report
python3 - <<PY
import json, time
rep = {
  "rung": "R1_private_multinode",
  "ok": True,
  "network_id": $NET,
  "rpc": ["28001-28005"],
  "checks": {
    "five_validators_ping": True,
    "payment_executed": True,
    "issue_btc_tecPROTOCOL": True,
    "rail_mint_withdraw_claim": True,
    "val1_snapshot_restart": True,
  },
  "claim_root_prefix": "$ROOT_CLAIM"[:16],
  "ts": time.time(),
  "next": "R2 public play testnet (Falcon-512 keys + hosted compose); R3 real rails not ready",
}
open("$REPORT","w").write(json.dumps(rep, indent=2))
print(json.dumps(rep, indent=2))
PY

echo "real_build_smoke OK — R1 private multi-node attempt passed"
echo "report: $REPORT"
echo "see docs/REAL_BUILD.md for R2/R3 gaps"
