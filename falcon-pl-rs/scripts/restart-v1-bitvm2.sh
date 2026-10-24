#!/usr/bin/env bash
# Restart host1 v1 archive with BTC_EXIT_MODE=bitvm2 (run ON host1).
set -euo pipefail
PL="${PL_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
export BTC_EXIT_MODE=bitvm2
export FPL_BTC_MIN_CONFIRMATIONS=1
export PATH="${HOME}/.cargo/bin:${PATH}"

ln -sfn falcon-pl-node "$PL/bin/falcon-pl-archive"

# Kill any running v1 archive (pid file may be stale after manual restarts).
while read -r pid; do
  kill "$pid" 2>/dev/null || true
done < <(pgrep -f 'falcon-pl-archive --id v1 ' || true)
sleep 2
while read -r pid; do
  kill -0 "$pid" 2>/dev/null && kill -TERM "$pid" 2>/dev/null || true
done < <(pgrep -f 'falcon-pl-archive --id v1 ' || true)
sleep 1
rm -f "$PL/run/v1.admin.sock"

PEERS="100.64.0.1:19301,100.64.0.2:19302,100.64.0.3:19303,100.64.0.4:19304,100.64.0.5:19305,100.64.0.6:19306,100.64.0.7:19307"
HUBS="100.64.0.1:19301,100.64.0.2:19302,100.64.0.4:19304,100.64.0.3:19303"

cd "$PL"
nohup env BTC_EXIT_MODE=bitvm2 ./bin/falcon-pl-archive \
  --id v1 --hub --archive --listen 0.0.0.0:19301 \
  --peers "$PEERS" --hubs "$HUBS" \
  --keys-dir "$PL/keys" --max-chain-ledgers 0 \
  --admin-sock "$PL/run/v1.admin.sock" \
  --snapshot "$PL/snapshots/v1.json" --snapshot-out "$PL/snapshots/v1.json" \
  --validators v1,v2,v3,v4,v5 --scheme falcon-512 --load-wallets 0 --bond 1000 \
  --network-id 2300 --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
  --max-txs 128 --max-mempool-txs 50000 --epoch-ms 604800000 --first-claim-epoch 1 \
  --unbond-cooldown-ms 1209600000 --snapshot-interval-ms 60000 \
  --online-epoch-heights 10 --lock-public-wire \
  >>"$PL/logs/v1.log" 2>&1 &
echo $! >"$PL/run/v1.pid"
echo "v1 restarted pid=$(cat "$PL/run/v1.pid") BTC_EXIT_MODE=$BTC_EXIT_MODE"
sleep 3
"$PL/bin/falcon-pl-ctl" --addr 127.0.0.1:19301 status | python3 -c 'import sys,json; b=json.load(sys.stdin).get("body",{}); print("btc_exit_mode:", b.get("btc_exit_mode"))'
