#!/usr/bin/env bash
# Issue token, transfer, pay fees, track supply + state_root.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC="${RPC:-127.0.0.1:18601}"
PORT="${RPC##*:}"
LOG="${LOG:-/tmp/fd-econ.log}"
PID=""

cleanup() {
  [[ -n "${PID:-}" ]] && kill "$PID" 2>/dev/null || true
  smoke_kill_port "$PORT"
  wait 2>/dev/null || true
}
trap cleanup EXIT

smoke_require_bin "$ROOT"
PID=$(smoke_start_node "$RPC" "$LOG")
smoke_wait_rpc "$RPC" 80 || { tail -50 "$LOG" || true; exit 1; }

post() { smoke_post "$RPC" "$1"; }
submit_ok() { smoke_require_engine "$(post "$1")" tesSUCCESS "${2:-submit}"; }
wait_exec() { sleep 0.4; }

submit_ok '{"method":"submit","params":[{"kind":"issue","account":"acct1","sequence":1,"asset":"USDC","amount":1000000,"destination":"acct1","fee":100,"max_supply":10000000}]}' "issue"
wait_exec
submit_ok '{"method":"submit","params":[{"kind":"transfer","account":"acct1","sequence":2,"asset":"USDC","amount":250000,"destination":"acct2","fee":50}]}' "transfer"
wait_exec
submit_ok '{"method":"submit","params":[{"kind":"payment","account":"acct1","sequence":3,"amount":1000,"destination":"acct2","fee":20}]}' "payment"
wait_exec
submit_ok '{"method":"submit","params":[{"kind":"mint","account":"acct1","sequence":4,"asset":"USDC","amount":5000,"destination":"acct2","fee":10}]}' "mint"
wait_exec
submit_ok '{"method":"submit","params":[{"kind":"burn","account":"acct2","sequence":1,"asset":"USDC","amount":1000,"fee":10}]}' "burn"
wait_exec

ECON=$(post '{"method":"economy","params":[{}]}')
echo "$ECON" | python3 -c "
import sys, json
d=json.load(sys.stdin)['result']
assert d['native_asset']=='FDAG', d
assert d['native_supply'] > 0, d
assert d['burned_native'] > 0, d
assert d['treasury_fees'] > 0, d
assert 'USDC' in d['assets'], d
usdc=d['assets']['USDC']
assert usdc['total_supply'] == 1_004_000, usdc
assert len(d['state_root'])==64, d
print('economy OK supply_usdc=', usdc['total_supply'], 'burned_native=', d['burned_native'], 'state_root=', d['state_root'][:16]+'...')
"
A1=$(post '{"method":"account_info","params":[{"account":"acct1"}]}')
A2=$(post '{"method":"account_info","params":[{"account":"acct2"}]}')
echo "$A1" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; print('acct1 USDC', d['balances'].get('USDC'), 'native', d['native'])"
echo "$A2" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; print('acct2 USDC', d['balances'].get('USDC'), 'native', d['native'])"
echo "economy_smoke OK (issue/transfer/mint/burn/fees/supply/state_root)"
