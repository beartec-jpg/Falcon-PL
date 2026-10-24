#!/usr/bin/env bash
# Falcon PL 2300 — one node per host (7 servers).
# Does not touch 2200. Does not pkill -f. Stops seats by pidfile only.
#
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-seven-2300.sh deploy
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-seven-2300.sh add      # v6 mhr + v7 hel6
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-seven-2300.sh move-v2  # f1 → hil archive
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-seven-2300.sh move-v4  # f2 → fsn archive
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-seven-2300.sh status
#
# Target:
#   v1 host1  archive  :19301  full-history tail
#   v2 hil-8gb  archive  :19302
#   v3 host2  hub/val  :19303
#   v4 fsn-8gb  archive  :19304
#   v5 droplet  val      :19305
#   v6 mhr-4gb  val      :19306  (bond + join)
#   v7 hel6-4gb val      :19307  (bond + join)
set -euo pipefail

if [[ "${CONFIRM:-}" != "2300" ]]; then
  echo "refuse: set CONFIRM=2300" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=pl-hosts.env
source "$SCRIPT_DIR/pl-hosts.env"

ROOT="${PL_PUBLIC_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
BIN="$ROOT/bin"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
RUN="$ROOT/run"
NID=2300
SCHEME=falcon-512
EPOCH_MS=604800000
FIRST_CLAIM=1
UNBOND_MS=1209600000
VALS=v1,v2,v3,v4,v5
SSH_OPTS=(-o ConnectTimeout=15 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)

F1_IP="${F1_TS:-100.64.0.1}"
F2_IP="${F2_TS:-100.64.0.3}"
F3_IP="${F3_TS:-100.64.0.5}"
HIL_IP="${HIL_IP:-100.64.0.2}"
FSN_IP="${FSN_IP:-100.64.0.4}"
MHR_IP="${MHR_IP:-100.64.0.6}"
HEL_IP="${HEL_IP:-100.64.0.7}"

HIL_SSH="${HIL_SSH:-root@${HIL_IP}}"
FSN_SSH="${FSN_SSH:-root@${FSN_IP}}"
MHR_SSH="${MHR_SSH:-root@${MHR_IP}}"
HEL_SSH="${HEL_SSH:-root@${HEL_IP}}"

# remote data root: falcon users share $HOME path; TS boxes are root
RROOT_FALCON="$ROOT"
RROOT_TS="/root/falcon-pl-public-testnet-2300"

cmd="${1:-status}"

tip_of() {
  local addr=$1
  "$BIN/falcon-pl-ctl" --addr "$addr" status 2>/dev/null | python3 -c '
import sys,json
try:
  b=json.load(sys.stdin).get("body",{})
  print(int(b.get("tip_height") or -1))
except Exception:
  print(-1)
' || echo -1
}

hash_of() {
  local addr=$1
  "$BIN/falcon-pl-ctl" --addr "$addr" status 2>/dev/null | python3 -c '
import sys,json
try:
  b=json.load(sys.stdin).get("body",{})
  print(str(b.get("tip_hash") or "")[:16])
except Exception:
  print("-")
' || echo -
}

wait_tip() {
  local addr=$1 want=$2 tries=${3:-40}
  local t
  for i in $(seq 1 "$tries"); do
    t=$(tip_of "$addr")
    echo "  wait $addr tip=$t want>=$want ($i/$tries)"
    if [[ "$t" -ge "$want" && "$t" -gt 0 ]]; then
      return 0
    fi
    sleep 3
  done
  echo "timeout waiting $addr >= $want (last $t)" >&2
  return 1
}

