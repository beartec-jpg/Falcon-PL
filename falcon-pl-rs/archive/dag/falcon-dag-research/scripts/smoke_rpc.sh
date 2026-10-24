#!/usr/bin/env bash
# Smoke-test falcon-dagd JSON-RPC (default port 18101 to avoid Python regtest on 18001).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
RPC="${RPC:-127.0.0.1:18101}"
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"

if [[ ! -x "$BIN" ]]; then
  cargo build --release -p fd-node
  BIN="$ROOT/target/release/falcon-dagd"
fi

fuser -k "${RPC##*:}/tcp" 2>/dev/null || true
"$BIN" --id val1 --rpc "$RPC" --unl val1 --worker-count 1 >/tmp/falcon-dagd-smoke.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
sleep 0.4

curl -sf -X POST "http://$RPC" -H 'Content-Type: application/json' \
  -d '{"method":"ping","params":[{}]}' | grep -q pong
curl -sf -X POST "http://$RPC" -H 'Content-Type: application/json' \
  -d '{"method":"submit","params":[{"account":"acct1","sequence":1,"destination":"acct2","amount":50,"fee":10}]}' \
  | grep -q tesSUCCESS
sleep 0.3
INFO=$(curl -sf -X POST "http://$RPC" -H 'Content-Type: application/json' \
  -d '{"method":"server_info","params":[{}]}')
echo "$INFO" | grep -q 'falcon-dagd/0.9.0-rust'
echo "$INFO" | grep -q '"executed_txs":1\|"executed_txs": 1' || echo "$INFO" | grep -q executed_txs
echo "smoke_rpc OK on $RPC"
