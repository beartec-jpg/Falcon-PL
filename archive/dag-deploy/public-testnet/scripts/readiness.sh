#!/usr/bin/env bash
# Gate: is falcon-dag-testnet-1 good enough to call a public play testnet?
# Exit 0 = READY, 1 = NOT READY
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
GMP="$ROOT/deploy/gmp"
PT="$ROOT/deploy/public-testnet"
MESH="${MESH_ROOT:-$HOME/falcon-dag-mesh}"
FACADE="${FACADE_URL:-http://127.0.0.1:18499}"
FAUCET="${FAUCET_URL:-http://127.0.0.1:18550}"

PASS=0
FAIL=0
warn() { echo "  WARN  $*"; }
ok()   { echo "  OK    $*"; PASS=$((PASS+1)); }
bad()  { echo "  FAIL  $*"; FAIL=$((FAIL+1)); }

echo "=== falcon-dag-testnet-1 readiness ==="

# 1. Genesis package
if [[ -f "$PT/genesis/network.json" && -f "$PT/genesis/unl_public.json" ]]; then
  ok "genesis package present"
else
  bad "genesis package missing"
fi

# 2. Health
set +e
python3 "$GMP/health.py" >"$MESH/stack/last_health.json" 2>"$MESH/stack/last_health.err"
HC=$?
set -e
if [[ "$HC" -eq 0 ]]; then
  ok "health OK (full mesh)"
elif [[ "$HC" -eq 1 ]]; then
  warn "health DEGRADED (acceptable for soft-ready)"
  PASS=$((PASS+1))
else
  bad "health DOWN (exit $HC)"
fi

# 3. Smoke
set +e
python3 "$GMP/smoke.py" >"$MESH/stack/last_smoke.out" 2>"$MESH/stack/last_smoke.err"
SC=$?
set -e
if [[ "$SC" -eq 0 ]]; then
  ok "smoke PASS (same-shard + xshard + idempotent)"
else
  bad "smoke FAIL"
fi

# 4. Facade
if curl -sf --max-time 4 "$FACADE/v1/status" >/dev/null; then
  ok "facade /v1/status"
else
  bad "facade unreachable"
fi

# 5. Faucet
if curl -sf --max-time 4 "$FAUCET/v1/health" >/dev/null; then
  ok "faucet /v1/health"
  # live drip to a lab account (small)
  DRIP=$(curl -sf --max-time 20 -X POST "$FAUCET/v1/drip" \
    -H 'Content-Type: application/json' \
    -d '{"destination":"acct7","amount":1}' || true)
  if echo "$DRIP" | python3 -c "import sys,json; d=json.load(sys.stdin); sys.exit(0 if d.get('ok') else 1)" 2>/dev/null; then
    ok "faucet drip"
  else
    bad "faucet drip failed: $DRIP"
  fi
else
  bad "faucet unreachable at $FAUCET"
fi

# 6. Falcon-512 / network_id
NID=$(curl -sf --max-time 4 -X POST http://127.0.0.1:18001/ \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"server_info","params":[{}]}' \
  | python3 -c "import sys,json; m=((json.load(sys.stdin).get('result')or{}).get('info')or{}).get('metrics')or{}; print(m.get('network_id','')); print(m.get('build_version',''), file=sys.stderr)" 2>/tmp/bv.err)
BV=$(cat /tmp/bv.err 2>/dev/null || true)
if [[ "$NID" == "9003" || "$NID" == "9930" ]]; then
  ok "network_id=$NID build=$BV"
else
  bad "unexpected network_id='$NID' (want 9003 public or 9930 lab)"
fi

# 7. Watchdog
if [[ -f "$MESH/logs/watchdog.pid" ]] && [[ -d "/proc/$(cat "$MESH/logs/watchdog.pid" 2>/dev/null || echo 0)" ]]; then
  ok "watchdog alive"
else
  warn "watchdog not running (start with watchdog.sh --daemon)"
fi

# 8. Docs
if [[ -f "$PT/PUBLIC_TESTNET.md" ]]; then
  ok "PUBLIC_TESTNET.md present"
else
  bad "PUBLIC_TESTNET.md missing"
fi

# 9. Dual shard evidence
SHARDS=$(curl -sf --max-time 4 "$FACADE/v1/status" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('shards')or[]))" 2>/dev/null || echo 0)
if [[ "${SHARDS:-0}" -ge 2 ]]; then
  ok "facade shards=$SHARDS"
else
  bad "facade shards=$SHARDS (need >=2)"
fi

echo
echo "passed=$PASS failed=$FAIL"
if [[ "$FAIL" -eq 0 ]]; then
  echo "READY: falcon-dag-testnet-1 is good enough for public play testnet (R2)"
  exit 0
fi
echo "NOT READY: fix failures above"
exit 1
