#!/usr/bin/env bash
# Simulate a *growing* private mesh: seed small, then live bond-in one-by-one.
#
#   bash crates/fd-pl/scripts/pl-grow-sim.sh run          # full sim
#   bash crates/fd-pl/scripts/pl-grow-sim.sh status
#   bash crates/fd-pl/scripts/pl-grow-sim.sh lottery
#   bash crates/fd-pl/scripts/pl-grow-sim.sh stop
#
# Natural path (not all-at-once genesis):
#   1) start seed validators only
#   2) light load so chain advances
#   3) bond + snapshot + --join next validator
#   4) wait ready at tip → log lottery board
#   5) repeat
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BIN="${PL_BIN:-$RS/target/release}"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
RUN="$ROOT/run"
SCHEME="${PL_SCHEME:-hmac}"
NETWORK_ID="${PL_NETWORK_ID:-2200}"
BOND="${PL_BOND:-1000}"
EPOCH_MS="${PL_EPOCH_MS:-3600000}"
UNBOND_CD_MS="${PL_UNBOND_COOLDOWN_MS:-86400000}"
SEAL_MS="${PL_SEAL_MS:-400}"
PACK_BUDGET_MS="${PL_PACK_BUDGET_MS:-1000}"
PROGRESS_TIMEOUT_MS="${PL_PROGRESS_TIMEOUT_MS:-1200}"
MAX_MEMPOOL="${PL_MAX_MEMPOOL_TXS:-20000}"
# Pause between successful joins (seconds)
JOIN_PAUSE="${PL_JOIN_PAUSE:-12}"
LOAD_TXS="${PL_GROW_LOAD_TXS:-12}"

SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)

# Seed = only these get free genesis bonds
SEED=(
  "v1|local|100.64.0.1|19101|1"
  "v2|local|100.64.0.1|19102|0"
  "v3|ssh:host2|100.64.0.3|19111|0"
)

# Growth queue: id|how|ip|port|rroot
# how: local | ssh:host | ts:user@host
GROW=(
  "v4|ssh:host3|100.64.0.5|19121|/home/falcon/falcon-pl-private-testnet-v22"
  "v5|ssh:host2|100.64.0.3|19112|/home/falcon/falcon-pl-private-testnet-v22"
  "v6|local|100.64.0.1|19103|/home/falcon/falcon-pl-private-testnet-v22"
  "v7|ssh:host2|100.64.0.3|19113|/home/falcon/falcon-pl-private-testnet-v22"
  "v8|ts:root@seat-host-4|100.64.0.2|19131|/root/falcon-pl-private-testnet-v22"
  "v9|ts:root@seat-host-4|100.64.0.2|19132|/root/falcon-pl-private-testnet-v22"
  "v10|ts:root@seat-host-5|100.64.0.4|19141|/root/falcon-pl-private-testnet-v22"
  "v11|ts:root@seat-host-6|100.64.0.6|19151|/root/falcon-pl-private-testnet-v22"
  "v12|ts:root@seat-host-7|100.64.0.7|19161|/root/falcon-pl-private-testnet-v22"
  "v13|local|100.64.0.1|19104|/home/falcon/falcon-pl-private-testnet-v22"
)

PRIMARY="127.0.0.1:19101"
BOARD="$LOG/grow-sim.log"
cmd="${1:-run}"

mkdir -p "$ROOT"/{bin,keys,logs,snapshots,run}

log() { echo "$*" | tee -a "$BOARD"; }

remote() {
  local how="$1"; shift
  case "$how" in
    local) bash -c "$*" ;;
    ssh:*) ssh "${SSH_OPTS[@]}" "${how#ssh:}" "bash -s" <<<"$*" ;;
    ts:*) timeout 90 tailscale ssh "${how#ts:}" "bash -s" <<<"$*" ;;
    *) echo "bad how=$how" >&2; return 2 ;;
  esac
}

seed_ids() {
  local s=() e
  for e in "${SEED[@]}"; do s+=("${e%%|*}"); done
  (IFS=,; echo "${s[*]}")
}

all_ids() {
  local s=() e
  for e in "${SEED[@]}"; do s+=("${e%%|*}"); done
  for e in "${GROW[@]}"; do s+=("${e%%|*}"); done
  (IFS=,; echo "${s[*]}")
}

