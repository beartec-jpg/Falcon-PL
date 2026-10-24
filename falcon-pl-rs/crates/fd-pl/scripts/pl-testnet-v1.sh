#!/usr/bin/env bash
# Falcon PL testnet v1 — dual host (host1 + host2).
#
# Layout:
#   F1 (this host): v1 :19101, v2 :19102
#   F2 (SSH):       v3 :19103
#   Wallets/load:   this host (or any client with keys + RPC)
#
#   bash crates/fd-pl/scripts/pl-testnet-v1.sh start
#   bash crates/fd-pl/scripts/pl-testnet-v1.sh status
#   bash crates/fd-pl/scripts/pl-testnet-v1.sh load
#   bash crates/fd-pl/scripts/pl-testnet-v1.sh stop
set -euo pipefail

RS="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_ROOT:-$HOME/falcon-pl-testnet}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SCHEME="${PL_SCHEME:-hmac}"
LOAD_WALLETS="${PL_LOAD_WALLETS:-32}"
SSH_F2="${SSH_F2:-100.64.0.3}"
# Advertise LAN IPs for peer mesh (override if needed)
F1_IP="${F1_IP:-192.0.2.1}"
F2_IP="${F2_IP:-192.0.2.2}"
SEAL_MS="${PL_SEAL_MS:-400}"
MAX_TXS="${PL_MAX_TXS:-0}"                    # 0 = no hard cap
PACK_BUDGET_MS="${PL_PACK_BUDGET_MS:-1000}"    # the real limit: pack all you can in this many ms
# No tip advance for this long (with mempool work) → skip #1, next ponger packs
PROGRESS_TIMEOUT_MS="${PL_PROGRESS_TIMEOUT_MS:-1000}"
# Cap pending txs/node so Falcon-sized floods cannot OOM (reject when full)
MAX_MEMPOOL_TXS="${PL_MAX_MEMPOOL_TXS:-50000}"
BOND=1000
VALS="v1,v2,v3"

cmd="${1:-start}"

mkdir -p "$ROOT" "$LOG" "$KEYS"

build() {
  cd "$RS"
  cargo build -p fd-pl --release 2>&1 | tail -6
}

gen_keys() {
  rm -rf "$KEYS"
  mkdir -p "$KEYS"
  "$BIN/falcon-pl-ctl" gen-keys \
    --keys-dir "$KEYS" \
    --scheme "$SCHEME" \
    --validators v1,v2,v3 \
    --load-wallets "$LOAD_WALLETS"
}

stop_local() {
  for f in "$LOG"/v1.pid "$LOG"/v2.pid; do
    if [[ -f "$f" ]]; then
      kill "$(cat "$f")" 2>/dev/null || true
      rm -f "$f"
    fi
  done
  for p in 19101 19102; do fuser -k "${p}/tcp" 2>/dev/null || true; done
}

stop_remote() {
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$SSH_F2" \
    "if [[ -f $ROOT/logs/v3.pid ]]; then kill \$(cat $ROOT/logs/v3.pid) 2>/dev/null || true; rm -f $ROOT/logs/v3.pid; fi; fuser -k 19103/tcp 2>/dev/null || true" \
    2>/dev/null || true
}

sync_remote() {
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$SSH_F2" "mkdir -p $ROOT/bin $ROOT/keys $ROOT/logs"
  rsync -az "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$SSH_F2:$ROOT/bin/"
  rsync -az --delete "$KEYS/" "$SSH_F2:$ROOT/keys/"
}

start_local() {
  local common=(
    --validators "$VALS"
    --scheme "$SCHEME"
    --keys-dir "$KEYS"
    --load-wallets "$LOAD_WALLETS"
    --seal-interval-ms "$SEAL_MS"
    --max-txs "$MAX_TXS"
    --pack-budget-ms "$PACK_BUDGET_MS"
    --progress-timeout-ms "$PROGRESS_TIMEOUT_MS"
    --max-mempool-txs "$MAX_MEMPOOL_TXS"
    --bond "$BOND"
  )
  nohup "$BIN/falcon-pl-node" --id v1 \
    --listen "0.0.0.0:19101" \
    --peers "${F1_IP}:19102,${F2_IP}:19103" \
    "${common[@]}" >>"$LOG/v1.log" 2>&1 &
  echo $! >"$LOG/v1.pid"
  nohup "$BIN/falcon-pl-node" --id v2 \
    --listen "0.0.0.0:19102" \
    --peers "${F1_IP}:19101,${F2_IP}:19103" \
    "${common[@]}" >>"$LOG/v2.log" 2>&1 &
  echo $! >"$LOG/v2.pid"
  echo "F1 started v1/v2 pids $(cat "$LOG/v1.pid") $(cat "$LOG/v2.pid")"
}

