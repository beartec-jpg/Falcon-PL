#!/usr/bin/env bash
# Falcon PL v2.2.4 testnet restart — hub mempool + light vals (GROWTH_AND_MEMPOOL).
#
# Topology:
#   v1 host1 :19101  hub+archive
#   v2 host1 :19102  hub
#   v3 host2 :19111  hub
#   v4..v7 light vals (f1, f2, droplet, hil) via live bond-in after seed packs
#
#   bash crates/fd-pl/scripts/pl-restart-v224.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
BIN="$RS/target/release"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
RUN="$ROOT/run"
SCHEME=hmac
NETWORK_ID=2200
BOND=1000
LOAD_WALLETS=32
PROGRESS=2500
SEAL=400
PACK=1000

SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)

HUBS="100.64.0.1:19101,100.64.0.1:19102,100.64.0.3:19111"
SEED_VALS="v1,v2,v3"

mkdir -p "$ROOT"/{bin,keys,logs,snapshots,run}

log() { echo "[v224] $*"; }

stop_all() {
  log "stop all nodes + load"
  ps -eo pid,cmd | awk '/falcon-pl-load-net/ && !/awk/ {print $1}' | while read -r p; do kill "$p" 2>/dev/null || true; done
  pkill -x falcon-pl-node 2>/dev/null || true
  for h in host2 host3; do
    ssh "${SSH_OPTS[@]}" "$h" 'pkill -x falcon-pl-node 2>/dev/null||true; for p in $(seq 19101 19170); do fuser -k ${p}/tcp 2>/dev/null; done; true' 2>/dev/null || true
  done
  for t in root@seat-host-4 root@seat-host-5 root@seat-host-6 root@seat-host-7; do
    timeout 20 tailscale ssh "$t" 'pkill -x falcon-pl-node 2>/dev/null||true; for p in $(seq 19101 19170); do fuser -k ${p}/tcp 2>/dev/null; done; true' 2>/dev/null || true
  done
  for p in $(seq 19101 19170); do fuser -k "${p}/tcp" 2>/dev/null || true; done
  sleep 1
}

build() {
  cd "$RS"
  cargo build -p fd-pl --release 2>&1 | tail -6
  cp -f "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$ROOT/bin/"
  chmod +x "$ROOT/bin/"*
}

genesis() {
  rm -rf "$KEYS" "$SNAP"
  # Stale load-seq from prior fleets submits future sequences → seq-poison freeze.
  rm -f "$RUN/load-seq.json"
  mkdir -p "$KEYS" "$SNAP" "$LOG" "$RUN"
  # Pre-key seed + planned light joins
  "$ROOT/bin/falcon-pl-ctl" gen-keys \
    --keys-dir "$KEYS" --scheme "$SCHEME" \
    --validators "v1,v2,v3,v4,v5,v6,v7" \
    --load-wallets "$LOAD_WALLETS"
  "$ROOT/bin/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account faucet --scheme "$SCHEME" || true
  echo "$SEED_VALS" >"$RUN/validators.txt"
  cat >"$RUN/live-mesh.txt" <<EOF
v1 100.64.0.1 19101
v2 100.64.0.1 19102
v3 100.64.0.3 19111
EOF
  cat >"$RUN/layout.txt" <<EOF
v1 100.64.0.1 19101 1 f1 local $ROOT
v2 100.64.0.1 19102 0 f1 local $ROOT
v3 100.64.0.3 19111 0 f2 ssh:host2 $ROOT
v4 100.64.0.5 19121 0 f3 ssh:host3 $ROOT
v5 100.64.0.3 19112 0 f2 ssh:host2 $ROOT
v6 100.64.0.1 19103 0 f1 local $ROOT
v7 100.64.0.2 19131 0 hil ts:root@seat-host-4 /root/falcon-pl-private-testnet-v22
EOF
  log "genesis keys + layout ready"
}

deploy_remote() {
  local how="$1" rroot="$2"
  case "$how" in
    ssh:*)
      local h="${how#ssh:}"
      ssh "${SSH_OPTS[@]}" "$h" "mkdir -p $rroot/{bin,keys,logs,snapshots,run}"
      rsync -az -e "ssh ${SSH_OPTS[*]}" "$ROOT/bin/" "$h:$rroot/bin/"
      rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$h:$rroot/keys/"
      ;;
    ts:*)
      local th="${how#ts:}"
      timeout 40 tailscale ssh "$th" "mkdir -p $rroot/{bin,keys,logs,snapshots,run}"
      tar -C "$ROOT/bin" -czf - falcon-pl-node falcon-pl-ctl \
        | timeout 90 tailscale ssh "$th" "tar -C $rroot/bin -xzf - && chmod +x $rroot/bin/*"
      tar -C "$ROOT" -czf - keys \
        | timeout 90 tailscale ssh "$th" "cd $rroot && rm -rf keys && tar -xzf -"
      ;;
  esac
}

