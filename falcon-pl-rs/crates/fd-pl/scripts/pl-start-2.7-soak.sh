#!/usr/bin/env bash
# Falcon PL 2.7.0 — fresh-genesis 3-hub soak with a load supervisor that
# restarts falcon-pl-load-net when the tip stops advancing (inject Recv-Q hang).
#
# Topology (same as the 2.6 private mesh):
#   host1  v1 :19101  +  v2 :19102
#   host2  v3 :19111
#
# Usage:
#   bash crates/fd-pl/scripts/pl-start-2.7-soak.sh          # stop, wipe snaps, start, soak
#   bash crates/fd-pl/scripts/pl-start-2.7-soak.sh status
#   bash crates/fd-pl/scripts/pl-start-2.7-soak.sh stop
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=pl-hosts.env
# shellcheck disable=SC1091
source "$SCRIPT_DIR/pl-hosts.env"

RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SRC_BIN="$RS/target/release"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
BIN="$ROOT/bin"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
RUN="$ROOT/run"
SCHEME=falcon-512
NID="${PL_NETWORK_ID:-2200}"
F1_IP="${F1_IP:-$F1_TS}"
F2_IP="${F2_IP:-$F2_TS}"
SSH_F2="${SSH_F2:-host2}"
SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
RATE="${PL_SOAK_RATE:-25}"
WINDOW="${PL_SOAK_WINDOW:-90}"
WORKERS="${PL_SOAK_WORKERS:-12}"
WALLETS="${PL_SOAK_WALLETS:-256}"

cmd="${1:-start}"
mkdir -p "$BIN" "$LOG" "$SNAP" "$RUN"

