#!/usr/bin/env bash
# Falcon PL public testnet 2300 — NEW genesis. Never writes 2200.
#
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-start-public-2300.sh keys
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-start-public-2300.sh start   # local 5-val smoke
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-start-public-2300.sh status
#   CONFIRM=2300 bash crates/fd-pl/scripts/pl-start-public-2300.sh stop
#
# Ports 19301–19305 so a live private 2200 mesh on 19101 is not touched.
#
#   CONFIRM=2300 bash .../pl-start-public-2300.sh expand
#     host1  v1 archive+hub :19301, v2 hub :19302
#     host2  v3 hub :19303, v4 val :19304
#     droplet  v5 val :19305   (one seat — 2-core)
set -euo pipefail

if [[ "${CONFIRM:-}" != "2300" ]]; then
  echo "refuse: set CONFIRM=2300 (will not touch network 2200)" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=pl-hosts.env
# shellcheck disable=SC1091
source "$SCRIPT_DIR/pl-hosts.env"
RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SRC_BIN="$RS/target/release"
ROOT="${PL_PUBLIC_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
BIN="$ROOT/bin"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
RUN="$ROOT/run"
NID=2300
SCHEME=falcon-512
WANT_VER="${PL_PRODUCT_VERSION:-2.9.30}"
VALS=v1,v2,v3,v4,v5
# Public params (not soak)
EPOCH_MS=604800000          # 7d
FIRST_CLAIM=1
UNBOND_MS=1209600000        # 14d
cmd="${1:-status}"

mkdir -p "$BIN" "$KEYS" "$LOG" "$SNAP" "$RUN"

kill_by_exe_in_root() {
  local name="$1"
  for p in /proc/[0-9]*; do
    pid=${p#/proc/}
    exe=$(readlink "$p/exe" 2>/dev/null) || continue
    cmdl=$(tr '\0' ' ' <"$p/cmdline" 2>/dev/null || true)
    [[ "$exe" == *"$name"* ]] || continue
    [[ "$cmdl" == *"$ROOT"* ]] || continue
    kill "$pid" 2>/dev/null || true
  done
}

case "$cmd" in
  keys)
    if [[ ! -x "$SRC_BIN/falcon-pl-ctl" ]]; then
      echo "missing $SRC_BIN/falcon-pl-ctl — cargo build --release" >&2
      exit 2
    fi
    install -m 0755 "$SRC_BIN/falcon-pl-node" "$BIN/falcon-pl-node"
    install -m 0755 "$SRC_BIN/falcon-pl-ctl" "$BIN/falcon-pl-ctl"
    install -m 0755 "$SRC_BIN/falcon-pl-load-net" "$BIN/falcon-pl-load-net" 2>/dev/null || true
    "$BIN/falcon-pl-ctl" gen-keys --keys-dir "$KEYS" --scheme "$SCHEME" \
      --validators "$VALS" --load-wallets 0
    for a in faucet alice bob community builder; do
      "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account "$a" --scheme "$SCHEME"
    done
    cat >"$RUN/genesis.env" <<EOF
NETWORK_ID=2300
SCHEME=falcon-512
PRODUCT=$WANT_VER
VALS=$VALS
EPOCH_MS=$EPOCH_MS
FIRST_CLAIM_EPOCH=$FIRST_CLAIM
UNBOND_COOLDOWN_MS=$UNBOND_MS
BOND=1000
PORTS=19301-19305
ROOT=$ROOT
# Do not load any 2200 snapshot.
EOF
    echo "[keys] wrote $KEYS and $RUN/genesis.env"
    echo "[keys] record validator publics:"
    python3 - "$KEYS" <<'PY'
import json,os,sys
kdir=sys.argv[1]
for fn in sorted(os.listdir(kdir)):
    if not fn.endswith(".json") or fn=="pl_keys.json":
        continue
    d=json.load(open(os.path.join(kdir,fn)))
    pk=d.get("public_key") or d.get("public") or ""
    print(" ", d.get("id") or d.get("account") or fn, (pk or "")[:32]+"…")
