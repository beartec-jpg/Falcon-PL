#!/usr/bin/env bash
# Wipe stale v1 snap/bodies and resync from canonical mesh (run ON host1).
set -euo pipefail
PL="${PL_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
CTL="$PL/bin/falcon-pl-ctl"
RPC="127.0.0.1:19301"
ADMIN="$PL/run/v1.admin.sock"
SNAP_BASE="${PL}/snapshots/v3.from-v3.json"
ts=$(date +%Y%m%d-%H%M%S)

echo "==> stop v1"
while read -r pid; do kill "$pid" 2>/dev/null || true; done < <(pgrep -f 'falcon-pl-archive --id v1 ' || true)
sleep 2
rm -f "$ADMIN"

echo "==> archive stale v1 snapshot + bodies"
mkdir -p "$PL/snapshots/stale-$ts"
for f in v1.json v1.join.json v1.join.serve.json v1.pull.json; do
  mv "$PL/snapshots/$f" "$PL/snapshots/stale-$ts/" 2>/dev/null || true
done
mv "$PL/snapshots/bodies/v1" "$PL/snapshots/stale-$ts/bodies-v1" 2>/dev/null || true
mkdir -p "$PL/snapshots/bodies/v1"

echo "==> phase 1: hub sync (no --archive) from canonical v3 snap"
export BTC_EXIT_MODE=bitvm2
export FPL_BTC_MIN_CONFIRMATIONS=1
PEERS="100.64.0.1:19301,100.64.0.2:19302,100.64.0.3:19303,100.64.0.4:19304,100.64.0.5:19305,100.64.0.6:19306,100.64.0.7:19307"
HUBS="100.64.0.1:19301,100.64.0.2:19302,100.64.0.4:19304,100.64.0.3:19303"
cd "$PL"
nohup env BTC_EXIT_MODE=bitvm2 ./bin/falcon-pl-archive \
  --id v1 --hub --listen 0.0.0.0:19301 \
  --peers "$PEERS" --hubs "$HUBS" \
  --keys-dir "$PL/keys" --max-chain-ledgers 0 \
  --admin-sock "$ADMIN" \
  --snapshot "$PL/snapshots/v1.json" --snapshot-out "$PL/snapshots/v1.json" \
  --validators v1,v2,v3,v4,v5 --scheme falcon-512 --load-wallets 0 --bond 1000 \
  --network-id 2300 --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
  --max-txs 128 --max-mempool-txs 50000 --epoch-ms 604800000 --first-claim-epoch 1 \
  --unbond-cooldown-ms 1209600000 --snapshot-interval-ms 60000 \
  --online-epoch-heights 10 --lock-public-wire \
  >>"$PL/logs/v1.log" 2>&1 &
echo $! >"$PL/run/v1.pid"
sleep 4

if [[ ! -f "$SNAP_BASE" ]]; then
  echo "WARN: $SNAP_BASE missing — joining from genesis via mesh snap"
else
  echo "==> snapshot-load $SNAP_BASE"
  "$CTL" --addr "$RPC" --admin-sock "$ADMIN" snapshot-load --path "$SNAP_BASE"
fi

echo "==> wait for catch-up (up to 15 min)"
for i in $(seq 1 90); do
  if "$CTL" --addr "$RPC" status > /tmp/v1-status.json 2>/dev/null; then
    if python3 - <<'PY'
import json, sys
b = json.load(open("/tmp/v1-status.json")).get("body") or json.load(open("/tmp/v1-status.json"))
tip = b.get("tip_height") or 0
peer = b.get("best_peer_tip") or 0
lag = peer - tip
conf = (b.get("last_catchup_conflict") or "")[:80]
print(f"  tip={tip} peer={peer} lag={lag} conflict={conf!r}")
if lag <= 3 and "state root mismatch" not in (b.get("last_catchup_conflict") or ""):
    sys.exit(0)
sys.exit(1)
PY
    then
      echo "==> synced — promoting to archive"
      bash "$PL/scripts/restart-v1-bitvm2.sh" 2>&1 | tail -3
      "$CTL" --addr "$RPC" status | python3 -c 'import sys,json; b=json.load(sys.stdin).get("body",{}); print("final tip",b.get("tip_height"),"peer",b.get("best_peer_tip"),"mode",b.get("btc_exit_mode"))'
      exit 0
    fi
  fi
  sleep 10
done

echo "WARN: v1 still lagging — inspect $PL/logs/v1.log"
exit 1
