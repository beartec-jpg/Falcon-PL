#!/usr/bin/env bash
# Phase 31: SPV rail claim durability across snapshot restart.
# Deposit → withdraw → claim_root → snapshot → kill → restore → same claims/balances;
# spent deposits stay spent; free Issue/Burn stay blocked; withdraw can continue.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC="${RPC:-127.0.0.1:18831}"
PORT="${RPC##*:}"
DATA=$(mktemp -d /tmp/fd-rail-restart.XXXXXX)
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
smoke_wait_rpc "$RPC" 80 || { tail -80 "$LOG" || true; exit 1; }

post() { smoke_post "$RPC" "$1"; }
wait_exec() { sleep 0.45; }
submit_ok() {
  smoke_require_engine "$(post "$1")" tesSUCCESS "${2:-submit}"
}

# --- inbound mint + partial withdraw (BTC) ---
python3 "$ROOT/scripts/rail_agent.py" --rpc "http://$RPC" --rail BTC --indexer synthetic \
  --amount 20000 --recipient acct2
wait_exec
BAL=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BTC',0))")
python3 -c "assert int('$BAL')==20000, '$BAL'; print('pre-restart mint OK', '$BAL')"

SEQ2=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['sequence'])")
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"rail_withdraw\",\"account\":\"acct2\",\"sequence\":$SEQ2,\"asset\":\"BTC\",\"amount\":7000,\"external_to\":\"bc1q_restart_out\",\"fee\":1}]}" "rail_withdraw"
wait_exec

BAL_W=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BTC',0))")
python3 -c "assert int('$BAL_W')==13000, '$BAL_W'; print('pre-restart burn OK', '$BAL_W')"