kill_by_exe() {
  local name="$1"
  for p in /proc/[0-9]*; do
    local pid=${p#/proc/}
    local exe
    exe=$(readlink -f "$p/exe" 2>/dev/null) || continue
    [[ "$(basename "$exe")" == "$name" ]] || continue
    kill "$pid" 2>/dev/null || true
  done
}

ssh_kill_by_exe() {
  local host="$1" name="$2"
  ssh "${SSH_OPTS[@]}" "$host" "bash -s" <<EOF
for p in /proc/[0-9]*; do
  pid=\${p#/proc/}
  exe=\$(readlink -f "\$p/exe" 2>/dev/null) || continue
  [[ "\$(basename "\$exe")" == "$name" ]] || continue
  kill "\$pid" 2>/dev/null || true
done
EOF
}

tip() {
  local a=${1:-127.0.0.1:19101}
  "$BIN/falcon-pl-ctl" --addr "$a" status 2>/dev/null | python3 -c '
import sys, json
try:
    print(int(json.load(sys.stdin)["body"]["tip_height"]))
except Exception:
    print(-1)
' || echo -1
}

status_field() {
  local a=${1:-127.0.0.1:19101}
  local f=${2:-product_version}
  "$BIN/falcon-pl-ctl" --addr "$a" status 2>/dev/null | python3 -c '
import sys, json
field = sys.argv[1]
try:
    b = json.load(sys.stdin)["body"]
    print(b.get(field, ""))
except Exception:
    print("")
' "$f" || true
}

stop_all() {
  echo "[stop] local nodes + load-net"
  kill_by_exe falcon-pl-load-net
  kill_by_exe falcon-pl-node
  echo "[stop] host2 nodes"
  ssh_kill_by_exe "$SSH_F2" falcon-pl-node
  sleep 2
  # leftover stubborn inject
  kill_by_exe falcon-pl-load-net
}

deploy_bins() {
  echo "[deploy] copy 2.7 bins → $BIN and $SSH_F2"
  install -m 0755 "$SRC_BIN/falcon-pl-node" "$BIN/falcon-pl-node"
  install -m 0755 "$SRC_BIN/falcon-pl-ctl" "$BIN/falcon-pl-ctl"
  install -m 0755 "$SRC_BIN/falcon-pl-load-net" "$BIN/falcon-pl-load-net"
  ssh "${SSH_OPTS[@]}" "$SSH_F2" "mkdir -p '$BIN' '$LOG' '$SNAP' '$RUN' '$KEYS'"
  rsync -az "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$SSH_F2:$BIN/"
}

wipe_snaps() {
  local stamp
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  mkdir -p "$SNAP/archive-pre-2.7-$stamp"
  mv "$SNAP"/v*.json "$SNAP/archive-pre-2.7-$stamp/" 2>/dev/null || true
  ssh "${SSH_OPTS[@]}" "$SSH_F2" "mkdir -p '$SNAP/archive-pre-2.7-$stamp'; mv '$SNAP'/v*.json '$SNAP/archive-pre-2.7-$stamp/' 2>/dev/null || true"
  rm -f "$RUN"/v1.admin.sock "$RUN"/v2.admin.sock
  ssh "${SSH_OPTS[@]}" "$SSH_F2" "rm -f '$RUN/v3.admin.sock'"
  echo "[wipe] snapshots archived, fresh genesis"
}

start_local() {
  local id=$1 port=$2 peers=$3
  local snap_in=()
  if [[ "${KEEP_SNAP:-0}" == "1" && -f "$SNAP/${id}.json" ]]; then
    snap_in=(--snapshot "$SNAP/${id}.json")
    echo "[start] $id loading snapshot $SNAP/${id}.json"
  fi
  : >"$LOG/${id}.log"
  nohup "$BIN/falcon-pl-node" --id "$id" --hub --listen "0.0.0.0:$port" \
    --peers "$peers" \
    --validators v1,v2,v3 --scheme "$SCHEME" --keys-dir "$KEYS" \
    --load-wallets 1024 --bond 1000 --network-id "$NID" \
    --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
    --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
    --snapshot-interval-ms 60000 --max-chain-ledgers 20000 \
    --hubs "${F1_IP}:19101,${F1_IP}:19102,${F2_IP}:19111" \
    --online-epoch-heights 10 \
    --admin-sock "$RUN/${id}.admin.sock" \
    --lock-public-wire \
    "${snap_in[@]}" \
    --snapshot-out "$SNAP/${id}.json" \
    >"$LOG/${id}.log" 2>&1 &
  echo $! >"$RUN/${id}.pid"
  echo "[start] $id pid=$(cat "$RUN/${id}.pid") :$port admin=$RUN/${id}.admin.sock"
}

start_f2_v3() {
  local snap_flag=""
  if [[ "${KEEP_SNAP:-0}" == "1" ]]; then
    snap_flag="--snapshot '$SNAP/v3.json'"
  fi
  ssh "${SSH_OPTS[@]}" "$SSH_F2" "rm -f '$RUN/v3.admin.sock'; : >'$LOG/v3.log'; nohup '$BIN/falcon-pl-node' --id v3 --hub --listen 0.0.0.0:19111 \
    --peers ${F1_IP}:19101,${F1_IP}:19102 \
    --validators v1,v2,v3 --scheme $SCHEME --keys-dir '$KEYS' \
    --load-wallets 1024 --bond 1000 --network-id $NID \
    --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
    --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
    --snapshot-interval-ms 60000 --max-chain-ledgers 20000 \
    --hubs ${F1_IP}:19101,${F1_IP}:19102,${F2_IP}:19111 \
    --online-epoch-heights 10 \
    --admin-sock '$RUN/v3.admin.sock' \
    --lock-public-wire \
    $snap_flag \
    --snapshot-out '$SNAP/v3.json' \
    >'$LOG/v3.log' 2>&1 & echo \$! >'$RUN/v3.pid'; echo started v3 pid=\$(cat '$RUN/v3.pid')"
}

write_supervisor() {
  cat >"$RUN/soak-supervisor-2.7.sh" <<EOS
#!/usr/bin/env bash
# Restarts load-net in ${WINDOW}s windows so a Recv-Q hang cannot freeze inject.
set -u
BIN="$BIN"
KEYS="$KEYS"
LOG="$LOG"
RUN="$RUN"
SCHEME="$SCHEME"
NID="$NID"
RATE="$RATE"
WINDOW="$WINDOW"
WORKERS="$WORKERS"
WALLETS="$WALLETS"
RPC="127.0.0.1:19101,127.0.0.1:19102,${F2_IP}:19111"
CTL="\$BIN/falcon-pl-ctl"
SLOG="\$LOG/soak-2.7.log"

tip() {
  local a=\${1:-127.0.0.1:19101}
  "\$CTL" --addr "\$a" status 2>/dev/null | python3 -c 'import sys,json
try: print(int(json.load(sys.stdin)["body"]["tip_height"]))
except: print(-1)' || echo -1
}

kill_load() {
  for p in /proc/[0-9]*; do
    pid=\${p#/proc/}
    exe=\$(readlink -f "\$p/exe" 2>/dev/null) || continue
    [[ "\$(basename "\$exe")" == "falcon-pl-load-net" ]] || continue
    kill "\$pid" 2>/dev/null || true
  done
}

log() { echo "[\$(date -u +%Y-%m-%dT%H:%M:%SZ)] \$*" | tee -a "\$SLOG"; }

last=-1
stalls=0
cycle=0
log "soak supervisor start rate=\$RATE window=\${WINDOW}s wallets=\$WALLETS"
while true; do
  cycle=\$((cycle+1))
  t0=\$(tip)
  h1=\$(tip 127.0.0.1:19101); h2=\$(tip 127.0.0.1:19102); h3=\$(tip ${F2_IP}:19111)
  log "cycle=\$cycle pre tip v1=\$h1 v2=\$h2 v3=\$h3"
  kill_load
  "\$BIN/falcon-pl-load-net" --keys "\$KEYS" --scheme "\$SCHEME" \\
    --rpc "\$RPC" --wallets "\$WALLETS" --seconds "\$WINDOW" --rate "\$RATE" \\
    --workers "\$WORKERS" --network-id "\$NID" --amount 1 --fee 25 --drain-wait 8 \\
    --seq-file "\$RUN/load-seq-2.7.json" \\
    >>"\$LOG/load-2.7.log" 2>&1 &
  lpid=\$!
  echo "\$lpid" >"\$RUN/load-2.7.pid"
  # Hard cap: load-net Recv-Q can hang past --seconds.
  (
    sleep \$((WINDOW + 20))
    if kill -0 "\$lpid" 2>/dev/null; then
      log "load-net \$lpid hung past window; killing"
      kill "\$lpid" 2>/dev/null || true
    fi
  ) &
  wait "\$lpid" 2>/dev/null || true
  kill_load
  t1=\$(tip)
  if [ "\$t1" -gt "\$t0" ] 2>/dev/null; then
    stalls=0
    log "PROGRESS \$t0->\$t1 Δ\$((t1-t0))"
  else
    stalls=\$((stalls+1))
    log "STALL tip=\$t1 (was \$t0) stalls=\$stalls"
  fi
  last=\$t1
  sleep 2
done
EOS
  chmod +x "$RUN/soak-supervisor-2.7.sh"
}

start_supervisor() {
  write_supervisor
  : >"$LOG/load-2.7.log"
  nohup "$RUN/soak-supervisor-2.7.sh" >"$LOG/soak-2.7.supervisor.out" 2>&1 &
  echo $! >"$RUN/soak-2.7.pid"
  echo "[soak] supervisor pid=$(cat "$RUN/soak-2.7.pid") log=$LOG/soak-2.7.log"
}

show_status() {
  echo "======== 2.7 soak status ========"
  for a in 127.0.0.1:19101 127.0.0.1:19102 "${F2_IP}:19111"; do
    echo -n "  $a tip=$(tip "$a") product=$(status_field "$a" product_version) "
    echo "hash=$(status_field "$a" tip_hash | cut -c1-16)"
  done
  echo "  supervisor pid=$(cat "$RUN/soak-2.7.pid" 2>/dev/null || echo -)"
  tail -n 8 "$LOG/soak-2.7.log" 2>/dev/null || true
}

case "$cmd" in
  stop)
    if [[ -f "$RUN/soak-2.7.pid" ]]; then
      kill "$(cat "$RUN/soak-2.7.pid")" 2>/dev/null || true
      rm -f "$RUN/soak-2.7.pid"
    fi
    stop_all
    echo "[stop] done"
    ;;
  status)
    show_status
    ;;
  start|restart|*)
    if [[ -f "$RUN/soak-2.7.pid" ]]; then
      kill "$(cat "$RUN/soak-2.7.pid")" 2>/dev/null || true
      rm -f "$RUN/soak-2.7.pid"
    fi
    stop_all
    if [[ ! -x "$SRC_BIN/falcon-pl-node" ]]; then
      echo "missing $SRC_BIN/falcon-pl-node — cargo build --release first" >&2
      exit 2
    fi
    deploy_bins
    if [[ "$cmd" == "restart" ]]; then
      KEEP_SNAP=1
      echo "[restart] keeping snapshots"
    else
      wipe_snaps
    fi
    start_local v1 19101 "${F1_IP}:19102,${F2_IP}:19111"
    start_local v2 19102 "${F1_IP}:19101,${F2_IP}:19111"
    start_f2_v3
    echo "[wait] hubs coming up"
    for i in 1 2 3 4 5 6 7 8 9 10; do
      t=$(tip 127.0.0.1:19101)
      p=$(status_field 127.0.0.1:19101 product_version)
      echo "  try $i tip=$t product=$p"
      if [[ "$p" == "2.7.0" ]]; then
        break
      fi
      sleep 1
    done
    show_status
    start_supervisor
    echo "[ok] 2.7.0 soak running (rate=${RATE} tps, ${WINDOW}s windows) cmd=$cmd"
    ;;
esac
