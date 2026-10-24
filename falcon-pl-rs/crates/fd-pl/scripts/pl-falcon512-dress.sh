#!/usr/bin/env bash
# Falcon-512 private dress rehearsal (short): keys + 3-node + one pay.
#
#   bash crates/fd-pl/scripts/pl-falcon512-dress.sh
set -euo pipefail

RS="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-v22-falcon}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
SCHEME=falcon-512
LOAD=4
NETWORK_ID=2200

mkdir -p "$ROOT" "$KEYS" "$LOG" "$SNAP"
cd "$RS"
cargo build -p fd-pl --release 2>&1 | tail -4

echo "[f512] gen keys (validators + $LOAD wallets) — may take a bit…"
rm -rf "$KEYS"
mkdir -p "$KEYS"
"$BIN/falcon-pl-ctl" gen-keys \
  --keys-dir "$KEYS" \
  --scheme "$SCHEME" \
  --validators v1,v2,v3 \
  --load-wallets "$LOAD"
"$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account faucet --scheme "$SCHEME"

for p in 19101 19102 19103; do fuser -k "${p}/tcp" 2>/dev/null || true; done
sleep 0.5
common=(
  --validators v1,v2,v3 --scheme "$SCHEME" --keys-dir "$KEYS"
  --load-wallets "$LOAD" --bond 1000 --network-id "$NETWORK_ID"
  --epoch-ms 10000 --seal-interval-ms 500 --progress-timeout-ms 1000
  --max-mempool-txs 50000
)
nohup "$BIN/falcon-pl-node" --id v1 --listen 0.0.0.0:19101 \
  --peers 127.0.0.1:19102,127.0.0.1:19103 "${common[@]}" \
  --snapshot-out "$SNAP/v1.json" >>"$LOG/v1.log" 2>&1 &
echo $! >"$LOG/v1.pid"
nohup "$BIN/falcon-pl-node" --id v2 --listen 0.0.0.0:19102 \
  --peers 127.0.0.1:19101,127.0.0.1:19103 "${common[@]}" \
  --snapshot-out "$SNAP/v2.json" >>"$LOG/v2.log" 2>&1 &
echo $! >"$LOG/v2.pid"
nohup "$BIN/falcon-pl-node" --id v3 --listen 0.0.0.0:19103 \
  --peers 127.0.0.1:19101,127.0.0.1:19102 "${common[@]}" \
  --snapshot-out "$SNAP/v3.json" >>"$LOG/v3.log" 2>&1 &
echo $! >"$LOG/v3.pid"
sleep 2

echo "[f512] status scheme"
"$BIN/falcon-pl-ctl" --addr 127.0.0.1:19101 status | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
print('scheme', body.get('scheme'), 'network', body.get('network'), 'id', body.get('id'))
assert 'falcon' in (body.get('scheme') or '').lower()
"
echo "[f512] pay alice→bob (Falcon-512 sig)"
"$BIN/falcon-pl-ctl" --addr 127.0.0.1:19101 pay \
  --from alice --to bob --amount 5 --fee 2 --sequence 0 \
  --scheme "$SCHEME" --keys-dir "$KEYS" --network-id "$NETWORK_ID"
sleep 2
"$BIN/falcon-pl-ctl" --addr 127.0.0.1:19101 account --account bob

for f in "$LOG"/v{1,2,3}.pid; do
  [[ -f "$f" ]] && kill "$(cat "$f")" 2>/dev/null || true
  rm -f "$f"
done
for p in 19101 19102 19103; do fuser -k "${p}/tcp" 2>/dev/null || true; done
echo "[f512] OK dress rehearsal"
