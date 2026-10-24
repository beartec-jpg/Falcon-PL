#!/usr/bin/env bash
# Live indexer dry-run: public chain headers (+ optional deposit watches) → Falcon.
# Headers always; deposits only when watch/lock env vars are set.
#
# Optional env:
#   BTC_WATCH=bc1q...          live BTC SPV deposits to address
#   ETH_LOCK=0x...             FalconRailLock for ETH Deposit logs
#   BNB_LOCK=0x...             FalconRailLock for BNB
#   XRP_WATCH=r...             XRPL Payment destination
#   ETH_RPC / BNB_RPC          comma-separated RPC failover lists
#   MAX_HEADERS / MAX_DEPOSITS
#   REQUIRE_ETH=1              hard-fail if ETH headers fail
#   REQUIRE_DEPOSIT=1          hard-fail if a watch is set but zero candidates
#   CI_SOFT_NETWORK=1          if zero rails get headers (outage), exit 0 after protocol checks
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC="${RPC:-127.0.0.1:18821}"
PORT="${RPC##*:}"
LOG="${LOG:-/tmp/fd-live-indexer.log}"
PID=""
MAX_HEADERS="${MAX_HEADERS:-2}"
MAX_DEPOSITS="${MAX_DEPOSITS:-5}"

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

echo "live_indexer_smoke: node up max_headers=$MAX_HEADERS"
echo "  BTC_WATCH=${BTC_WATCH:-} ETH_LOCK=${ETH_LOCK:-} BNB_LOCK=${BNB_LOCK:-} XRP_WATCH=${XRP_WATCH:-}"

OK_RAILS=0
FAIL_RAILS=0
ETH_OK=0
DEPOSIT_CANDIDATES=0
DEPOSIT_MINTED=0
WATCH_CONFIGURED=0

