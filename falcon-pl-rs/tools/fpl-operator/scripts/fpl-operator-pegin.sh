#!/usr/bin/env bash
# BRIDGE BTC4: submit the SPV peg-in mint for a graph PegInConfirm (permissionless;
# the ledger mints to the depositor account all operators bound, never to the submitter).
# Env (set by fpl-operatord): FPL_PEGIN_TXID, FPL_OP_BITCOIN_CLI, FPL_OP_FALCON_*.
# Exits non-zero below 6 confirmations; the daemon retries.
set -euo pipefail
BIN=${FPL_OP_BIN_DIR:-$(dirname "$0")}
ev=$(python3 "$BIN/slash_evidence.py" --cli "$FPL_OP_BITCOIN_CLI" --txid "$FPL_PEGIN_TXID" --pegin)
conf=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["confirmations"])' <<<"$ev")
[[ "$conf" -ge 6 ]] || { echo "peg-in $FPL_PEGIN_TXID has $conf confirmations (< 6)" >&2; exit 3; }
body=$(python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["body"]))' <<<"$ev")
kd=(); [[ -n "${FPL_OP_FALCON_KEYS_DIR:-}" ]] && kd=(--keys-dir "$FPL_OP_FALCON_KEYS_DIR")
exec "$FPL_OP_FALCON_CTL" --addr "$FPL_OP_FALCON_RPC" operator --account "$FPL_OP_FALCON_ACCOUNT" \
  --scheme "${FPL_OP_FALCON_SCHEME:-falcon}" --network-id "${FPL_OP_FALCON_NETWORK_ID:-2300}" "${kd[@]}" --body "$body"