start_seed() {
  local common=(
    --validators "$SEED_VALS" --scheme "$SCHEME" --keys-dir "$KEYS"
    --load-wallets "$LOAD_WALLETS" --bond "$BOND" --network-id "$NETWORK_ID"
    --seal-interval-ms "$SEAL" --pack-budget-ms "$PACK"
    --progress-timeout-ms "$PROGRESS" --max-mempool-txs 20000
    --epoch-ms 3600000 --unbond-cooldown-ms 86400000
    --snapshot-interval-ms 60000 --hubs "$HUBS"
  )

  # v1 hub archive
  rm -f "$SNAP/v1.json"
  nohup "$ROOT/bin/falcon-pl-node" --id v1 --hub --listen 0.0.0.0:19101 \
    --peers "100.64.0.1:19102,100.64.0.3:19111" \
    --max-chain-ledgers 0 \
    --snapshot "$SNAP/v1.json" --snapshot-out "$SNAP/v1.json" \
    "${common[@]}" >>"$LOG/v1.log" 2>&1 &
  echo $! >>"$RUN/seed.pids"
  log "start v1 hub+archive pid=$!"

  # v2 hub
  rm -f "$SNAP/v2.json"
  nohup "$ROOT/bin/falcon-pl-node" --id v2 --hub --listen 0.0.0.0:19102 \
    --peers "100.64.0.1:19101,100.64.0.3:19111" \
    --max-chain-ledgers 20000 \
    --snapshot "$SNAP/v2.json" --snapshot-out "$SNAP/v2.json" \
    "${common[@]}" >>"$LOG/v2.log" 2>&1 &
  echo $! >>"$RUN/seed.pids"
  log "start v2 hub pid=$!"

  # v3 hub remote
  deploy_remote "ssh:host2" "$ROOT"
  ssh "${SSH_OPTS[@]}" host2 bash -s <<EOF
set -e
R=$ROOT
rm -f \$R/snapshots/v3.json
nohup \$R/bin/falcon-pl-node --id v3 --hub --listen 0.0.0.0:19111 \
  --peers "100.64.0.1:19101,100.64.0.1:19102" \
  --validators $SEED_VALS --scheme $SCHEME --keys-dir \$R/keys \
  --load-wallets $LOAD_WALLETS --bond $BOND --network-id $NETWORK_ID \
  --seal-interval-ms $SEAL --pack-budget-ms $PACK --progress-timeout-ms $PROGRESS \
  --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
  --snapshot-interval-ms 60000 --max-chain-ledgers 20000 --hubs "$HUBS" \
  --snapshot \$R/snapshots/v3.json --snapshot-out \$R/snapshots/v3.json \
  >>\$R/logs/v3.log 2>&1 &
echo \$! > \$R/run/v3.pid
echo start v3 hub pid=\$!
EOF
}

light_load() {
  local n="${1:-15}" i
  for ((i = 0; i < n; i++)); do
    "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 pay --from alice --to bob --amount 1 --fee 2 \
      --sequence 0 --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  done
  sleep 2
}