kill_pidfile() {
  local pf=$1
  if [[ -f "$pf" ]]; then
    local pid
    pid=$(cat "$pf" || true)
    if [[ -n "${pid:-}" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" || true
      sleep 1
      kill -0 "$pid" 2>/dev/null && kill -TERM "$pid" || true
    fi
    rm -f "$pf"
  fi
}

remote_kill() {
  local host=$1 rroot=$2 id=$3
  ssh "${SSH_OPTS[@]}" "$host" "pf='$rroot/run/${id}.pid'; if [[ -f \$pf ]]; then pid=\$(cat \$pf); kill \$pid 2>/dev/null || true; sleep 1; kill -0 \$pid 2>/dev/null && kill -TERM \$pid 2>/dev/null || true; rm -f \$pf; fi"
}

deploy_host() {
  local host=$1 rroot=$2
  echo "[deploy] $host → $rroot"
  ssh "${SSH_OPTS[@]}" "$host" "mkdir -p '$rroot/bin' '$rroot/logs' '$rroot/snapshots' '$rroot/run' '$rroot/keys'"
  rsync -az -e "ssh ${SSH_OPTS[*]}" \
    "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$host:$rroot/bin/"
  ssh "${SSH_OPTS[@]}" "$host" "ln -sfn falcon-pl-node '$rroot/bin/falcon-pl-archive'; ln -sfn falcon-pl-node '$rroot/bin/falcon-pl-hub'; ln -sfn falcon-pl-node '$rroot/bin/falcon-pl-val'"
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$host:$rroot/keys/"
  if [[ -f "$SNAP/v1.join.json" ]]; then
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/v1.join.json" "$host:$rroot/snapshots/v1.join.json"
  fi
}

common_flags() {
  local rroot=$1 id=$2 extra=$3 listen=$4 peers=$5 hubs=$6 chain=$7
  echo "--id $id $extra --listen $listen --peers $peers --validators $VALS --scheme $SCHEME --keys-dir $rroot/keys --load-wallets 0 --bond 1000 --network-id $NID --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 --max-txs 128 --max-mempool-txs 50000 --epoch-ms $EPOCH_MS --first-claim-epoch $FIRST_CLAIM --unbond-cooldown-ms $UNBOND_MS --snapshot-interval-ms 60000 --max-chain-ledgers $chain --hubs $hubs --online-epoch-heights 10 --lock-public-wire --admin-sock $rroot/run/${id}.admin.sock --snapshot-out $rroot/snapshots/${id}.json"
}

start_remote() {
  local host=$1 rroot=$2 id=$3 exe=$4 extra=$5 port=$6 peers=$7 hubs=$8 chain=$9 snap=${10:-}
  local flags
  flags=$(common_flags "$rroot" "$id" "$extra" "0.0.0.0:$port" "$peers" "$hubs" "$chain")
  local snapf=""
  if [[ -n "$snap" ]]; then
    snapf="--snapshot $rroot/snapshots/$snap"
  fi
  ssh "${SSH_OPTS[@]}" "$host" "rm -f '$rroot/run/${id}.admin.sock'; : >'$rroot/logs/${id}.log'; nohup '$rroot/bin/$exe' $flags $snapf >'$rroot/logs/${id}.log' 2>&1 & echo \$! >'$rroot/run/${id}.pid'; echo started $id pid=\$(cat '$rroot/run/${id}.pid')"
}

PEERS_NOW="${F1_IP}:19301,${F1_IP}:19302,${F2_IP}:19303,${F2_IP}:19304,${F3_IP}:19305"
HUBS_NOW="${F1_IP}:19301,${F1_IP}:19302,${F2_IP}:19303"
PEERS_SEVEN="${F1_IP}:19301,${HIL_IP}:19302,${F2_IP}:19303,${FSN_IP}:19304,${F3_IP}:19305,${MHR_IP}:19306,${HEL_IP}:19307"
HUBS_SEVEN="${F1_IP}:19301,${HIL_IP}:19302,${FSN_IP}:19304,${F2_IP}:19303"

case "$cmd" in
  deploy)
    deploy_host "$HIL_SSH" "$RROOT_TS"
    deploy_host "$FSN_SSH" "$RROOT_TS"
    deploy_host "$MHR_SSH" "$RROOT_TS"
    deploy_host "$HEL_SSH" "$RROOT_TS"
    echo "[deploy] remotes ready"
    ;;
  add)
    echo "[add] gen v6 v7 keys"
    "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account v6 --scheme "$SCHEME"
    "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account v7 --scheme "$SCHEME"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$MHR_SSH:$RROOT_TS/keys/"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$HEL_SSH:$RROOT_TS/keys/"
    echo "[add] bond v6 v7 from faucet"
    "$BIN/falcon-pl-ctl" --addr 127.0.0.1:19301 bond --id v6 --from faucet --amount 1000 \
      --scheme "$SCHEME" --keys-dir "$KEYS" --network-id "$NID"
    "$BIN/falcon-pl-ctl" --addr 127.0.0.1:19301 bond --id v7 --from faucet --amount 1000 \
      --scheme "$SCHEME" --keys-dir "$KEYS" --network-id "$NID"
    sleep 4
    head=$(tip_of 127.0.0.1:19301)
    echo "[add] mesh tip=$head — start joiners"
    # refresh join snap
    [[ -f "$SNAP/v1.join.json" ]] || cp -f "$SNAP/v1.json" "$SNAP/v1.join.json"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/v1.join.json" "$MHR_SSH:$RROOT_TS/snapshots/v1.join.json"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/v1.join.json" "$HEL_SSH:$RROOT_TS/snapshots/v1.join.json"
    start_remote "$MHR_SSH" "$RROOT_TS" v6 falcon-pl-val "--join" 19306 \
      "$PEERS_NOW" "$HUBS_NOW" 128 v1.join.json
    start_remote "$HEL_SSH" "$RROOT_TS" v7 falcon-pl-val "--join" 19307 \
      "$PEERS_NOW" "$HUBS_NOW" 128 v1.join.json
    wait_tip "${MHR_IP}:19306" $((head - 32)) 50 || true
    wait_tip "${HEL_IP}:19307" $((head - 32)) 50 || true
    echo "[add] v6 tip=$(tip_of ${MHR_IP}:19306) v7 tip=$(tip_of ${HEL_IP}:19307) head=$(tip_of 127.0.0.1:19301)"
    ;;
  move-v2)
    head=$(tip_of 127.0.0.1:19301)
    echo "[move-v2] head=$head — stop local v2 then start on hil"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$HIL_SSH:$RROOT_TS/keys/"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/v1.join.json" "$HIL_SSH:$RROOT_TS/snapshots/v1.join.json"
    kill_pidfile "$RUN/v2.pid"
    sleep 1
    start_remote "$HIL_SSH" "$RROOT_TS" v2 falcon-pl-archive "--hub --archive" 19302 \
      "${F1_IP}:19301,${F2_IP}:19303,${F2_IP}:19304,${F3_IP}:19305,${MHR_IP}:19306,${HEL_IP}:19307" \
      "${F1_IP}:19301,${HIL_IP}:19302,${F2_IP}:19303" 20000 v1.join.json
    wait_tip "${HIL_IP}:19302" $((head - 16)) 50
    echo "[move-v2] hil v2 tip=$(tip_of ${HIL_IP}:19302) hash=$(hash_of ${HIL_IP}:19302) head=$(tip_of 127.0.0.1:19301) $(hash_of 127.0.0.1:19301)"
    ;;
  move-v4)
    head=$(tip_of 127.0.0.1:19301)
    echo "[move-v4] head=$head — stop f2 v4 then start on fsn"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$FSN_SSH:$RROOT_TS/keys/"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/v1.join.json" "$FSN_SSH:$RROOT_TS/snapshots/v1.join.json"
    remote_kill host2 "$RROOT_FALCON" v4
    sleep 1
    start_remote "$FSN_SSH" "$RROOT_TS" v4 falcon-pl-archive "--hub --archive" 19304 \
      "${F1_IP}:19301,${HIL_IP}:19302,${F2_IP}:19303,${F3_IP}:19305,${MHR_IP}:19306,${HEL_IP}:19307" \
      "${F1_IP}:19301,${HIL_IP}:19302,${FSN_IP}:19304,${F2_IP}:19303" 20000 v1.join.json
    wait_tip "${FSN_IP}:19304" $((head - 16)) 50
    echo "[move-v4] fsn v4 tip=$(tip_of ${FSN_IP}:19304) hash=$(hash_of ${FSN_IP}:19304) head=$(tip_of 127.0.0.1:19301)"
    ;;
  status)
    printf '%-6s %-22s %s %s\n' SEAT ADDR TIP HASH
    for spec in \
      "v1 ${F1_IP}:19301" \
      "v2 ${HIL_IP}:19302" \
      "v2old ${F1_IP}:19302" \
      "v3 ${F2_IP}:19303" \
      "v4 ${FSN_IP}:19304" \
      "v4old ${F2_IP}:19304" \
      "v5 ${F3_IP}:19305" \
      "v6 ${MHR_IP}:19306" \
      "v7 ${HEL_IP}:19307"
    do
      set -- $spec
      printf '%-6s %-22s %s %s\n' "$1" "$2" "$(tip_of "$2")" "$(hash_of "$2")"
    done
    ;;
  *)
    echo "usage: CONFIRM=2300 $0 deploy|add|move-v2|move-v4|status" >&2
    exit 2
    ;;
esac
