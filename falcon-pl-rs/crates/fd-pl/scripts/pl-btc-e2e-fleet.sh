#!/usr/bin/env bash
# Local 3-val fleet for BTC vault e2e. Does not touch live 2300.
set -euo pipefail
RS="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_E2E_ROOT:-$HOME/falcon-pl-vault-test/e2e}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
VAULT_PUB="$HOME/falcon-pl-vault-test/keys/vault-pub.json"
SCHEME=hmac
NETWORK_ID="${PL_NETWORK_ID:-2300}"

cmd="${1:-start}"
mkdir -p "$ROOT" "$LOG" "$KEYS" "$SNAP"

build() {
  cd "$RS"
  cargo build -p fd-pl --release --bins
}

gen() {
  rm -rf "$KEYS"
  mkdir -p "$KEYS"
  "$BIN/falcon-pl-ctl" gen-keys --keys-dir "$KEYS" --scheme "$SCHEME" \
    --validators v1,v2,v3 --load-wallets 8
  "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account sally --scheme "$SCHEME"
  cp -a "$VAULT_PUB" "$KEYS/vault-pub.json"
}

stop() {
  for f in "$LOG"/v1.pid "$LOG"/v2.pid "$LOG"/v3.pid; do
    [[ -f "$f" ]] && kill "$(cat "$f")" 2>/dev/null || true
    rm -f "$f"
  done
  for p in 19401 19402 19403; do fuser -k "${p}/tcp" 2>/dev/null || true; done
}

start() {
  local common=(
    --validators v1,v2,v3 --scheme "$SCHEME" --keys-dir "$KEYS"
    --load-wallets 8 --seal-interval-ms 400 --bond 1000
    --network-id "$NETWORK_ID" --allow-dev-crypto --allow-admin-wire
    --first-claim-epoch 1
  )
  nohup "$BIN/falcon-pl-node" --id v1 --listen 0.0.0.0:19401 \
    --peers 127.0.0.1:19402,127.0.0.1:19403 \
    --snapshot "$SNAP/v1.json" --snapshot-out "$SNAP/v1.json" \
    "${common[@]}" >>"$LOG/v1.log" 2>&1 &
  echo $! >"$LOG/v1.pid"
  nohup "$BIN/falcon-pl-node" --id v2 --listen 0.0.0.0:19402 \
    --peers 127.0.0.1:19401,127.0.0.1:19403 \
    --snapshot "$SNAP/v2.json" --snapshot-out "$SNAP/v2.json" \
    "${common[@]}" >>"$LOG/v2.log" 2>&1 &
  echo $! >"$LOG/v2.pid"
  nohup "$BIN/falcon-pl-node" --id v3 --listen 0.0.0.0:19403 \
    --peers 127.0.0.1:19401,127.0.0.1:19402 \
    --snapshot "$SNAP/v3.json" --snapshot-out "$SNAP/v3.json" \
    "${common[@]}" >>"$LOG/v3.log" 2>&1 &
  echo $! >"$LOG/v3.pid"
  echo "e2e fleet 19401-3 pids $(cat "$LOG/v1.pid") $(cat "$LOG/v2.pid") $(cat "$LOG/v3.pid")"
}

case "$cmd" in
  build) build ;;
  gen) gen ;;
  start) start ;;
  stop) stop ;;
  restart) stop; start ;;
  bootstrap) build; gen; stop; start ;;
  *) echo "usage: $0 build|gen|start|stop|restart|bootstrap" >&2; exit 2 ;;
esac