PY
    ;;
  start)
    [[ -x "$BIN/falcon-pl-node" && -f "$KEYS/v1.json" ]] || {
      echo "run: CONFIRM=2300 $0 keys" >&2
      exit 2
    }
    kill_by_exe_in_root falcon-pl-node
    sleep 1
    HUBS="127.0.0.1:19301,127.0.0.1:19302,127.0.0.1:19303"
    ALL="127.0.0.1:19301,127.0.0.1:19302,127.0.0.1:19303,127.0.0.1:19304,127.0.0.1:19305"
    i=1
    for port in 19301 19302 19303 19304 19305; do
      id="v$i"
      extra=""
      hub=""
      [[ "$id" == "v1" ]] && extra="--archive"
      [[ "$i" -le 3 ]] && hub="--hub"
      peers=$(python3 -c "print(','.join(p for p in '$ALL'.split(',') if p!='127.0.0.1:$port'))")
      : >"$LOG/${id}.log"
      # No --snapshot: clean genesis.
      nohup "$BIN/falcon-pl-node" --id "$id" $hub $extra \
        --listen "0.0.0.0:$port" --peers "$peers" \
        --validators "$VALS" --scheme "$SCHEME" --keys-dir "$KEYS" \
        --load-wallets 0 --bond 1000 --network-id "$NID" \
        --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
        --max-txs 128 --max-mempool-txs 50000 \
        --epoch-ms "$EPOCH_MS" --first-claim-epoch "$FIRST_CLAIM" \
        --unbond-cooldown-ms "$UNBOND_MS" \
        --snapshot-interval-ms 60000 --max-chain-ledgers 20000 \
        --hubs "$HUBS" --online-epoch-heights 10 --lock-public-wire \
        --admin-sock "$RUN/${id}.admin.sock" \
        --snapshot-out "$SNAP/${id}.json" \
        >"$LOG/${id}.log" 2>&1 &
      echo $! >"$RUN/${id}.pid"
      echo "[start] $id :$port pid=$(cat "$RUN/${id}.pid")"
      i=$((i+1))
    done
    echo "[start] public 2300 local 5-val. RPC 127.0.0.1:19301  (2200 on 19101 is untouched)"
    ;;
  expand|multi)
    F1_IP="${F1_IP:-$F1_TS}"
    F2_IP="${F2_IP:-$F2_TS}"
    F3_IP="${F3_IP:-$F3_TS}"
    SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
    ALL="${F1_IP}:19301,${F1_IP}:19302,${F2_IP}:19303,${F2_IP}:19304,${F3_IP}:19305"
    HUBS="${F1_IP}:19301,${F1_IP}:19302,${F2_IP}:19303"
    COMMON=(
      --validators "$VALS" --scheme "$SCHEME" --keys-dir "$KEYS"
      --load-wallets 0 --bond 1000 --network-id "$NID"
      --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000
      --max-txs 128 --max-mempool-txs 50000
      --epoch-ms "$EPOCH_MS" --first-claim-epoch "$FIRST_CLAIM"
      --unbond-cooldown-ms "$UNBOND_MS"
      --snapshot-interval-ms 60000 --max-chain-ledgers 20000
      --hubs "$HUBS" --online-epoch-heights 10 --lock-public-wire
    )
    exe_for() {
      case "$1" in
        v1) echo "$BIN/falcon-pl-archive" ;;
        v2|v3) echo "$BIN/falcon-pl-hub" ;;
        *) echo "$BIN/falcon-pl-val" ;;
      esac
    }
    peers_for() {
      python3 -c "print(','.join(p for p in '$ALL'.split(',') if p!='$1'))"
    }
    echo "[expand] stop local 2300 (2200 untouched)"
    kill_by_exe_in_root falcon-pl-node
    sleep 2
    echo "[expand] deploy bins+keys → $SSH_F2 $SSH_F3"
    install -m 0755 "$SRC_BIN/falcon-pl-node" "$BIN/falcon-pl-node"
    install -m 0755 "$SRC_BIN/falcon-pl-ctl" "$BIN/falcon-pl-ctl"
    ln -sfn falcon-pl-node "$BIN/falcon-pl-archive"
    ln -sfn falcon-pl-node "$BIN/falcon-pl-hub"
    ln -sfn falcon-pl-node "$BIN/falcon-pl-val"
    for h in "$SSH_F2" "$SSH_F3"; do
      ssh "${SSH_OPTS[@]}" "$h" "mkdir -p '$BIN' '$LOG' '$SNAP' '$RUN' '$KEYS'"
      rsync -az -e "ssh ${SSH_OPTS[*]}" \
        "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$h:$BIN/"
      ssh "${SSH_OPTS[@]}" "$h" "ln -sfn falcon-pl-node '$BIN/falcon-pl-archive'; ln -sfn falcon-pl-node '$BIN/falcon-pl-hub'; ln -sfn falcon-pl-node '$BIN/falcon-pl-val'"
      rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$h:$KEYS/"
    done
    start_local() {
      local id=$1 port=$2 extra=$3
      local exe peers
      exe=$(exe_for "$id")
      peers=$(peers_for "${F1_IP}:${port}")
      : >"$LOG/${id}.log"
      local snap=()
      [[ -f "$SNAP/${id}.json" ]] && snap=(--snapshot "$SNAP/${id}.json")
      nohup "$exe" --id "$id" $extra --listen "0.0.0.0:$port" \
        --peers "$peers" "${COMMON[@]}" \
        --admin-sock "$RUN/${id}.admin.sock" \
        "${snap[@]}" --snapshot-out "$SNAP/${id}.json" \
        >"$LOG/${id}.log" 2>&1 &
      echo $! >"$RUN/${id}.pid"
      echo "[expand] local $id exe=$(basename "$exe") :$port pid=$(cat "$RUN/${id}.pid")"
    }
    start_remote() {
      local host=$1 ip=$2 id=$3 port=$4 extra=$5
      local exe peers
      exe=$(exe_for "$id")
      peers=$(peers_for "${ip}:${port}")
      ssh "${SSH_OPTS[@]}" "$host" "rm -f '$RUN/${id}.admin.sock'; : >'$LOG/${id}.log'; nohup '$exe' --id $id $extra --listen 0.0.0.0:$port \
        --peers $peers \
        --validators $VALS --scheme $SCHEME --keys-dir '$KEYS' \
        --load-wallets 0 --bond 1000 --network-id $NID \
        --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
        --max-txs 128 --max-mempool-txs 50000 \
        --epoch-ms $EPOCH_MS --first-claim-epoch $FIRST_CLAIM \
        --unbond-cooldown-ms $UNBOND_MS \
        --snapshot-interval-ms 60000 --max-chain-ledgers 20000 \
        --hubs $HUBS --online-epoch-heights 10 --lock-public-wire \
        --admin-sock '$RUN/${id}.admin.sock' \
        --snapshot-out '$SNAP/${id}.json' \
        >'$LOG/${id}.log' 2>&1 & echo \$! >'$RUN/${id}.pid'; echo started $id pid=\$(cat '$RUN/${id}.pid')"
    }
    start_local v1 19301 "--hub --archive"
    start_local v2 19302 "--hub"
    start_remote "$SSH_F2" "$F2_IP" v3 19303 "--hub"
    start_remote "$SSH_F2" "$F2_IP" v4 19304 ""
    start_remote "$SSH_F3" "$F3_IP" v5 19305 ""
    echo "[expand] wait for remotes to join-snap"
    ready=0
    for i in $(seq 1 60); do
      t1=$("$BIN/falcon-pl-ctl" --addr 127.0.0.1:19301 status 2>/dev/null | python3 -c 'import sys,json
try: print(int(json.load(sys.stdin)["body"]["tip_height"]))
except: print(-1)' || echo -1)
      t5=$("$BIN/falcon-pl-ctl" --addr "${F3_IP}:19305" status 2>/dev/null | python3 -c 'import sys,json
try: print(int(json.load(sys.stdin)["body"]["tip_height"]))
except: print(-1)' || echo -1)
      echo "  try $i v1=$t1 droplet:v5=$t5"
      if [[ "$t1" -gt 0 && "$t5" -ge 0 && "$t1" -eq "$t5" ]]; then
        ready=1
        break
      fi
      # v5 caught up within 8 ledgers is fine
      if [[ "$t1" -gt 0 && "$t5" -gt 0 && $((t1 - t5)) -le 8 && $((t1 - t5)) -ge 0 ]]; then
        ready=1
        break
      fi
      sleep 3
    done
    echo "[expand] multi-host 2300 peers=$ALL"
    ;;
  stop)
    kill_by_exe_in_root falcon-pl-node
    echo "[stop] 2300 nodes in $ROOT only"
    ;;
  status)
    "$BIN/falcon-pl-ctl" --addr 127.0.0.1:19301 status 2>/dev/null | python3 -c '
import sys,json
b=json.load(sys.stdin).get("body",{})
print("id", b.get("id"), "ver", b.get("product_version"), "nid", b.get("network_id"),
      "tip", b.get("tip_height"), "epoch_ms", b.get("epoch_ms"),
      "first_claim", b.get("first_claim_epoch"), "unbond", b.get("unbond_cooldown_ms"))
' || echo "2300 RPC down (start not run, or not yet up)"
    echo "2200 still:" 
    "$HOME/falcon-pl-private-testnet-v22/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 status 2>/dev/null | python3 -c '
import sys,json
b=json.load(sys.stdin).get("body",{})
print("  ver", b.get("product_version"), "nid", b.get("network_id"), "tip", b.get("tip_height"))
' || echo "  2200 RPC down"
    ;;
  *)
    echo "usage: CONFIRM=2300 $0 keys|start|status|stop" >&2
    exit 2
    ;;
esac
