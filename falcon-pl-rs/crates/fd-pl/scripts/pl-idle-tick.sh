#!/usr/bin/env bash
# Pre-public beta 2300 — one signed Pay every 3s (alice ↔ bob only).
# Fails over to a hub with tip_lag=0 so a lagging public RPC cannot
# black-hole work (AMD-20260827-1).
set -euo pipefail
ROOT="${PL_PUBLIC_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
CTL="$ROOT/bin/falcon-pl-ctl"
KEYS="$ROOT/keys"
LOG="$ROOT/logs/idle-tick.log"
# Comma-separated. First healthy (tip_lag=0) wins.
ADDRS="${PL_IDLE_RPC:-127.0.0.1:19301}"
SLEEP="${PL_IDLE_SLEEP:-3}"
mkdir -p "$(dirname "$LOG")"
from=alice
to=bob

pick_rpc() {
  local IFS=',' a lag
  for a in $ADDRS; do
    a="${a// /}"
    [[ -n "$a" ]] || continue
    lag=$("$CTL" --addr "$a" status 2>/dev/null | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(1)
b = d.get('body', d) if isinstance(d, dict) else {}
if not isinstance(b, dict) or 'tip_lag' not in b:
    b = d if isinstance(d, dict) else {}
print(int(b.get('tip_lag') or 0) if isinstance(b, dict) else 99)
" 2>/dev/null) || continue
    if [[ "$lag" == "0" ]]; then
      echo "$a"
      return 0
    fi
  done
  echo "${ADDRS%%,*}"
}

echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] idle-tick 1 tx / ${SLEEP}s $from↔$to nid=2300 rpcs=$ADDRS" | tee -a "$LOG"
while true; do
  addr=$(pick_rpc)
  if "$CTL" --addr "$addr" pay \
    --from "$from" --to "$to" --amount 1 --fee 2 \
    --scheme falcon-512 --keys-dir "$KEYS" --network-id 2300 \
    >>"$LOG" 2>&1; then
    :
  else
    echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] pay failed via $addr; retry" >>"$LOG"
  fi
  tmp=$from; from=$to; to=$tmp
  sleep "$SLEEP"
done