start_remote() {
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$SSH_F2" bash -s <<EOF
set -euo pipefail
ROOT="$ROOT"
SCHEME="$SCHEME"
LOAD_WALLETS="$LOAD_WALLETS"
SEAL_MS="$SEAL_MS"
MAX_TXS="$MAX_TXS"
PACK_BUDGET_MS="$PACK_BUDGET_MS"
PROGRESS_TIMEOUT_MS="$PROGRESS_TIMEOUT_MS"
MAX_MEMPOOL_TXS="$MAX_MEMPOOL_TXS"
F1_IP="$F1_IP"
F2_IP="$F2_IP"
BOND="$BOND"
if [[ -f "\$ROOT/logs/v3.pid" ]]; then kill "\$(cat "\$ROOT/logs/v3.pid")" 2>/dev/null || true; fi
fuser -k 19103/tcp 2>/dev/null || true
sleep 0.3
nohup "\$ROOT/bin/falcon-pl-node" --id v3 \\
  --listen 0.0.0.0:19103 \\
  --peers \${F1_IP}:19101,\${F1_IP}:19102 \\
  --validators v1,v2,v3 \\
  --scheme "\$SCHEME" \\
  --keys-dir "\$ROOT/keys" \\
  --load-wallets "\$LOAD_WALLETS" \\
  --seal-interval-ms "\$SEAL_MS" \\
  --max-txs "\$MAX_TXS" \\
  --pack-budget-ms "\$PACK_BUDGET_MS" \\
  --progress-timeout-ms "\$PROGRESS_TIMEOUT_MS" \\
  --max-mempool-txs "\$MAX_MEMPOOL_TXS" \\
  --bond "\$BOND" >>"\$ROOT/logs/v3.log" 2>&1 &
echo \$! >"\$ROOT/logs/v3.pid"
echo "F2 started v3 pid=\$(cat \$ROOT/logs/v3.pid)"
EOF
}

status() {
  echo "=== v1 (F1) ==="
  "$BIN/falcon-pl-ctl" --addr "127.0.0.1:19101" status 2>/dev/null | head -50 || echo DOWN
  echo "=== v2 (F1) ==="
  "$BIN/falcon-pl-ctl" --addr "127.0.0.1:19102" status 2>/dev/null | head -20 || echo DOWN
  echo "=== v3 (F2 ${F2_IP}) ==="
  "$BIN/falcon-pl-ctl" --addr "${F2_IP}:19103" status 2>/dev/null | head -20 || echo DOWN
}

case "$cmd" in
  start)
    build
    stop_local
    stop_remote
    gen_keys
    sync_remote
    start_local
    start_remote
    sleep 2
    status
    echo
    echo "RPC:  ${F1_IP}:19101  (primary)"
    echo "keys:  $KEYS"
    echo "load:  bash $0 load"
    echo "stop:  bash $0 stop"
    ;;
  stop)
    stop_local
    stop_remote
    echo "stopped"
    ;;
  status)
    status
    ;;
  load)
    # Falcon-512 uses Rust load-net (real PQ sigs). HMAC can use python or rust.
    # PL_LOAD_RATE=0 → dump; e.g. PL_LOAD_RATE=50 for steady.
    if [[ "$SCHEME" == falcon* || "$SCHEME" == "pq" ]]; then
      "$BIN/falcon-pl-load-net" \
        --keys "$KEYS" \
        --scheme falcon-512 \
        --rpc "127.0.0.1:19101,127.0.0.1:19102,${F2_IP}:19103" \
        --wallets "$LOAD_WALLETS" \
        --seconds "${PL_LOAD_SECONDS:-20}" \
        --workers "${PL_LOAD_WORKERS:-4}" \
        --rate "${PL_LOAD_RATE:-50}" \
        --drain-wait "${PL_DRAIN_WAIT:-60}" \
        --amount 1 --fee 1
    else
      python3 "$RS/crates/fd-pl/scripts/pl-testnet-load.py" \
        --rpc "127.0.0.1:19101,127.0.0.1:19102,${F2_IP}:19103" \
        --keys "$KEYS" \
        --scheme "$SCHEME" \
        --wallets "$LOAD_WALLETS" \
        --seconds "${PL_LOAD_SECONDS:-30}" \
        --workers "${PL_LOAD_WORKERS:-8}" \
        --drain-wait "${PL_DRAIN_WAIT:-90}" \
        --rate "${PL_LOAD_RATE:-0}" \
        --amount 1 \
        --fee 1
    fi
    ;;
  sync)
    build
    sync_remote
    ;;
  *)
    echo "usage: $0 start|stop|status|load|sync"
    exit 2
    ;;
esac
