#!/usr/bin/env bash
# Protocol SPV rails smoke: all four rails synthetic mint + claim finality.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC="${RPC:-127.0.0.1:18811}"
PORT="${RPC##*:}"
LOG="${LOG:-/tmp/fd-bridge.log}"
PID=""

cleanup() {
  [[ -n "${PID:-}" ]] && kill "$PID" 2>/dev/null || true
  smoke_kill_port "$PORT"
  wait 2>/dev/null || true
}
trap cleanup EXIT

smoke_require_bin "$ROOT"
PID=$(smoke_start_node "$RPC" "$LOG")
smoke_wait_rpc "$RPC" 80 || { tail -80 "$LOG" || true; exit 1; }

post() { smoke_post "$RPC" "$1"; }
wait_exec() { sleep 0.45; }
submit_ok() {
  smoke_require_engine "$(post "$1")" tesSUCCESS "${2:-submit}"
}

RI=$(post '{"method":"rail_info","params":[{"rail":"BTC"}]}')
echo "$RI" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d.get('federated_mint') is False; assert d['mint_path']=='spv_deposit_only'; print('rail_info OK', d['rail'], d['lock_id'][:16], '...') "

# Issue must not mint (admit tecPROTOCOL preferred)
ISS=$(post '{"method":"submit","params":[{"kind":"issue","account":"acct1","sequence":1,"asset":"BTC","amount":999,"destination":"acct1","fee":1}]}' || true)
echo "$ISS" | python3 -c "
import sys,json
raw=sys.stdin.read().strip()
assert raw, 'empty issue response'
d=json.loads(raw)
code=(d.get('result') or d).get('engine_result')
assert code is not None and str(code).lower()!='none', raw
assert code!='tesSUCCESS', ('issue admitted', code)
print('issue BTC blocked admit', code)
"
wait_exec
SUP=$(post '{"method":"rail_info","params":[{"rail":"BTC"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result'].get('supply',0))")
python3 -c "assert int('$SUP')==0, 'federated/issue mint leaked'; print('issue BTC no mint OK supply', '$SUP')"

python3 "$ROOT/scripts/rail_agent.py" --rpc "http://$RPC" --rail BTC --indexer synthetic --amount 25000 --recipient acct2
wait_exec
BAL=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BTC',0))")
python3 -c "assert int('$BAL')==25000; print('deposit mint OK', '$BAL')"

SEQ2=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['sequence'])")
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"rail_withdraw\",\"account\":\"acct2\",\"sequence\":$SEQ2,\"asset\":\"BTC\",\"amount\":5000,\"external_to\":\"bc1q_smoke\",\"fee\":1}]}" "rail_withdraw"
wait_exec
BAL2=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BTC',0))")
python3 -c "assert int('$BAL2')==20000; print('withdraw burn OK', '$BAL2')"

RI2=$(post '{"method":"rail_info","params":[{"rail":"BTC"}]}')
echo "$RI2" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d['total_minted']==25000; assert d['total_burned']==5000; print('rail totals OK minted', d['total_minted'], 'burned', d['total_burned'])"

ISS2=$(post '{"method":"submit","params":[{"kind":"issue","account":"acct1","sequence":99,"asset":"BTC","amount":1,"destination":"acct1","fee":1}]}')
smoke_require_engine "$ISS2" tecPROTOCOL "issue blocked"

WN=$(post '{"method":"withdrawal_notes","params":[{"rail":"BTC"}]}')
echo "$WN" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d['count']>=1; assert d.get('committee') is None; assert len(d['claim_root'])==64; print('withdrawal_notes OK count', d['count'], 'root', d['claim_root'][:16])"
NID=$(echo "$WN" | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['notes'][0]['note_id'])")
CE=$(post "{\"method\":\"claim_export\",\"params\":[{\"note_id\":\"$NID\"}]}")
echo "$CE" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d['note_id']=='$NID'; assert d['claim_root']; assert d.get('committee') is None; print('claim_export OK path_len', len(d.get('merkle_path',[])))"

RI3=$(post '{"method":"rail_info","params":[{"rail":"BTC"}]}')
echo "$RI3" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d.get('claim_root_matches') is True, d; assert d.get('claim_note_count',0)>=1; print('committed claim_root OK', d['claim_root'][:16], 'h', d.get('claim_height'))"

