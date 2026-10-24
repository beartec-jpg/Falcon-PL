#!/usr/bin/env bash
# Keep live-joined validators tip-synced during soak.
# If lag > MAX_LAG vs primary, re-bootstrap from primary snapshot and restart.
#
#   bash crates/fd-pl/scripts/pl-joiner-keep-sync.sh
set -euo pipefail

ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
BIN="${PL_BIN:-$HOME/falcon-pl/falcon-pl-rs/target/release}"
PRIMARY="${PL_PRIMARY:-127.0.0.1:19101}"
JOINERS_FILE="${PL_JOINERS:-$ROOT/run/joiners.txt}"
MAX_LAG="${PL_MAX_LAG:-5}"
INTERVAL="${PL_KEEP_SYNC_SEC:-30}"
PRIMARY_SNAP="${PL_PRIMARY_SNAP:-$ROOT/snapshots/v1.json}"
LOG="${PL_KEEP_SYNC_LOG:-$ROOT/logs/joiner-keep-sync.log}"

mkdir -p "$ROOT/logs" "$ROOT/run" "$ROOT/snapshots"
# Single instance
exec 9>"$ROOT/run/keep-sync.lock"
if ! flock -n 9; then
  echo "[keep-sync] another instance holds lock — exit"
  exit 0
fi
echo "[keep-sync] start max_lag=$MAX_LAG interval=${INTERVAL}s joiners=$JOINERS_FILE bin=$BIN" | tee -a "$LOG"

tip_primary() {
  local out
  out=$("$BIN/falcon-pl-ctl" --addr "$PRIMARY" status 2>/dev/null) || return 1
  printf '%s\n' "$out" | python3 -c "
import sys,json
r=sys.stdin.read()
i=r.find('{')
if i<0: raise SystemExit(1)
b=json.JSONDecoder().raw_decode(r[i:])[0]
body=b.get('body',b)
print(int(body.get('tip_height') or 0))
print((body.get('tip_hash') or '')[:16])
"
}

tip_joiner() {
  local host=$1 port=$2
  if [[ "$host" == "local" || "$host" == "host1" ]]; then
    "$BIN/falcon-pl-ctl" --addr "127.0.0.1:${port}" status 2>/dev/null
  else
    ssh -o BatchMode=yes -o ConnectTimeout=10 "$host" \
      "$ROOT/bin/falcon-pl-ctl --addr 127.0.0.1:${port} status" 2>/dev/null
  fi | python3 -c "
import sys,json
r=sys.stdin.read(); b=json.JSONDecoder().raw_decode(r[r.find('{'):])[0]
body=b.get('body',b)
print(int(body.get('tip_height') or 0))
print((body.get('tip_hash') or '')[:16])
"
}

resync_joiner() {
  local id=$1 host=$2 port=$3 peers=$4
  echo "[keep-sync] RESYNC $id on $host:$port from $PRIMARY_SNAP" | tee -a "$LOG"
  [[ -f "$PRIMARY_SNAP" ]] || { echo "no primary snap"; return 1; }
  cp -f "$PRIMARY_SNAP" "$ROOT/snapshots/${id}.json"
  python3 -c "
import json
p='$ROOT/snapshots/${id}.json'
d=json.load(open(p)); d['id']='$id'
json.dump(d, open(p,'w'), indent=2)
print('snap tip', d.get('tip_height'))
"
  if [[ "$host" != "local" && "$host" != "host1" ]]; then
    rsync -az "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$host:$ROOT/bin/"
    rsync -az "$ROOT/snapshots/${id}.json" "$host:$ROOT/snapshots/${id}.json"
    rsync -az "$ROOT/keys/" "$host:$ROOT/keys/" 2>/dev/null || true
    ssh -o BatchMode=yes "$host" bash -s <<EOF
set -e
ROOT="$ROOT"
for pid in \$(ps -eo pid,args | awk '/falcon-pl-node/ && /--id $id / {print \$1}'); do
  kill -9 \$pid 2>/dev/null || true
done
sleep 1
nohup \$ROOT/bin/falcon-pl-node --join --id $id --listen 0.0.0.0:$port \
  --peers "$peers" --validators v1,v2,v3,v4 --scheme hmac --keys-dir \$ROOT/keys \
  --load-wallets 32 --bond 1000 --network-id 2200 --seal-interval-ms 400 \
  --pack-budget-ms 1000 --progress-timeout-ms 1000 --max-mempool-txs 20000 \
  --epoch-ms 3600000 --unbond-cooldown-ms 86400000 --snapshot-interval-ms 60000 \
  --max-chain-ledgers 20000 \
  --snapshot \$ROOT/snapshots/${id}.json --snapshot-out \$ROOT/snapshots/${id}.json \
  >>\$ROOT/logs/${id}.log 2>&1 &
echo resync_pid=\$!
EOF
  fi
}

# Default peers for host2 joiners
DEFAULT_PEERS="${PL_JOIN_PEERS:-100.64.0.1:19101,100.64.0.1:19102,100.64.0.3:19111,100.64.0.5:19121}"

while true; do
  if [[ ! -f "$JOINERS_FILE" ]]; then
    sleep "$INTERVAL"
    continue
  fi
  mapfile -t pt < <(tip_primary 2>/dev/null || true)
  p_tip=${pt[0]:-0}
  p_hash=${pt[1]:-}
  ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  if [[ "$p_tip" -le 0 ]]; then
    echo "$ts primary unreachable — skip cycle" | tee -a "$LOG"
    sleep "$INTERVAL"
    continue
  fi
  while read -r id host port rest; do
    [[ -z "${id:-}" || "$id" == \#* ]] && continue
    peers="${rest:-$DEFAULT_PEERS}"
    mapfile -t jt < <(tip_joiner "$host" "$port" 2>/dev/null || echo -e "0\n")
    j_tip=${jt[0]:-0}
    j_hash=${jt[1]:-}
    lag=$((p_tip - j_tip))
    match=$([[ "$p_hash" == "$j_hash" && "$j_tip" -gt 0 ]] && echo Y || echo N)
    echo "$ts $id tip=$j_tip primary=$p_tip lag=$lag match=$match" | tee -a "$LOG"
    # Resync only when joiner is behind / dead / hash-mismatched with real lag.
    if [[ "$j_tip" -le 0 ]] || [[ "$lag" -gt "$MAX_LAG" ]] || [[ "$match" != "Y" && "$lag" -ge 2 ]]; then
      resync_joiner "$id" "$host" "$port" "$peers" || true
      sleep 3
    fi
  done <"$JOINERS_FILE"
  sleep "$INTERVAL"
done
