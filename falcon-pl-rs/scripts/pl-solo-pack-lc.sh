#!/usr/bin/env bash
# Brief solo v1 window: empty mempool, submit LC finality, seal until advanced.
# Run ON host1. Restores full mesh peers on exit.
set -euo pipefail
PL="${PL_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
CTL="$PL/bin/falcon-pl-ctl"
RPC="${FPL_RPC:-127.0.0.1:19301}"
ADMIN="$PL/run/v1.admin.sock"
export BTC_EXIT_MODE=bitvm2
export PL_ROOT="$PL"
export FPL_RPC="$RPC"
export FALCON_QC_BRIDGE="${FALCON_QC_BRIDGE:-0x0AaB7a532707DA131a0b78B1e6eb5Ab300677B5f}"

cleanup() {
  echo "==> restore full mesh"
  bash "$PL/scripts/restart-v1-bitvm2.sh" 2>&1 | tail -2 || true
}
trap cleanup EXIT

echo "==> stop v1"
while read -r pid; do kill "$pid" 2>/dev/null || true; done < <(pgrep -f 'falcon-pl-archive --id v1 ' || true)
sleep 2
rm -f "$ADMIN"

echo "==> start solo v1 (localhost only — no inbound mesh)"
cd "$PL"
nohup env BTC_EXIT_MODE=bitvm2 ./bin/falcon-pl-archive \
  --id v1 --hub --archive --listen 127.0.0.1:19301 \
  --peers 127.0.0.1:19999 --hubs 127.0.0.1:19999 \
  --keys-dir "$PL/keys" --max-chain-ledgers 0 \
  --admin-sock "$ADMIN" \
  --snapshot "$PL/snapshots/v1.json" --snapshot-out "$PL/snapshots/v1.json" \
  --validators v1,v2,v3,v4,v5 --scheme falcon-512 --load-wallets 0 --bond 1000 \
  --network-id 2300 --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
  --max-txs 128 --max-mempool-txs 50000 --epoch-ms 604800000 --first-claim-epoch 1 \
  --unbond-cooldown-ms 1209600000 --snapshot-interval-ms 60000 \
  --online-epoch-heights 10 --lock-public-wire \
  >>"$PL/logs/v1-solo.log" 2>&1 &
echo $! >"$PL/run/v1.pid"
sleep 8

"$CTL" --addr "$RPC" status > /tmp/solo-st.json
python3 - <<'PY'
import json
b = json.load(open("/tmp/solo-st.json")).get("body", {})
elc = b.get("eth_lc") or {}
print(f"solo start mempool={b.get('mempool')} tip={b.get('tip_height')} fin={elc.get('finalized_slot')}")
PY

echo "==> submit LC finality update"
python3 "$PL/scripts/pl-eth-lc-relayer.py" update 2>&1 | tail -5 || true

echo "==> seal loop (up to 2 min)"
for i in $(seq 1 40); do
  out=$("$CTL" --admin-sock "$ADMIN" seal --force 2>&1 || true)
  fin=$("$CTL" --addr "$RPC" status 2>/dev/null | python3 -c "import sys,json; b=(json.load(sys.stdin).get('body') or json.load(sys.stdin)); print((b.get('eth_lc') or {}).get('finalized_slot',0))" 2>/dev/null || echo 0)
  exec_n=$("$CTL" --addr "$RPC" status 2>/dev/null | python3 -c "import sys,json; b=(json.load(sys.stdin).get('body') or json.load(sys.stdin)); print((b.get('eth_lc') or {}).get('execution_number',0))" 2>/dev/null || echo 0)
  mp=$("$CTL" --addr "$RPC" status 2>/dev/null | python3 -c "import sys,json; b=(json.load(sys.stdin).get('body') or json.load(sys.stdin)); print(b.get('mempool',0))" 2>/dev/null || echo 0)
  echo "  try=$i fin=$fin exec=$exec_n mempool=$mp seal=${out:0:80}"
  if [ "$fin" != "11025216" ] && [ "$fin" != "0" ]; then
    echo "==> LC finalized_slot advanced to $fin"
    break
  fi
  sleep 3
done

"$CTL" --addr "$RPC" status > /tmp/solo-st.json
python3 - <<'PY'
import json
b = json.load(open("/tmp/solo-st.json")).get("body", {})
elc = b.get("eth_lc") or {}
print(f"solo done mempool={b.get('mempool')} tip={b.get('tip_height')} exec={elc.get('execution_number')} fin={elc.get('finalized_slot')}")
PY