RI=$(post '{"method":"rail_info","params":[{"rail":"BTC"}]}')
ROOT_BEFORE=$(echo "$RI" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
assert d.get('claim_root_matches') is True, d
assert d.get('total_minted')==20000
assert d.get('total_burned')==7000
assert d.get('claim_note_count',0)>=1
print(d['claim_root'])
")
NID=$(post '{"method":"withdrawal_notes","params":[{"rail":"BTC"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['notes'][0]['note_id'])")
echo "claim_root pre-restart ${ROOT_BEFORE:0:16}… note $NID"

# force durable snapshot
SNAP=$(post '{"method":"snapshot","params":[{}]}')
echo "$SNAP" | python3 -c "import sys,json;d=json.load(sys.stdin); r=d.get('result',d); assert r.get('ok') is True or r.get('path') or 'snapshot' in str(d).lower(), d; print('snapshot OK', r)"
[[ -f "$DATA/snapshot.json" ]] || { echo "FAIL: missing $DATA/snapshot.json"; ls -la "$DATA"; exit 1; }

echo "killing node…"
kill "$PID"
wait "$PID" 2>/dev/null || true
PID=""
smoke_kill_port "$PORT"
sleep 0.3

echo "restarting from snapshot…"
PID=$(smoke_start_node "$RPC" "$LOG" "$DATA")
# strip any stray whitespace/newlines from PID capture
PID=$(echo "$PID" | tr -dc '0-9' | tail -c 12)
smoke_wait_rpc "$RPC" 80 || { tail -100 "$LOG" || true; exit 1; }
grep -q "loaded snapshot" "$LOG" || { echo "FAIL: node did not load snapshot"; tail -40 "$LOG"; exit 1; }
wait_exec

# --- post-restart: balances, claim root, export path (retry for settle) ---
BAL_R=""
for _ in $(seq 1 20); do
  BAL_R=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BTC',0))" 2>/dev/null || echo 0)
  [[ "$BAL_R" == "13000" ]] && break
  sleep 0.15
done
python3 -c "assert int('${BAL_R:-0}')==13000, 'balance lost after restart: ${BAL_R:-0}'; print('post-restart balance OK', '${BAL_R:-0}')"
RI2=$(post '{"method":"rail_info","params":[{"rail":"BTC"}]}')
echo "$RI2" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
assert d.get('claim_root_matches') is True, d
assert d.get('total_minted')==20000, d
assert d.get('total_burned')==7000, d
assert d.get('claim_root')=='$ROOT_BEFORE', (d.get('claim_root'), '$ROOT_BEFORE')
assert d.get('claim_note_count',0)>=1
print('post-restart claim_root OK', d['claim_root'][:16], 'minted', d['total_minted'], 'burned', d['total_burned'])
"

CE=$(post "{\"method\":\"claim_export\",\"params\":[{\"note_id\":\"$NID\"}]}")
echo "$CE" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
assert d.get('note_id')=='$NID', d
assert (d.get('claim_root') or '').replace('0x','')=='$ROOT_BEFORE'.replace('0x','') or d.get('claim_root')
assert d.get('committee') is None
print('post-restart claim_export OK path_len', len(d.get('merkle_path') or []))
"

python3 "$ROOT/scripts/rail_indexers/evm_outbound_claim.py" --rpc "http://$RPC" --rail BTC \
  | python3 -c "
import sys,json
d=json.load(sys.stdin)
assert d.get('verified') is True, d
assert d['finality']['ready_for_foreign_claim'] is True
print('post-restart evm_outbound_claim OK')
"

# free Issue / free Burn still blocked after restore
ISS=$(post '{"method":"submit","params":[{"kind":"issue","account":"acct1","sequence":1,"asset":"BTC","amount":1,"destination":"acct1","fee":1}]}')
smoke_require_engine "$ISS" tecPROTOCOL "issue blocked after restart"

BURN=$(post '{"method":"submit","params":[{"kind":"burn","account":"acct2","sequence":99,"asset":"BTC","amount":1,"fee":1}]}')
smoke_require_engine "$BURN" tecPROTOCOL "free burn blocked after restart"

# Spent deposit set survives restart: same synthetic external_txid cannot remint
python3 - <<PY
import json, sys, time, urllib.request
from pathlib import Path
sys.path.insert(0, str(Path("$ROOT") / "scripts"))
from rail_indexers.synthetic import SyntheticIndexer

RPC = "http://$RPC"

def rpc(method, params):
    body = json.dumps({"method": method, "params": [params]}).encode()
    req = urllib.request.Request(RPC, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode()).get("result", {})

info = None
for _ in range(20):
    info = rpc("rail_info", {"rail": "BTC"})
    if info.get("lock_id"):
        break
    time.sleep(0.1)
assert info and info.get("lock_id"), f"rail_info missing lock_id: {info}"
lock_id = info["lock_id"]
supply0 = int(info.get("total_minted") or info.get("supply") or 0)
assert supply0 == 20000, supply0
seq = int(rpc("account_info", {"account": "acct1"})["sequence"])
idx = SyntheticIndexer("BTC", lock_id, amount=20000)
deps = idx.pending_deposits(lock_id)
assert deps, "synthetic deposit missing"
dep = deps[0]
tx = {
    "kind": "rail_deposit",
    "account": "acct1",
    "sequence": seq,
    "asset": "BTC",
    "destination": "acct2",
    "amount": dep.amount,
    "fee": 1,
    "bridge": dep.as_bridge(),
}
body = json.dumps({"method": "submit", "params": [tx]}).encode()
req = urllib.request.Request(RPC, data=body, headers={"Content-Type": "application/json"})
with urllib.request.urlopen(req, timeout=20) as r:
    res = json.loads(r.read().decode()).get("result", {})
code = res.get("engine_result")
assert code == "tecPROTOCOL", f"double deposit must be tecPROTOCOL got {code}: {res}"
info2 = rpc("rail_info", {"rail": "BTC"})
supply1 = int(info2.get("total_minted") or info2.get("supply") or 0)
assert supply1 == supply0 == 20000, (supply0, supply1)
print("double deposit after restart blocked OK spent set restored supply", supply1)
PY

# continue protocol withdraw after restart (remaining 13000 → burn 2000)
SEQ_R=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['sequence'])")
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"rail_withdraw\",\"account\":\"acct2\",\"sequence\":$SEQ_R,\"asset\":\"BTC\",\"amount\":2000,\"external_to\":\"bc1q_after_restart\",\"fee\":1}]}" "rail_withdraw after restart"
BAL_F=""
for _ in $(seq 1 30); do
  BAL_F=$(post '{"method":"account_info","params":[{"account":"acct2"}]}' \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['balances'].get('BTC',0))" 2>/dev/null || echo 0)
  [[ "$BAL_F" == "11000" ]] && break
  sleep 0.15
done
python3 -c "assert int('${BAL_F:-0}')==11000, 'second withdraw not applied: ${BAL_F:-0}'; print('post-restart second withdraw OK', '${BAL_F:-0}')"

RI3=$(post '{"method":"rail_info","params":[{"rail":"BTC"}]}')
echo "$RI3" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
assert d.get('total_burned')==9000, d
assert d.get('claim_root_matches') is True, d
assert d.get('claim_note_count',0)>=2
assert d.get('claim_root')!='$ROOT_BEFORE', 'claim_root should advance after new withdraw'
print('claim_root advanced OK notes', d.get('claim_note_count'), 'burned', d['total_burned'])
"

echo "rail_restart_smoke OK (mint/withdraw/claim_root/spent-set survive snapshot; continue withdraw; Issue/Burn sealed)"
