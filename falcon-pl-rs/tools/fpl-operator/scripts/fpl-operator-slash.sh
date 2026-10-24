#!/usr/bin/env bash
# BRIDGE: submit slash evidence for a mined BitVM2 Disprove / DisproveChain.
# Env (set by fpl-operatord): FPL_SLASH_TXID, FPL_SLASH_OPERATOR (cheater x-only),
# FPL_SLASH_DEPOSIT (peg-in confirm txid), FPL_OP_BITCOIN_CLI, FPL_OP_FALCON_*.
# The ledger re-verifies everything (SPV >= 6 confs, tx shape vs the bound graph);
# exits non-zero until the Disprove is deep enough, and the daemon retries.
set -euo pipefail
BIN=${FPL_OP_BIN_DIR:-$(dirname "$0")}
ev=$(python3 "$BIN/slash_evidence.py" --cli "$FPL_OP_BITCOIN_CLI" --txid "$FPL_SLASH_TXID" \
       --operator-xonly "$FPL_SLASH_OPERATOR" --deposit "$FPL_SLASH_DEPOSIT")
body=$(python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["body"]))' <<<"$ev")
kd=(); [[ -n "${FPL_OP_FALCON_KEYS_DIR:-}" ]] && kd=(--keys-dir "$FPL_OP_FALCON_KEYS_DIR")
exec "$FPL_OP_FALCON_CTL" --addr "$FPL_OP_FALCON_RPC" operator --account "$FPL_OP_FALCON_ACCOUNT" \
  --scheme "${FPL_OP_FALCON_SCHEME:-falcon}" --network-id "${FPL_OP_FALCON_NETWORK_ID:-2300}" "${kd[@]}" --body "$body"
