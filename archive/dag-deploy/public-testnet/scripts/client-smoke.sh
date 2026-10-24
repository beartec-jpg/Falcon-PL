#!/usr/bin/env bash
# End-user style client checks against public entrypoints.
set -euo pipefail
FACADE="${FACADE_URL:-http://127.0.0.1:18499}"
FAUCET="${FAUCET_URL:-http://127.0.0.1:18550}"
RPC="${PUBLIC_RPC:-http://127.0.0.1:18001}"

echo "1) network via faucet"
curl -sS "$FAUCET/v1/info" | python3 -m json.tool | head -30

echo "2) route acct3 / acct23"
curl -sS -X POST "$FACADE/v1/route" -H 'Content-Type: application/json' \
  -d '{"account":"acct3"}' | python3 -m json.tool
curl -sS -X POST "$FACADE/v1/route" -H 'Content-Type: application/json' \
  -d '{"account":"acct23"}' | python3 -m json.tool

echo "3) faucet drip → acct8"
curl -sS -X POST "$FAUCET/v1/drip" -H 'Content-Type: application/json' \
  -d '{"destination":"acct8","amount":5}' | python3 -m json.tool

echo "4) account_info acct8"
sleep 2
curl -sS -X POST "$RPC/" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"account_info","params":[{"account":"acct8"}]}' \
  | python3 -m json.tool | head -40

echo "client-smoke done"