python3 "$ROOT/scripts/rail_indexers/evm_outbound_claim.py" --rpc "http://$RPC" --rail BTC \
  | python3 -c "import sys,json;d=json.load(sys.stdin); assert d.get('verified') is True; assert d['finality']['ready_for_foreign_claim'] is True; print('evm_outbound_claim OK root', d['submitFalconRoot']['root'][:18])"

BS=$(post '{"method":"bridge_status","params":[{}]}')
echo "$BS" | python3 -c "import sys,json;d=json.load(sys.stdin)['result']; assert d.get('state_root'); assert d['rails']['BTC']['claim_root_matches'] is True; print('bridge_status OK hard_final', d.get('hard_final_batches'))"

python3 "$ROOT/scripts/rail_agent.py" --rpc "http://$RPC" --rail ETH --indexer synthetic --amount 5000 --recipient acct3 --prove-forbidden
wait_exec
ETH_BAL=$(post '{"method":"account_info","params":[{"account":"acct3"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('ETH',0))")
python3 -c "assert int('$ETH_BAL')==5000; print('ETH inbound mint OK', '$ETH_BAL')"

python3 "$ROOT/scripts/rail_agent.py" --rpc "http://$RPC" --rail BNB --indexer synthetic --amount 3000 --recipient acct4 --prove-forbidden
wait_exec
BNB_BAL=$(post '{"method":"account_info","params":[{"account":"acct4"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BNB',0))")
python3 -c "assert int('$BNB_BAL')==3000; print('BNB inbound mint OK', '$BNB_BAL')"

python3 "$ROOT/scripts/rail_agent.py" --rpc "http://$RPC" --rail XRP --indexer synthetic --amount 4000 --recipient acct5 --prove-forbidden
wait_exec
XRP_BAL=$(post '{"method":"account_info","params":[{"account":"acct5"}]}' | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('XRP',0))")
python3 -c "assert int('$XRP_BAL')==4000; print('XRP inbound mint OK', '$XRP_BAL')"

# Multi-rail outbound: partial withdraw + claim_root_matches per rail
withdraw_partial() {
  local acct="$1" asset="$2" amt="$3" ext="$4"
  local seq bal
  seq=$(post "{\"method\":\"account_info\",\"params\":[{\"account\":\"$acct\"}]}" \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['sequence'])")
  submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"rail_withdraw\",\"account\":\"$acct\",\"sequence\":$seq,\"asset\":\"$asset\",\"amount\":$amt,\"external_to\":\"$ext\",\"fee\":1}]}" "withdraw $asset"
  wait_exec
  bal=$(post "{\"method\":\"account_info\",\"params\":[{\"account\":\"$acct\"}]}" \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('$asset',0))")
  echo "$asset post-withdraw bal=$bal"
  post "{\"method\":\"rail_info\",\"params\":[{\"rail\":\"$asset\"}]}" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
assert d.get('claim_root_matches') is True, d
assert int(d.get('total_burned',0))>=$amt, d
print('$asset claim_root OK burned', d['total_burned'], d['claim_root'][:16])
"
}

withdraw_partial acct3 ETH 1500 "0xeth_smoke_out"
withdraw_partial acct4 BNB 1000 "0xbnb_smoke_out"
withdraw_partial acct5 XRP 1200 "rXrpSmokeOut"

# Free Burn of rail assets must never be the burn path
BURN_BTC=$(post '{"method":"submit","params":[{"kind":"burn","account":"acct2","sequence":50,"asset":"BTC","amount":1,"fee":1}]}')
smoke_require_engine "$BURN_BTC" tecPROTOCOL "free burn BTC blocked"
BURN_ETH=$(post '{"method":"submit","params":[{"kind":"burn","account":"acct3","sequence":50,"asset":"ETH","amount":1,"fee":1}]}')
smoke_require_engine "$BURN_ETH" tecPROTOCOL "free burn ETH blocked"

BS2=$(post '{"method":"bridge_status","params":[{}]}')
echo "$BS2" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
for r in ('BTC','ETH','BNB','XRP'):
    assert d['rails'][r]['claim_root_matches'] is True, (r, d['rails'][r])
    assert int(d['rails'][r].get('total_burned',0))>0, (r, d['rails'][r])
print('all rails claim_root_matches + burned OK')
"

echo "bridge_smoke OK (BTC+ETH+BNB+XRP inbound+outbound + claim finality, no federated mint/burn)"
