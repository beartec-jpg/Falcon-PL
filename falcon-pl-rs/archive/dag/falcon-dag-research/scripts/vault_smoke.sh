#!/usr/bin/env bash
# Vault permanent destination lock — protocol-enforced.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC="${RPC:-127.0.0.1:18701}"
PORT="${RPC##*:}"
LOG="${LOG:-/tmp/fd-vault.log}"
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

submit_ok '{"method":"submit","params":[{"kind":"open_account_type","account":"acct1","sequence":1,"account_type":"vault","fee":10}]}' "open vault"
wait_exec

# send should fail (receive-only)
R=$(post '{"method":"submit","params":[{"kind":"payment","account":"acct1","sequence":2,"destination":"acct2","amount":1,"fee":1}]}' || true)
echo "$R" | python3 -c "
import sys,json
raw=sys.stdin.read().strip()
assert raw, 'empty'
code=json.loads(raw).get('result',{}).get('engine_result')
assert code is not None and str(code).lower()!='none', raw
assert code!='tesSUCCESS', code
print('vault open OK allowlist [] blocked payment', code)
"

submit_ok '{"method":"submit","params":[{"kind":"set_allowlist","account":"acct1","sequence":2,"destinations":["acct2"],"fee":10}]}' "set_allowlist"
wait_exec
submit_ok '{"method":"submit","params":[{"kind":"vault_lock","account":"acct1","sequence":3,"fee":10}]}' "vault_lock"
wait_exec

AI=$(post '{"method":"account_info","params":[{"account":"acct1"}]}')
echo "$AI" | python3 -c "
import sys,json
d=json.load(sys.stdin)['result']
assert d.get('hard_locked') is True
assert d.get('allowlist')==['acct2']
print('hard_locked OK', d.get('allowlist'))
"

# only acct2 allowed
submit_ok '{"method":"submit","params":[{"kind":"payment","account":"acct1","sequence":4,"destination":"acct2","amount":10,"fee":1}]}' "pay allowlist"
wait_exec
R=$(post '{"method":"submit","params":[{"kind":"payment","account":"acct1","sequence":5,"destination":"acct3","amount":1,"fee":1}]}' || true)
echo "$R" | python3 -c "
import sys,json
raw=sys.stdin.read().strip()
code=json.loads(raw).get('result',{}).get('engine_result') if raw else None
assert code!='tesSUCCESS', code
print('blocked drain to acct3 OK', code)
"

R=$(post '{"method":"submit","params":[{"kind":"set_allowlist","account":"acct1","sequence":5,"destinations":["acct3"],"fee":1}]}' || true)
echo "$R" | python3 -c "
import sys,json
raw=sys.stdin.read().strip()
code=json.loads(raw).get('result',{}).get('engine_result') if raw else None
assert code!='tesSUCCESS', code
print('allowlist immutable after lock')
"

echo "vault_smoke OK (receive-only → set dest → hard lock → only one destination)"