live_peers() {
  # currently running mesh peer list from run/live-mesh.txt
  if [[ -f "$RUN/live-mesh.txt" ]]; then
    awk '{printf "%s%s:%s", (n++?",":""), $2, $3} END{print ""}' "$RUN/live-mesh.txt"
  else
    echo ""
  fi
}

append_live() {
  local id="$1" ip="$2" port="$3"
  grep -q "^$id " "$RUN/live-mesh.txt" 2>/dev/null && return 0
  echo "$id $ip $port" >>"$RUN/live-mesh.txt"
}

lottery_board() {
  local label="${1:-}"
  log ""
  log "======== LOTTERY $label $(date -u +%Y-%m-%dT%H:%M:%SZ) ========"
  if ! "$BIN/falcon-pl-ctl" --addr "$PRIMARY" status >/dev/null 2>&1; then
    log "primary down"
    return 1
  fi
  "$BIN/falcon-pl-ctl" --addr "$PRIMARY" status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
vals=body.get('validators') or []
ready=[v for v in vals if v.get('lottery_ready')]
stale=[v for v in vals if not v.get('pong_fresh')]
print(f\"tip={body.get('tip_height')} hash={(body.get('tip_hash') or '')[:14]} lag={body.get('tip_lag')}\")
print(f\"winner={body.get('lottery_winner_next')} standby={body.get('lottery_standby_next')} ranked={body.get('lottery_ranked_next')}\")
print(f\"validators={len(vals)} lottery_ready={len(ready)} stale/offline={len(stale)} product={body.get('product_version')}\")
print(f\"{'id':5} {'bond':>5} {'ready':>6} {'packs':>6} {'perf':>6} {'pong_ms':>8}\")
for v in sorted(vals, key=lambda x: (-(x.get('pack_count') or 0), x.get('id') or '')):
  age=v.get('pong_age_ms')
  print(f\"{(v.get('id') or '?'):5} {v.get('bond') or 0:5} {str(bool(v.get('lottery_ready'))):>6} {v.get('pack_count') or 0:6} {float(v.get('perf') or 0):6.3f} {str(age if age is not None else '—'):>8}\")
" | tee -a "$BOARD"
}

status_mesh() {
  log "--- live mesh ---"
  if [[ ! -f "$RUN/live-mesh.txt" ]]; then log "(empty)"; return; fi
  while read -r id ip port; do
    out=$("$BIN/falcon-pl-ctl" --addr "$ip:$port" status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
print(f\"tip={body.get('tip_height')} lag={body.get('tip_lag')} winner={body.get('lottery_winner_next')} chain={body.get('chain_len')}\")
" 2>/dev/null || echo DOWN)
    log "  $id $ip:$port  $out"
  done <"$RUN/live-mesh.txt"
}

light_load() {
  local n="${1:-$LOAD_TXS}"
  local i
  for ((i = 0; i < n; i++)); do
    "$BIN/falcon-pl-ctl" --addr "$PRIMARY" pay --from alice --to bob --amount 1 --fee 2 \
      --sequence 0 --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  done
  sleep 2
}

stop_all() {
  log "stop all falcon-pl-node..."
  pkill -x falcon-pl-node 2>/dev/null || true
  for h in host2 host3; do
    ssh "${SSH_OPTS[@]}" "$h" 'pkill -x falcon-pl-node 2>/dev/null || true; for p in $(seq 19101 19170); do fuser -k ${p}/tcp 2>/dev/null; done; true' 2>/dev/null || true
  done
  for t in root@seat-host-4 root@seat-host-5 root@seat-host-6 root@seat-host-7; do
    timeout 25 tailscale ssh "$t" 'pkill -x falcon-pl-node 2>/dev/null || true; for p in $(seq 19101 19170); do fuser -k ${p}/tcp 2>/dev/null; done; true' 2>/dev/null || true
  done
  for p in $(seq 19101 19170); do fuser -k "${p}/tcp" 2>/dev/null || true; done
  rm -f "$RUN/live-mesh.txt" "$RUN/seed.pids" "$RUN/join.pids"
  sleep 1
  log "stopped"
}

build_and_keys() {
  cd "$RS"
  cargo build -p fd-pl --release 2>&1 | tail -4
  cp -f "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$ROOT/bin/" 2>/dev/null || {
    BIN="$RS/target/release"
    cp -f "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$ROOT/bin/"
  }
  chmod +x "$ROOT/bin/"*
  BIN="$ROOT/bin"

  rm -rf "$KEYS" "$SNAP"
  mkdir -p "$KEYS" "$SNAP" "$LOG" "$RUN"
  local all
  all=$(all_ids)
  # Pre-generate keys for seed + future joiners (shared keyring grows with network)
  "$BIN/falcon-pl-ctl" gen-keys \
    --keys-dir "$KEYS" --scheme "$SCHEME" \
    --validators "$all" --load-wallets 32
  "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account faucet --scheme "$SCHEME" || true
  echo "$(seed_ids)" >"$RUN/validators.txt"
  echo "$all" >"$RUN/all-planned.txt"
  : >"$RUN/live-mesh.txt"
  log "keys for planned validators: $all"
  log "seed genesis set: $(seed_ids)"
}

deploy_host() {
  local how="$1" rroot="$2"
  case "$how" in
    local)
      mkdir -p "$rroot"/{bin,keys,logs,snapshots,run}
      rsync -a "$ROOT/bin/" "$rroot/bin/"
      rsync -a --delete "$KEYS/" "$rroot/keys/"
      ;;
    ssh:*)
      local h="${how#ssh:}"
      ssh "${SSH_OPTS[@]}" "$h" "mkdir -p $rroot/{bin,keys,logs,snapshots,run}"
      rsync -az -e "ssh ${SSH_OPTS[*]}" "$ROOT/bin/" "$h:$rroot/bin/"
      rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$h:$rroot/keys/"
      ;;
    ts:*)
      local th="${how#ts:}"
      timeout 40 tailscale ssh "$th" "mkdir -p $rroot/{bin,keys,logs,snapshots,run}"
      local stage
      stage=$(mktemp -d)
      cp -a "$ROOT/bin/falcon-pl-node" "$ROOT/bin/falcon-pl-ctl" "$stage/"
      cp -a "$KEYS" "$stage/keys"
      tar -C "$stage" -czf - falcon-pl-node falcon-pl-ctl keys \
        | timeout 180 tailscale ssh "$th" "cd $rroot && tar -xzf - && mv -f falcon-pl-node falcon-pl-ctl bin/ && rm -rf keys && mv keys keys.tmp 2>/dev/null; rm -rf keys; mv keys.tmp keys 2>/dev/null || tar -C $rroot -xzf /dev/null; cd $rroot && rm -rf keys && mkdir -p keys && true"
      # more reliable second path for keys
      tar -C "$ROOT" -czf - keys \
        | timeout 120 tailscale ssh "$th" "cd $rroot && rm -rf keys && tar -xzf - && chmod +x bin/*"
      rm -rf "$stage"
      ;;
  esac
}

start_seed_node() {
  local id="$1" how="$2" ip="$3" port="$4" hist="$5"
  local seed_vals peers mcl rroot
  seed_vals=$(seed_ids)
  peers=$(live_peers)
  # first node peers empty until second starts — use planned seed peers
  peers=$(
    for e in "${SEED[@]}"; do
      IFS='|' read -r sid show sip sport shist <<<"$e"
      [[ "$sid" == "$id" ]] && continue
      echo -n "${sip}:${sport},"
    done | sed 's/,$//'
  )
  mcl=20000
  [[ "$hist" == "1" ]] && mcl=0
  rroot="/home/falcon/falcon-pl-private-testnet-v22"
  [[ "$how" == ts:* ]] && rroot="/root/falcon-pl-private-testnet-v22"

  deploy_host "$how" "$rroot"
  remote "$how" "
set -e
R=$rroot
rm -f \$R/snapshots/${id}.json
nohup \$R/bin/falcon-pl-node \
  --id $id --listen 0.0.0.0:$port --peers '$peers' \
  --validators $seed_vals --scheme $SCHEME --keys-dir \$R/keys \
  --load-wallets 32 --bond $BOND --network-id $NETWORK_ID \
  --seal-interval-ms $SEAL_MS --pack-budget-ms $PACK_BUDGET_MS \
  --progress-timeout-ms $PROGRESS_TIMEOUT_MS --max-mempool-txs $MAX_MEMPOOL \
  --epoch-ms $EPOCH_MS --unbond-cooldown-ms $UNBOND_CD_MS \
  --snapshot-interval-ms 60000 --max-chain-ledgers $mcl \
  --snapshot \$R/snapshots/${id}.json --snapshot-out \$R/snapshots/${id}.json \
  >>\$R/logs/${id}.log 2>&1 &
echo \$! >> \$R/run/seed.pids
echo seed $id pid=\$!
"
  append_live "$id" "$ip" "$port"
  log "SEED start $id @ $ip:$port hist=$hist"
}

join_one() {
  local id="$1" how="$2" ip="$3" port="$4" rroot="$5"
  local seed_vals peers
  seed_vals=$(seed_ids)
  peers=$(live_peers)
  [[ -n "$peers" ]] || { log "FAIL: no live peers"; return 2; }

  log ""
  log ">>>>>>>> GROW join $id on $how :$port <<<<<<<<"
  deploy_host "$how" "$rroot"

  # fund + bond on primary
  "$BIN/falcon-pl-ctl" --addr "$PRIMARY" faucet --to alice --amount 2000000 \
    --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  light_load 6
  log "bond $BOND → $id"
  "$BIN/falcon-pl-ctl" --addr "$PRIMARY" bond \
    --id "$id" --from alice --amount "$BOND" \
    --scheme "$SCHEME" --keys-dir "$KEYS" || true

  # pack until bond visible
  local i tip
  for i in $(seq 1 50); do
    light_load 4
    if "$BIN/falcon-pl-ctl" --addr "$PRIMARY" status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
for v in body.get('validators') or []:
  if v.get('id')=='$id' and int(v.get('bond') or 0) >= $BOND:
    print(body.get('tip_height')); sys.exit(0)
sys.exit(1)
" 2>/dev/null; then
      tip=$("$BIN/falcon-pl-ctl" --addr "$PRIMARY" status 2>/dev/null | python3 -c "import sys,json;r=sys.stdin.read();b=json.JSONDecoder().raw_decode(r[r.find('{'):])[0];print((b.get('body')or b).get('tip_height'))")
      log "bond sealed tip=$tip"
      break
    fi
    [[ $i -eq 50 ]] && { log "FAIL bond not sealed for $id"; return 2; }
  done

  # force snapshot from primary for state-sync
  mkdir -p "$SNAP"
  "$BIN/falcon-pl-ctl" --addr "$PRIMARY" snapshot-save --path "$SNAP/v1.json" >/dev/null 2>&1 || true
  # wait for bond in snap
  for i in $(seq 1 30); do
    if [[ -f "$SNAP/v1.json" ]] && python3 -c "
import json
d=json.load(open('$SNAP/v1.json'))
v=(d.get('validators') or {}).get('$id') or {}
import sys
sys.exit(0 if int(v.get('bond') or 0) >= $BOND else 1)
" 2>/dev/null; then
      break
    fi
    "$BIN/falcon-pl-ctl" --addr "$PRIMARY" snapshot-save --path "$SNAP/v1.json" >/dev/null 2>&1 || true
    sleep 1
  done
  cp -f "$SNAP/v1.json" "$SNAP/${id}.json"
  log "state-sync snap tip=$(python3 -c "import json;print(json.load(open('$SNAP/${id}.json')).get('tip_height'))")"

  # start --join
  remote "$how" "
set -e
R=$rroot
mkdir -p \$R/snapshots \$R/logs \$R/run
# snap may already be local if primary local; write provided content via stdin base64 if needed
true
"
  # copy snap to remote if needed
  case "$how" in
    local)
      cp -f "$SNAP/${id}.json" "$rroot/snapshots/${id}.json"
      ;;
    ssh:*)
      rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/${id}.json" "${how#ssh:}:$rroot/snapshots/${id}.json"
      rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "${how#ssh:}:$rroot/keys/"
      ;;
    ts:*)
      tar -C "$SNAP" -czf - "${id}.json" \
        | timeout 60 tailscale ssh "${how#ts:}" "mkdir -p $rroot/snapshots && tar -C $rroot/snapshots -xzf -"
      tar -C "$ROOT" -czf - keys \
        | timeout 90 tailscale ssh "${how#ts:}" "cd $rroot && rm -rf keys && tar -xzf -"
      ;;
  esac

  remote "$how" "
set -e
R=$rroot
nohup \$R/bin/falcon-pl-node \
  --join \
  --id $id --listen 0.0.0.0:$port --peers '$peers' \
  --validators $seed_vals --scheme $SCHEME --keys-dir \$R/keys \
  --load-wallets 32 --bond $BOND --network-id $NETWORK_ID \
  --seal-interval-ms $SEAL_MS --pack-budget-ms $PACK_BUDGET_MS \
  --progress-timeout-ms $PROGRESS_TIMEOUT_MS --max-mempool-txs $MAX_MEMPOOL \
  --epoch-ms $EPOCH_MS --unbond-cooldown-ms $UNBOND_CD_MS \
  --snapshot-interval-ms 60000 --max-chain-ledgers 20000 \
  --snapshot \$R/snapshots/${id}.json --snapshot-out \$R/snapshots/${id}.json \
  >>\$R/logs/${id}.log 2>&1 &
echo \$! >> \$R/run/join.pids
echo join $id pid=\$!
"

  # wait catch-up: tip_lag 0 and pong ready
  local j addr lag ready
  addr="$ip:$port"
  for j in $(seq 1 60); do
    sleep 2
    if "$BIN/falcon-pl-ctl" --addr "$addr" status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
lag=int(body.get('tip_lag') or 0)
tip=int(body.get('tip_height') or 0)
# self lottery ready
me=None
for v in body.get('validators') or []:
  if v.get('id')=='$id':
    me=v; break
ready=bool(me and me.get('lottery_ready'))
print(f'tip={tip} lag={lag} ready={ready}')
sys.exit(0 if lag==0 and tip>0 and ready else 1)
" 2>/dev/null | tee -a "$BOARD"; then
      log "JOINED $id catch-up OK"
      append_live "$id" "$ip" "$port"
      return 0
    fi
    # keep mesh packing so residual can land
    light_load 3
  done
  log "WARN: $id join not fully ready; recording live anyway"
  append_live "$id" "$ip" "$port"
  return 0
}

run_sim() {
  : >"$BOARD"
  log "Falcon PL growing-network simulation"
  log "seed=$(seed_ids)  planned_joins=${#GROW[@]}"
  stop_all
  build_and_keys

  log ""
  log "======== PHASE 1: seed network ========"
  local e id how ip port hist
  for e in "${SEED[@]}"; do
    IFS='|' read -r id how ip port hist <<<"$e"
    start_seed_node "$id" "$how" "$ip" "$port" "$hist"
    sleep 1
  done
  sleep 4
  light_load 20
  sleep 3
  lottery_board "seed($(seed_ids))"
  status_mesh

  log ""
  log "======== PHASE 2: gradual live bond-in ========"
  local n=0
  for e in "${GROW[@]}"; do
    n=$((n + 1))
    IFS='|' read -r id how ip port rroot <<<"$e"
    join_one "$id" "$how" "$ip" "$port" "$rroot" || log "join $id failed (continue)"
    light_load 10
    sleep 2
    lottery_board "after+$id ($n/${#GROW[@]})"
    status_mesh
    log "pause ${JOIN_PAUSE}s before next join..."
    sleep "$JOIN_PAUSE"
  done

  log ""
  log "======== PHASE 3: final soak load ========"
  light_load 40
  sleep 5
  lottery_board "FINAL"
  status_mesh
  log "grow sim complete. board → $BOARD"
  log "live mesh:"
  cat "$RUN/live-mesh.txt" | tee -a "$BOARD"
}

case "$cmd" in
  run) run_sim ;;
  stop) stop_all ;;
  status) status_mesh ;;
  lottery) lottery_board "manual" ;;
  *)
    echo "usage: $0 {run|stop|status|lottery}"
    exit 2
    ;;
esac
