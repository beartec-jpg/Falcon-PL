#!/usr/bin/env bash
# Watcher action against another operator's bad kickoff/claim (no matching Falcon withdrawal,
# mismatched binding, PEG_OUT not paying the exact payout script / too little, or missed deadline).
# Env (from fpl-operatord): FPL_CHALLENGE_NOTE, FPL_CHALLENGE_OPERATOR (x-only), FPL_CHALLENGE_DEPOSIT
# (peg-in confirm outpoint), FPL_CHALLENGE_REASON, FPL_OP_HOME, FPL_NETWORK, FPL_ESPLORA,
# FPL_OP_BITCOIN_CLI (cli + args + -rpcwallet), FPL_DRY_RUN (1 = plan only), FPL_OPERATOR_SECRET
# (never echoed). Optional: FPL_CHALLENGE_VK_DIR (vk.bin) -> follow the graph in the background and
# broadcast Disprove (non-standard: miner channels from $FPL_OP_HOME/submit.json).
# BitVM2 path: fpl_challenge funds the crowdfunding input of the presigned CHALLENGE from this
# operator's own wallet and broadcasts it (forcing the operator to ASSERT); a wrong assertion is
# DISPROVED (connector C leaf / FPL binding leaf), burning the operator's reimbursement.
# fpl_challenge prints CHALLENGE_TXID / ALREADY_CHALLENGED / DRY_RUN; exit 75 = retry later.
set -uo pipefail
LOG="${FPL_OP_HOME:?}/logs/challenges.log"
echo "$(date -u +%FT%TZ) note=$FPL_CHALLENGE_NOTE operator=$FPL_CHALLENGE_OPERATOR deposit=${FPL_CHALLENGE_DEPOSIT:-} reason=$FPL_CHALLENGE_REASON dry=${FPL_DRY_RUN:-1}" >>"$LOG"
BIN="$FPL_OP_HOME/bin/fpl_challenge"
if [[ ! -x "$BIN" ]]; then echo "recorded (no fpl_challenge binary installed)"; exit 0; fi
if [[ -z "${FPL_CHALLENGE_DEPOSIT:-}" ]]; then echo "recorded (no deposit outpoint in the record; nothing to challenge on-chain)"; exit 0; fi
ARGS=(--network "${FPL_NETWORK:-testnet4}" --deposit "$FPL_CHALLENGE_DEPOSIT" --operator "$FPL_CHALLENGE_OPERATOR")
[[ -n "${FPL_ESPLORA:-}" ]] && ARGS+=(--esplora "$FPL_ESPLORA")
out=$("$BIN" "${ARGS[@]}" 2>>"$LOG"); rc=$?
echo "$out" | tee -a "$LOG"
if [[ $rc == 0 && -n "${FPL_CHALLENGE_VK_DIR:-}" && "${FPL_DRY_RUN:-1}" != 1 ]] && grep -qE '^(CHALLENGE_TXID|ALREADY_CHALLENGED) ' <<<"$out"; then
  export FPL_PROOF_DIR="${FPL_PROOF_DIR:-$FPL_CHALLENGE_VK_DIR}"   # disprove scripts use the deployment VK
  setsid nohup "$BIN" "${ARGS[@]}" --follow --vk-dir "$FPL_CHALLENGE_VK_DIR" >>"$FPL_OP_HOME/logs/disprove-$FPL_CHALLENGE_DEPOSIT.log" 2>&1 </dev/null &
fi
exit $rc
