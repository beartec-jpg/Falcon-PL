#!/usr/bin/env bash
# Falcon PL 2.9.0 — fresh-genesis 11-val committee soak.
#
#   host1   v1 :19101 hub, v2 :19102 hub, v6 :19103, v7 :19104, v8 :19105
#   host2   v3 :19111 hub, v5 :19112, v9 :19113, v10 :19114
#   droplet   v4 :19121, v11 :19122
#
# Lottery freezes 1 packer + 5 checkers from the 11. Commit = 4 of those 6 (packer counts). Cert rides the next header.
# Hard = ceil(11*0.6)=7. Committee rotates each height.
#
#   bash crates/fd-pl/scripts/pl-start-2.9.0-soak.sh          # stop, wipe, start, soak
#   bash crates/fd-pl/scripts/pl-start-2.9.0-soak.sh resume   # stop, deploy, start from existing snaps
#   bash crates/fd-pl/scripts/pl-start-2.9.0-soak.sh status
#   bash crates/fd-pl/scripts/pl-start-2.9.0-soak.sh stop
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
WANT_VER="${PL_PRODUCT_VERSION:-2.9.30}"
F1_IP="${F1_IP:-$F1_TS}"
F2_IP="${F2_IP:-$F2_TS}"
F3_IP="${F3_IP:-$F3_TS}"
SSH_F2="${SSH_F2:-host2}"
SSH_F3="${SSH_F3:-host3}"
SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
RATE="${PL_SOAK_RATE:-25}"
WINDOW="${PL_SOAK_WINDOW:-90}"
WORKERS="${PL_SOAK_WORKERS:-12}"
WALLETS="${PL_SOAK_WALLETS:-256}"
# Mainnet-like: 1h epoch, first emit at epoch 8. Rewards test: PL_EPOCH_MS=30000 PL_FIRST_CLAIM_EPOCH=1
EPOCH_MS="${PL_EPOCH_MS:-3600000}"
FIRST_CLAIM="${PL_FIRST_CLAIM_EPOCH:-8}"
VALS=v1,v2,v3,v4,v5,v6,v7,v8,v9,v10,v11
HUBS="${F1_IP}:19101,${F1_IP}:19102,${F2_IP}:19111"
ALL_PEERS="${F1_IP}:19101,${F1_IP}:19102,${F1_IP}:19103,${F1_IP}:19104,${F1_IP}:19105,${F2_IP}:19111,${F2_IP}:19112,${F2_IP}:19113,${F2_IP}:19114,${F3_IP}:19121,${F3_IP}:19122"

cmd="${1:-start}"
mkdir -p "$BIN" "$LOG" "$SNAP" "$RUN"

# Match live and replaced-bin leftovers. `readlink -f` drops
# " (deleted)", so a soak restart that installs a new binary
# used to miss the old pids and leave a mixed mesh.
kill_by_exe() {
  local name="$1"
  local pid exe comm
  for p in /proc/[0-9]*; do
    pid=${p#/proc/}
    exe=$(readlink "$p/exe" 2>/dev/null) || continue
    comm=$(cat "$p/comm" 2>/dev/null || true)
    [[ "$exe" == *"$name"* || "$comm" == "$name" ]] || continue
    kill "$pid" 2>/dev/null || true
  done
}

ssh_kill_by_exe() {
  local host="$1" name="$2"
  ssh "${SSH_OPTS[@]}" "$host" "bash -s" <<EOF
for p in /proc/[0-9]*; do
  pid=\${p#/proc/}
  exe=\$(readlink "\$p/exe" 2>/dev/null) || continue
  comm=\$(cat "\$p/comm" 2>/dev/null || true)
  [[ "\$exe" == *"$name"* || "\$comm" == "$name" ]] || continue
  kill "\$pid" 2>/dev/null || true
done
EOF
}

exe_for() {
  case "$1" in
    v1) echo "$BIN/falcon-pl-archive" ;;
    v2|v3) echo "$BIN/falcon-pl-hub" ;;
    *) echo "$BIN/falcon-pl-val" ;;
  esac
}

