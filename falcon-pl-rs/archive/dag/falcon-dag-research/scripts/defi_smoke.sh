#!/usr/bin/env bash
# Protocol AMM + lending smoke: sealed accounts, swap/oracle/fee skim, no free payment.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC="${RPC:-127.0.0.1:18801}"
PORT="${RPC##*:}"
LOG="${LOG:-/tmp/fd-defi.log}"
PID=""

cleanup() {
  [[ -n "${PID:-}" ]] && kill "$PID" 2>/dev/null || true
  smoke_kill_port "$PORT"
  wait 2>/dev/null || true
}
trap cleanup EXIT

smoke_require_bin "$ROOT"
PID=$(smoke_start_node "$RPC" "$LOG")
smoke_wait_rpc "$RPC" 80 || { echo "--- node log ---"; tail -50 "$LOG" || true; exit 1; }

post() { smoke_post "$RPC" "$1"; }
wait_exec() { sleep 0.4; }
submit_ok() {
  local body label
  body=$(post "$1")
  label="${2:-submit}"
  smoke_require_engine "$body" tesSUCCESS "$label"
}

seq1=1
# Fund pool shell + open as Pool
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"payment\",\"account\":\"acct1\",\"sequence\":$seq1,\"destination\":\"pool1\",\"amount\":200,\"fee\":10}]}" "fund pool1"
wait_exec; seq1=$((seq1+1))
submit_ok '{"method":"submit","params":[{"kind":"open_account_type","account":"pool1","sequence":1,"account_type":"pool","fee":10}]}' "open pool"
wait_exec

submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"issue\",\"account\":\"acct1\",\"sequence\":$seq1,\"asset\":\"GOLD\",\"amount\":100000,\"destination\":\"acct1\",\"fee\":10}]}" "issue GOLD"
wait_exec; seq1=$((seq1+1))
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"create_pool\",\"account\":\"acct1\",\"sequence\":$seq1,\"destination\":\"pool1\",\"asset\":\"FDAG\",\"asset_b\":\"GOLD\",\"amount\":20000,\"amount_b\":40000,\"fee\":10}]}" "create_pool"
wait_exec; seq1=$((seq1+1))

PI=$(post '{"method":"pool_info","params":[{"pool":"pool1"}]}')
echo "$PI" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d.get('key_can_payment') is False; assert d['reserve_b']==40000; assert d.get('protocol_fee_share_bps',0)>0; print('pool_info OK', d['reserve_a'], d['reserve_b'], d['lp_supply'], 'proto_share', d['protocol_fee_share_bps'])"

R_BEFORE=$(post '{"method":"account_info","params":[{"account":"acct9"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['native'])" 2>/dev/null || echo 0)
# free payment from pool must be protocol-rejected (admit or apply)
R=$(post '{"method":"submit","params":[{"kind":"payment","account":"pool1","sequence":2,"destination":"acct9","amount":1000,"fee":1}]}' || true)
echo "$R" | python3 -c "
import sys,json
raw=sys.stdin.read().strip()
if not raw:
  print('pool payment empty response (ok if rejected)')
  sys.exit(0)
d=json.loads(raw)
code=(d.get('result') or d).get('engine_result')
assert code in (None, 'tecPROTOCOL', 'tefBAD_SIGNATURE') or code != 'tesSUCCESS', ('pool drain admitted', code)
print('pool free payment blocked OK', code)
"
wait_exec
R_AFTER=$(post '{"method":"account_info","params":[{"account":"acct9"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['native'])" 2>/dev/null || echo 0)
python3 -c "assert int('$R_BEFORE')==int('$R_AFTER'), ('pool drain', '$R_BEFORE', '$R_AFTER')"

submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"payment\",\"account\":\"acct1\",\"sequence\":$seq1,\"destination\":\"acct2\",\"amount\":12000,\"fee\":10}]}" "fund acct2"
wait_exec; seq1=$((seq1+1))
G0=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('GOLD',0))")
submit_ok '{"method":"submit","params":[{"kind":"swap","account":"acct2","sequence":1,"destination":"pool1","asset":"FDAG","amount":10000,"min_out":1,"fee":10}]}' "swap"
wait_exec
G1=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('GOLD',0))")
python3 -c "assert int('$G1')>int('$G0'); print('swap OK gold', '$G0', '->', '$G1')"
ECO1=$(post '{"method":"economy","params":[{}]}' | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; print(d.get('protocol_fees_skimmed',0))")
python3 -c "assert int('$ECO1')>0; print('protocol fee skim OK', '$ECO1')"

submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"oracle_vote\",\"account\":\"acct1\",\"sequence\":$seq1,\"asset\":\"GOLD\",\"amount\":2000000,\"fee\":10}]}" "oracle"
wait_exec; seq1=$((seq1+1))
OI=$(post '{"method":"oracle_info","params":[{"asset":"GOLD"}]}')
echo "$OI" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d['price_1e6']==2000000; print('oracle_info OK', d['price_1e6'])"

submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"payment\",\"account\":\"acct1\",\"sequence\":$seq1,\"destination\":\"mkt1\",\"amount\":100,\"fee\":10}]}" "fund mkt1"
wait_exec; seq1=$((seq1+1))
submit_ok '{"method":"submit","params":[{"kind":"open_account_type","account":"mkt1","sequence":1,"account_type":"lending","fee":10}]}' "open market"
wait_exec
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"create_market\",\"account\":\"acct1\",\"sequence\":$seq1,\"destination\":\"mkt1\",\"asset\":\"FDAG\",\"fee\":10,\"collateral_assets\":[\"FDAG\",\"GOLD\"]}]}" "create_market"
wait_exec; seq1=$((seq1+1))
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"supply\",\"account\":\"acct1\",\"sequence\":$seq1,\"destination\":\"mkt1\",\"amount\":8000,\"fee\":10}]}" "supply"
wait_exec; seq1=$((seq1+1))
submit_ok '{"method":"submit","params":[{"kind":"borrow","account":"acct2","sequence":2,"destination":"mkt1","amount":1000,"amount_b":2500,"fee":10}]}' "borrow"
wait_exec
submit_ok '{"method":"submit","params":[{"kind":"repay","account":"acct2","sequence":3,"destination":"mkt1","amount":1000,"fee":10}]}' "repay"
wait_exec

MI=$(post '{"method":"market_info","params":[{"market":"mkt1"}]}')
echo "$MI" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d.get('key_can_payment') is False; assert d.get('supply_index',0)>=10**9; assert 'GOLD' in d.get('allowed_collateral',[]); print('market_info OK reserve', d['reserve'], 'ltv', d['ltv_bps'], 'idx', d['supply_index'], d['borrow_index'])"

echo "defi_smoke OK (AMM + oracle + fee skim + multi-coll market + sealed accounts)"