join_light() {
  local id="$1" how="$2" ip="$3" port="$4" rroot="$5"
  local peers="100.64.0.1:19101,100.64.0.1:19102,100.64.0.3:19111"
  log "bond-in light $id @ $ip:$port"
  "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 faucet --to alice --amount 1000000 \
    --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  light_load 8
  "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 bond \
    --id "$id" --from alice --amount "$BOND" \
    --scheme "$SCHEME" --keys-dir "$KEYS" || true
  local i
  for i in $(seq 1 40); do
    light_load 4
    if "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
for v in (b.get('body') or b).get('validators') or []:
  if v.get('id')=='$id' and int(v.get('bond') or 0)>=$BOND: raise SystemExit(0)
raise SystemExit(1)
" 2>/dev/null; then
      break
    fi
  done
  "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 snapshot-save --path "$SNAP/v1.json" >/dev/null 2>&1 || true
  sleep 1
  cp -f "$SNAP/v1.json" "$SNAP/${id}.json"

  case "$how" in
    local)
      nohup "$ROOT/bin/falcon-pl-node" --join --id "$id" --listen "0.0.0.0:$port" \
        --peers "$peers" --validators "$SEED_VALS" --scheme "$SCHEME" --keys-dir "$KEYS" \
        --load-wallets "$LOAD_WALLETS" --bond "$BOND" --network-id "$NETWORK_ID" \
        --seal-interval-ms "$SEAL" --pack-budget-ms "$PACK" --progress-timeout-ms "$PROGRESS" \
        --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
        --snapshot-interval-ms 60000 --max-chain-ledgers 20000 --hubs "$HUBS" \
        --snapshot "$SNAP/${id}.json" --snapshot-out "$SNAP/${id}.json" \
        >>"$LOG/${id}.log" 2>&1 &
      log "start light $id pid=$!"
      ;;
    ssh:*)
      deploy_remote "$how" "$rroot"
      rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/${id}.json" "${how#ssh:}:$rroot/snapshots/${id}.json"
      ssh "${SSH_OPTS[@]}" "${how#ssh:}" bash -s <<EOF
set -e
R=$rroot
nohup \$R/bin/falcon-pl-node --join --id $id --listen 0.0.0.0:$port \
  --peers "$peers" --validators $SEED_VALS --scheme $SCHEME --keys-dir \$R/keys \
  --load-wallets $LOAD_WALLETS --bond $BOND --network-id $NETWORK_ID \
  --seal-interval-ms $SEAL --pack-budget-ms $PACK --progress-timeout-ms $PROGRESS \
  --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
  --snapshot-interval-ms 60000 --max-chain-ledgers 20000 --hubs "$HUBS" \
  --snapshot \$R/snapshots/${id}.json --snapshot-out \$R/snapshots/${id}.json \
  >>\$R/logs/${id}.log 2>&1 &
echo light $id pid=\$!
EOF
      ;;
    ts:*)
      deploy_remote "$how" "$rroot"
      tar -C "$SNAP" -czf - "${id}.json" \
        | timeout 40 tailscale ssh "${how#ts:}" "tar -C $rroot/snapshots -xzf -"
      timeout 40 tailscale ssh "${how#ts:}" bash -s <<EOF
set -e
R=$rroot
nohup \$R/bin/falcon-pl-node --join --id $id --listen 0.0.0.0:$port \
  --peers "$peers" --validators $SEED_VALS --scheme $SCHEME --keys-dir \$R/keys \
  --load-wallets $LOAD_WALLETS --bond $BOND --network-id $NETWORK_ID \
  --seal-interval-ms $SEAL --pack-budget-ms $PACK --progress-timeout-ms $PROGRESS \
  --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
  --snapshot-interval-ms 60000 --max-chain-ledgers 20000 --hubs "$HUBS" \
  --snapshot \$R/snapshots/${id}.json --snapshot-out \$R/snapshots/${id}.json \
  >>\$R/logs/${id}.log 2>&1 &
echo light $id pid=\$!
EOF
      ;;
  esac
  echo "$id $ip $port" >>"$RUN/live-mesh.txt"
  sleep 3
}

status() {
  "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
vals=body.get('validators') or []
print(f\"product={body.get('product_version')} role={body.get('role')} tip={body.get('tip_height')} mp={body.get('mempool')} winner={body.get('lottery_winner_next')} applicable={body.get('has_applicable_tx')}\")
print(f\"hub pulls={body.get('mempool_pulls')} served={body.get('mempool_served')} absorbed={body.get('mempool_absorbed')} no_app={body.get('pack_no_applicable')}\")
print(f\"ready={sum(1 for v in vals if v.get('lottery_ready'))}/{len(vals)} soft_need={body.get('soft_need')} hard_need={body.get('hard_need')}\")
for v in sorted(vals, key=lambda x: x.get('id') or ''):
  print(f\"  {v.get('id'):4} bond={v.get('bond')} ready={v.get('lottery_ready')} packs={v.get('pack_count')}\")
"
}

# --- main ---
stop_all
build
genesis
: >"$RUN/seed.pids"
# Keep genesis seed mesh (hubs); join_light appends lights. Do NOT wipe live-mesh.
start_seed
sleep 4
light_load 20
status

# grow light capacity
join_light v6 local 100.64.0.1 19103 "$ROOT"
join_light v5 ssh:host2 100.64.0.3 19112 "$ROOT"
join_light v4 ssh:host3 100.64.0.5 19121 "$ROOT"
join_light v7 ts:root@seat-host-4 100.64.0.2 19131 /root/falcon-pl-private-testnet-v22

light_load 25
sleep 3
log "======== final status ========"
status

# Submit load to hubs (logical global pending), not lagging light joiners.
nohup "$BIN/falcon-pl-load-net" \
  --keys "$KEYS" --scheme hmac --rpc "$HUBS" \
  --wallets 32 --rate 80 --seconds 600 --workers 12 \
  --network-id 2200 --amount 1 --fee 2 \
  --seq-file "$RUN/load-seq.json" \
  >>"$LOG/load-v224.log" 2>&1 &
echo $! >"$RUN/load.pid"
log "load pid=$(cat $RUN/load.pid) hubs @ 80 tx/s 10min"
log "done v2.2.4 testnet"
