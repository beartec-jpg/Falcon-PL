#!/usr/bin/env bash
# Graph steps for one fronted withdrawal: PEG_OUT (pays the withdrawer from the operator-funded
# UTXO) -> PEG_OUT_CONFIRM -> KICK_OFF_1 -> START_TIME -> KICK_OFF_2 -> TAKE_1.
# Called by fpl-operatord after the peer cross-check passed. Env: FPL_OPERATOR_SECRET (never echoed),
# FPL_CLAIM_NOTE_ID, FPL_PAYOUT_OUTPOINT, FPL_DEPOSIT_DIR (presigned deposit graph chosen by the
# deposit lookup), FPL_FALCON_CLAIMS (claim taken from the Falcon ledger: the coordinator's
# FalconAdaptor turns it into the peg-out event; no mock), FPL_NETWORK, FPL_ESPLORA, FPL_OP_HOME.
set -euo pipefail
: "${FPL_OP_HOME:?}" "${FPL_CLAIM_NOTE_ID:?}" "${FPL_PAYOUT_OUTPOINT:?}" "${FPL_DEPOSIT_DIR:?}" "${FPL_FALCON_CLAIMS:?}"
[[ -f "$FPL_DEPOSIT_DIR/final.json" ]] || { echo "no presigned graph in $FPL_DEPOSIT_DIR" >&2; exit 3; }
LOG="$FPL_OP_HOME/logs/graph-$FPL_CLAIM_NOTE_ID.log"
setsid nohup "$FPL_OP_HOME/bin/fpl_ceremony_coordinator" --dir "$FPL_DEPOSIT_DIR" --network "${FPL_NETWORK:-testnet4}" \
  --esplora "${FPL_ESPLORA}" --scenario take1 --falcon-claims "$FPL_FALCON_CLAIMS" \
  --payout-outpoint "$FPL_PAYOUT_OUTPOINT" >>"$LOG" 2>&1 </dev/null &
echo "started graph steps pid $! (log $LOG)"