peers_for() {
  local self=$1
  python3 - "$ALL_PEERS" "$self" <<'PY'
import sys
allp, self = sys.argv[1], sys.argv[2]
print(",".join(p for p in allp.split(",") if p != self))
PY
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

stop_supervisor() {
  if [[ -f "$RUN/soak-2.7.pid" ]]; then
    kill "$(cat "$RUN/soak-2.7.pid")" 2>/dev/null || true
    rm -f "$RUN/soak-2.7.pid"
  fi
}

stop_all() {
  echo "[stop] supervisor + local nodes + load-net"
  stop_supervisor
  kill_by_exe falcon-pl-load-net
  kill_by_exe falcon-pl-node
  echo "[stop] host2 + droplet nodes"
  ssh_kill_by_exe "$SSH_F2" falcon-pl-node
  ssh_kill_by_exe "$SSH_F3" falcon-pl-node
  sleep 2
  kill_by_exe falcon-pl-load-net
  kill_by_exe falcon-pl-node
  # leftovers after a bin replace ignore SIGTERM if already exiting; force
  for p in /proc/[0-9]*; do
    local pid=${p#/proc/}
    local exe comm
    exe=$(readlink "$p/exe" 2>/dev/null) || continue
    comm=$(cat "$p/comm" 2>/dev/null || true)
    if [[ "$exe" == *falcon-pl-node* || "$comm" == falcon-pl-node || "$exe" == *falcon-pl-load-net* || "$comm" == falcon-pl-load-net ]]; then
      kill -9 "$pid" 2>/dev/null || true
    fi
  done
}

deploy_bins() {
  echo "[deploy] $WANT_VER bins → $BIN / $SSH_F2 / $SSH_F3"
  install -m 0755 "$SRC_BIN/falcon-pl-node" "$BIN/falcon-pl-node"
  install -m 0755 "$SRC_BIN/falcon-pl-ctl" "$BIN/falcon-pl-ctl"
  install -m 0755 "$SRC_BIN/falcon-pl-load-net" "$BIN/falcon-pl-load-net"
  # Same image, argv0 selects role (2.9.28+).
  ln -sfn falcon-pl-node "$BIN/falcon-pl-archive"
  ln -sfn falcon-pl-node "$BIN/falcon-pl-hub"
  ln -sfn falcon-pl-node "$BIN/falcon-pl-val"
  for h in "$SSH_F2" "$SSH_F3"; do
    ssh "${SSH_OPTS[@]}" "$h" "mkdir -p '$BIN' '$LOG' '$SNAP' '$RUN' '$KEYS'"
    rsync -az -e "ssh ${SSH_OPTS[*]}" \
      "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$BIN/falcon-pl-load-net" "$h:$BIN/"
    ssh "${SSH_OPTS[@]}" "$h" "ln -sfn falcon-pl-node '$BIN/falcon-pl-archive'; ln -sfn falcon-pl-node '$BIN/falcon-pl-hub'; ln -sfn falcon-pl-node '$BIN/falcon-pl-val'"
    rsync -az -e "ssh ${SSH_OPTS[*]}" \
      "$KEYS/pl_keys.json" "$KEYS/v1.json" "$KEYS/v2.json" "$KEYS/v3.json" \
      "$KEYS/v4.json" "$KEYS/v5.json" "$KEYS/v6.json" \
      "$KEYS/v7.json" "$KEYS/v8.json" "$KEYS/v9.json" \
      "$KEYS/v10.json" "$KEYS/v11.json" "$KEYS/alice.json" \
      "$h:$KEYS/"
  done
}

wipe_snaps() {
  local stamp
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  mkdir -p "$SNAP/archive-pre-274-$stamp"
  mv "$SNAP"/v*.json "$SNAP/archive-pre-274-$stamp/" 2>/dev/null || true
  rm -f "$RUN"/v*.admin.sock "$RUN"/load-seq-2.7.json
  for h in "$SSH_F2" "$SSH_F3"; do
    ssh "${SSH_OPTS[@]}" "$h" "mkdir -p '$SNAP/archive-pre-274-$stamp'; mv '$SNAP'/v*.json '$SNAP/archive-pre-274-$stamp/' 2>/dev/null || true; rm -f '$RUN'/v*.admin.sock"
  done
  echo "[wipe] snapshots archived $stamp, fresh genesis"
}

COMMON_FLAGS=(
  --validators "$VALS" --scheme "$SCHEME" --keys-dir "$KEYS"
  --load-wallets 256 --bond 1000 --network-id "$NID"
  --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000
  --max-txs 128 --max-mempool-txs 20000 --epoch-ms "$EPOCH_MS" --first-claim-epoch "$FIRST_CLAIM"
  --unbond-cooldown-ms 86400000
  --snapshot-interval-ms 60000 --max-chain-ledgers 20000
  --hubs "$HUBS" --online-epoch-heights 10 --lock-public-wire
)

start_local() {
  local id=$1 port=$2 hub_flag=$3
  local self="${F1_IP}:${port}"
  local peers exe
  peers=$(peers_for "$self")
  exe=$(exe_for "$id")
  : >"$LOG/${id}.log"
  local extra=""
  [[ "$id" == "v1" ]] && extra="--archive"
  local snap_in=()
  if [[ "${RESUME:-0}" == "1" && -f "$SNAP/${id}.json" ]]; then
    snap_in=(--snapshot "$SNAP/${id}.json")
  fi
  nohup "$exe" --id "$id" $hub_flag $extra --listen "0.0.0.0:$port" \
    --peers "$peers" \
    "${COMMON_FLAGS[@]}" \
    --admin-sock "$RUN/${id}.admin.sock" \
    "${snap_in[@]}" \
    --snapshot-out "$SNAP/${id}.json" \
    >"$LOG/${id}.log" 2>&1 &
  echo $! >"$RUN/${id}.pid"
  echo "[start] local $id exe=$(basename "$exe") pid=$(cat "$RUN/${id}.pid") :$port resume=${RESUME:-0}"
}

start_remote() {
  local host=$1 id=$2 port=$3 self_ip=$4 hub_flag=$5
  local self="${self_ip}:${port}"
  local peers
  peers=$(peers_for "$self")
  local snap_in=""
  if [[ "${RESUME:-0}" == "1" ]]; then
    snap_in="--snapshot '$SNAP/${id}.json'"
  fi
  local exe
  exe=$(exe_for "$id")
  ssh "${SSH_OPTS[@]}" "$host" "rm -f '$RUN/${id}.admin.sock'; : >'$LOG/${id}.log'; nohup '$exe' --id $id $hub_flag --listen 0.0.0.0:$port \
    --peers $peers \
    --validators $VALS --scheme $SCHEME --keys-dir '$KEYS' \
    --load-wallets 256 --bond 1000 --network-id $NID \
    --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
    --max-txs 128 --max-mempool-txs 20000 --epoch-ms $EPOCH_MS --first-claim-epoch $FIRST_CLAIM \
    --unbond-cooldown-ms 86400000 \
    --snapshot-interval-ms 60000 --max-chain-ledgers 20000 \
    --hubs $HUBS --online-epoch-heights 10 \
    --admin-sock '$RUN/${id}.admin.sock' --lock-public-wire \
    $snap_in \
    --snapshot-out '$SNAP/${id}.json' \
    >'$LOG/${id}.log' 2>&1 & echo \$! >'$RUN/${id}.pid'; echo started $id pid=\$(cat '$RUN/${id}.pid')"
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

stalls=0
cycle=0
log "soak supervisor 2.9.0 start rate=\$RATE window=\${WINDOW}s wallets=\$WALLETS"
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
  sleep 2
done
EOS
  chmod +x "$RUN/soak-supervisor-2.7.sh"
}

start_supervisor() {
  write_supervisor
  : >"$LOG/load-2.7.log"
  : >"$LOG/soak-2.7.log"
  nohup "$RUN/soak-supervisor-2.7.sh" >"$LOG/soak-2.7.supervisor.out" 2>&1 &
  echo $! >"$RUN/soak-2.7.pid"
  echo "[soak] supervisor pid=$(cat "$RUN/soak-2.7.pid") log=$LOG/soak-2.7.log"
}

show_status() {
  echo "======== 2.9.0 soak status ========"
  python3 - "$BIN/falcon-pl-ctl" "$F2_IP" "$F3_IP" <<'PY'
import json, subprocess, sys
ctl, f2, f3 = sys.argv[1], sys.argv[2], sys.argv[3]
rows=[]
for name,a in [
    ("v1","127.0.0.1:19101"),("v2","127.0.0.1:19102"),("v6","127.0.0.1:19103"),
    ("v7","127.0.0.1:19104"),("v8","127.0.0.1:19105"),
    ("v3",f"{f2}:19111"),("v5",f"{f2}:19112"),("v9",f"{f2}:19113"),("v10",f"{f2}:19114"),
    ("v4",f"{f3}:19121"),("v11",f"{f3}:19122")]:
    try:
        b=json.loads(subprocess.check_output([ctl,"--addr",a,"status"], text=True, timeout=8))["body"]
        rows.append((name, b.get("product_version"), int(b.get("tip_height") or -1),
                     str(b.get("tip_hash") or "")[:16],
                     b.get("soft_need"), b.get("hard_need"),
                     len(b.get("validators") or []),
                     ",".join(b.get("qc_committee") or b.get("lottery_order") or [])))
    except Exception as e:
        rows.append((name,"-",-1,str(e)[:40],"-","-",0,""))
print("  id  ver    tip  hash             soft hard n  committee")
for r in rows:
    print("  %-3s %-6s %4s  %-16s %4s %4s %d  %s" % r)
tips={r[0]:r[1:] for r in rows}
PY
  echo "  supervisor pid=$(cat "$RUN/soak-2.7.pid" 2>/dev/null || echo -)"
  tail -n 6 "$LOG/soak-2.7.log" 2>/dev/null || true
}

case "$cmd" in
  stop)
    stop_all
    echo "[stop] done"
    ;;
  status)
    show_status
    ;;
  resume)
    export RESUME=1
    stop_all
    if [[ ! -x "$SRC_BIN/falcon-pl-node" ]]; then
      echo "missing $SRC_BIN/falcon-pl-node — cargo build --release first" >&2
      exit 2
    fi
    deploy_bins
    echo "[resume] keeping snapshots (no wipe) WANT_VER=$WANT_VER epoch_ms=$EPOCH_MS first_claim=$FIRST_CLAIM"
    start_local v1 19101 --hub
    start_local v2 19102 --hub
    start_local v6 19103 ""
    start_local v7 19104 ""
    start_local v8 19105 ""
    start_remote "$SSH_F2" v3 19111 "$F2_IP" --hub
    start_remote "$SSH_F2" v5 19112 "$F2_IP" ""
    start_remote "$SSH_F2" v9 19113 "$F2_IP" ""
    start_remote "$SSH_F2" v10 19114 "$F2_IP" ""
    # v4 stays down (2-core droplet = one val).
    start_remote "$SSH_F3" v11 19122 "$F3_IP" ""
    echo "[wait] 11-val $WANT_VER resume"
    ready=0
    for i in $(seq 1 90); do
      p=$(status_field 127.0.0.1:19101 product_version)
      t=$(tip 127.0.0.1:19101)
      echo "  try $i v1 tip=$t product=$p"
      if [[ "$p" == "$WANT_VER" && "$t" -gt 0 ]]; then
        ready=1
        break
      fi
      sleep 2
    done
    if [[ "$ready" -ne 1 ]]; then
      echo "FAIL: v1 did not come up as $WANT_VER" >&2
      tail -30 "$LOG/v1.log" || true
      exit 2
    fi
    show_status
    start_supervisor
    echo "[ok] $WANT_VER 11-val resumed (rate=${RATE} tps, ${WINDOW}s windows)"
    ;;
  start|restart|*)
    stop_all
    if [[ ! -x "$SRC_BIN/falcon-pl-node" ]]; then
      echo "missing $SRC_BIN/falcon-pl-node — cargo build --release first" >&2
      exit 2
    fi
    deploy_bins
    wipe_snaps
    start_local v1 19101 --hub
    start_local v2 19102 --hub
    start_local v6 19103 ""
    start_local v7 19104 ""
    start_local v8 19105 ""
    start_remote "$SSH_F2" v3 19111 "$F2_IP" --hub
    start_remote "$SSH_F2" v5 19112 "$F2_IP" ""
    start_remote "$SSH_F2" v9 19113 "$F2_IP" ""
    start_remote "$SSH_F2" v10 19114 "$F2_IP" ""
    start_remote "$SSH_F3" v4 19121 "$F3_IP" ""
    start_remote "$SSH_F3" v11 19122 "$F3_IP" ""
    echo "[wait] 11-val $WANT_VER genesis"
    ready=0
    for i in $(seq 1 30); do
      p=$(status_field 127.0.0.1:19101 product_version)
      t=$(tip 127.0.0.1:19101)
      echo "  try $i v1 tip=$t product=$p"
      if [[ "$p" == "$WANT_VER" ]]; then
        ready=1
        break
      fi
      sleep 1
    done
    if [[ "$ready" -ne 1 ]]; then
      echo "FAIL: v1 did not come up as $WANT_VER" >&2
      tail -20 "$LOG/v1.log" || true
      exit 2
    fi
    show_status
    start_supervisor
    echo "[ok] $WANT_VER 11-val soak running (rate=${RATE} tps, ${WINDOW}s windows)"
    ;;
esac
