#!/usr/bin/env bash
# Resync host1 v1 archive when catch-up conflict / state root mismatch blocks SubmitTx.
# Run ON host1 after backing up snapshot.  Does NOT use admin rails for user flows.
set -euo pipefail
PL="${PL_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
CTL="$PL/bin/falcon-pl-ctl"
ADMIN="$PL/run/v1.admin.sock"
RPC="${FPL_RPC:-127.0.0.1:19301}"

echo "==> status before"
"$CTL" --addr "$RPC" status 2>/dev/null | python3 -c "
import sys,json
b=(json.load(sys.stdin).get('body') or json.load(sys.stdin))
print('tip', b.get('tip_height'), 'peer', b.get('best_peer_tip'))
print('conflict', (b.get('last_catchup_conflict') or '')[:100])
" || echo "(v1 not reachable)"

echo "==> backup snapshot"
ts=$(date +%Y%m%d-%H%M%S)
cp -a "$PL/snapshots/v1.json" "$PL/snapshots/v1.json.bak-$ts" 2>/dev/null || true

echo "==> operator hard-reset (arm catch-up from genesis)"
"$CTL" --addr "$RPC" --admin-sock "$ADMIN" hard-reset \
  --reason "bitvm2 production resync $ts"

echo "==> restart v1"
bash "$(dirname "$0")/restart-v1-bitvm2.sh"

echo "==> wait for catch-up (up to 10 min)"
for i in $(seq 1 60); do
  if out=$("$CTL" --addr "$RPC" status 2>/dev/null); then
    python3 - <<PY "$out"
import json,sys
b=json.loads(sys.argv[1]).get('body') or json.loads(sys.argv[1])
tip=b.get('tip_height'); peer=b.get('best_peer_tip'); lag=(peer or 0)- (tip or 0)
conf=(b.get('last_catchup_conflict') or '')[:80]
print(f'  try {tip} peer {peer} lag {lag} conflict={conf!r}')
if lag <= 2 and 'state root mismatch' not in (b.get('last_catchup_conflict') or ''):
    sys.exit(0)
PY
    if [[ $? -eq 0 ]]; then
      echo "==> v1 caught up"
      exit 0
    fi
  fi
  sleep 10
done

echo "WARN: v1 still lagging or conflict — inspect $PL/logs/v1.log"
exit 1