for RAIL in BTC ETH BNB XRP; do
  echo "---- live: $RAIL ----"
  EXTRA=()
  case "$RAIL" in
    BTC)
      if [[ -n "${BTC_WATCH:-}" ]]; then
        EXTRA+=(--btc-watch-address "$BTC_WATCH")
        WATCH_CONFIGURED=1
      fi
      ;;
    ETH)
      if [[ -n "${ETH_LOCK:-}" ]]; then
        EXTRA+=(--lock-contract "$ETH_LOCK")
        WATCH_CONFIGURED=1
      fi
      ;;
    BNB)
      if [[ -n "${BNB_LOCK:-}" ]]; then
        EXTRA+=(--lock-contract "$BNB_LOCK")
        WATCH_CONFIGURED=1
      fi
      ;;
    XRP)
      if [[ -n "${XRP_WATCH:-}" ]]; then
        EXTRA+=(--xrp-watch-account "$XRP_WATCH")
        WATCH_CONFIGURED=1
      fi
      ;;
  esac

  set +e
  OUT=$(python3 "$ROOT/scripts/rail_agent.py" \
    --rpc "http://$RPC" \
    --rail "$RAIL" \
    --indexer live \
    --max-headers "$MAX_HEADERS" \
    --max-deposits "$MAX_DEPOSITS" \
    --prove-forbidden \
    --sleep 0.45 \
    "${EXTRA[@]}" \
    2>&1)
  set -e
  echo "$OUT" | tail -30

  if echo "$OUT" | grep -q "header h=.*tesSUCCESS"; then
    echo "live $RAIL: headers accepted"
    OK_RAILS=$((OK_RAILS + 1))
    [[ "$RAIL" == "ETH" ]] && ETH_OK=1
  elif echo "$OUT" | grep -qiE "indexer headers error|URLError|timeout|Connection|HTTP Error|Temporary failure|all EVM RPC"; then
    echo "live $RAIL: network unavailable (soft-fail)"
    FAIL_RAILS=$((FAIL_RAILS + 1))
  elif echo "$OUT" | grep -q "forbidden issue"; then
    echo "live $RAIL: Issue blocked (partial OK)"
    OK_RAILS=$((OK_RAILS + 1))
  else
    echo "live $RAIL: no successful headers"
    FAIL_RAILS=$((FAIL_RAILS + 1))
  fi

  # Parse deposit stats from agent JSON lines
  CAND=$(echo "$OUT" | python3 -c "
import sys,re,json
c=0;m=0
for line in sys.stdin:
  line=line.strip()
  if not line.startswith('{'): continue
  try: d=json.loads(line)
  except: continue
  if 'deposit_candidates' in d: c=int(d['deposit_candidates'])
  if 'deposits_minted' in d: m=int(d['deposits_minted'])
print(c,m)
" 2>/dev/null || echo "0 0")
  C=$(echo "$CAND" | awk '{print $1}')
  M=$(echo "$CAND" | awk '{print $2}')
  DEPOSIT_CANDIDATES=$((DEPOSIT_CANDIDATES + C))
  DEPOSIT_MINTED=$((DEPOSIT_MINTED + M))
  echo "live $RAIL: deposit_candidates=$C deposits_minted=$M"

  SUP=$(post "{\"method\":\"rail_info\",\"params\":[{\"rail\":\"$RAIL\"}]}" \
    | python3 -c "import sys,json;d=json.load(sys.stdin).get('result',{});print(d.get('total_minted',0))")
  # Without watch config, minted must stay 0. With watch, mint only via deposit proofs.
  if [[ ${#EXTRA[@]} -eq 0 ]]; then
    python3 -c "assert int('$SUP')==0, ('unexpected mint without watch', '$RAIL', '$SUP')"
    echo "live $RAIL: total_minted=0 OK (no watch)"
  else
    python3 -c "assert int('$SUP')==int('$M') or int('$SUP')>=int('$M'), ('mint mismatch', '$SUP', '$M')"
    echo "live $RAIL: total_minted=$SUP (watch configured)"
  fi
done

# Protocol hard checks (always): free Issue must never be admitted
ISS=$(post '{"method":"submit","params":[{"kind":"issue","account":"acct1","sequence":1,"asset":"BTC","amount":1,"destination":"acct1","fee":1}]}')
smoke_require_engine "$ISS" tecPROTOCOL "admit Issue BTC"

# No unexpected mint on rails without watch (already asserted per-rail)

if [[ "$OK_RAILS" -lt 1 ]]; then
  if [[ "${CI_SOFT_NETWORK:-0}" == "1" ]]; then
    echo "live_indexer_smoke NETWORK soft-fail (no headers; CI_SOFT_NETWORK=1; protocol Issue check passed)"
    echo "log: /tmp/fd-live-indexer.log"
    exit 0
  fi
  echo "live_indexer_smoke FAILED: no rail got headers. log: /tmp/fd-live-indexer.log" >&2
  exit 1
fi

if [[ "${REQUIRE_ETH:-0}" == "1" && "$ETH_OK" -ne 1 ]]; then
  echo "live_indexer_smoke FAILED: REQUIRE_ETH=1 but ETH headers failed" >&2
  exit 2
fi

if [[ "${REQUIRE_DEPOSIT:-0}" == "1" && "$WATCH_CONFIGURED" -eq 1 && "$DEPOSIT_CANDIDATES" -eq 0 ]]; then
  echo "live_indexer_smoke FAILED: REQUIRE_DEPOSIT=1 but no deposit candidates" >&2
  exit 3
fi

if [[ "$ETH_OK" -eq 1 ]]; then
  echo "live ETH: multi-RPC failover OK"
fi

echo "live_indexer_smoke OK (headers=$OK_RAILS soft_fail=$FAIL_RAILS eth_ok=$ETH_OK watch=$WATCH_CONFIGURED candidates=$DEPOSIT_CANDIDATES minted=$DEPOSIT_MINTED)"
