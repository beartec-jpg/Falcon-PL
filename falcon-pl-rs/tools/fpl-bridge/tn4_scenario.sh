#!/bin/bash
# testnet4 live scenario (runbook a=take1 / e=timeouts). "testnet single-party setup".
# Per-run fresh operator + depositor wallet keys (0600 files in the run dir, never printed);
# n-of-n = depositor ephemeral + fpl-signer ephemeral (RAM-only, wiped).
# Usage: tn4_scenario.sh take1|timeouts|disprove [DEPOSIT_SATS]
# disprove (runbook f, dishonest operator): bad KICK_OFF_1 -> watcher Challenge (fpl_challenge, own
#   fresh key) -> operator Asserts an invalid proof -> watcher Disprove (fpl_challenge --follow,
#   non-standard tx: needs a mining channel in FPL_SUBMIT, e.g. brain's ckpool submit.json on
#   testnet4) -> coordinator checks Take1 and Take2 are both impossible.
#   Env: FPL_CHALLENGE_CLI (bitcoin-cli + wallet that funds the Challenge crowdfunding input;
#   defaults to $B), FPL_SUBMIT (SubmitConfig JSON), FPL_VK_DIR (vk.bin dir, default FPL_PROOF_DIR).
# FPL_NETWORK=regtest runs the same flow on the box regtest (esplora shim :8094, miner wallet).
# FPL_NETWORK=signet runs it on a test signet: default = our private box signet (signet_setup.md:
#   std node + own-signer miner node with -acceptnonstdtxn=1, esplora shim :8095, 'fund' wallet).
#   FPL_SIG_DIR, FPL_ESPLORA (e.g. a public signet esplora), FPL_EXPLORER (tx link prefix) override.
#   Timelocks: FPL_REGTEST_TIMELOCK_DIVISOR also scales signet (unset = upstream 2 blocks).
# Env (kit 0.4.0 runs): FPL_EX (BitVM examples dir), FPL_RUN (run dir), FPL_PROOF_DIR (VK/proof dir),
#   FPL_FUND_SSH (e.g. "sudo tailscale ssh root@HOST": fund from that operator's fpl-operator wallet instead of
#   the box 'fund' wallet), FPL_TIMEOUT_SECS.
set -euo pipefail
SC=$1; AMT=${2:-10000}
EX=${FPL_EX:-/workspace/Falcon-PL/falcon-pl-rs/vendor/BitVM/target/release/examples}
NET=${FPL_NETWORK:-testnet4}
if [ "$NET" = regtest ]; then
  B="bitcoin-cli -datadir=${FPL_BTC_DATADIR:-/workspace/deploy/btc-keyless/regtest} -rpcwallet=miner"
elif [ "$NET" = signet ]; then
  B="bitcoin-cli -datadir=${FPL_SIG_DIR:-/workspace/deploy/bridge-int/signet}/std -rpcwallet=fund"
elif [ -n "${FPL_FUND_SSH:-}" ]; then
  B=rb; rb() { $FPL_FUND_SSH bash -s <<<"/opt/fpl-tn4btc/bin/bitcoin-cli -testnet4 -datadir=/opt/fpl-tn4btc/data -conf=/opt/fpl-tn4btc/bitcoin.conf -rpcport=48342 -rpcwallet=fpl-operator $(printf '%q ' "$@")"; }
else B="bitcoin-cli -datadir=/workspace/deploy/btc-keyless/testnet4 -rpcwallet=fund"; fi
case $NET in regtest) ESP=http://127.0.0.1:8094/regtest/api/; XL=local:;; signet) ESP=${FPL_ESPLORA:-http://127.0.0.1:8095/signet/api/}; XL=${FPL_EXPLORER:-local-signet:};;
  *) ESP=https://mempool.space/testnet4/api/; XL=https://mempool.space/testnet4/tx/;; esac
RUN=${FPL_RUN:-/workspace/deploy/btc-keyless/testnet4/scenario_$SC}; mkdir -p $RUN; cd $RUN
umask 077
[ -f operator.secret ] || openssl rand -hex 32 > operator.secret
[ -f depositor_wallet.secret ] || openssl rand -hex 32 > depositor_wallet.secret
export FPL_OPERATOR_SECRET=$(cat operator.secret) FPL_PROOF_DIR=${FPL_PROOF_DIR:-/workspace/deploy/btc-keyless/fplproof-v3}
D=$RUN/exchange; mkdir -p $D
# Graph-binding gate: pre-activation the ledger has no bindings, so the scenario
# must opt out explicitly; once the bridge is active pass FPL_BIND_ARGS="--falcon-ctl <ctl> [--falcon-rpc URL]".
BIND=${FPL_BIND_ARGS:---no-binding-check}
C="$EX/fpl_ceremony_coordinator --dir $D --network $NET --esplora $ESP $BIND"
info=$($C --print-funding-addresses); echo "$info"
OA=$(echo "$info" | awk '/^operator/{print $2}'); PIF=$(echo "$info" | awk '/^peg_in_fee/{print $2}'); POF=$(echo "$info" | awk '/^peg_out_fee/{print $2}')
DA=$(FPL_DEPOSITOR_WALLET_SECRET=$(cat depositor_wallet.secret) $EX/fpl_depositor --network $NET --print-address)
echo "depositor $DA"
sendto() { $B -named send outputs="{\"$1\":$(amt $2)}" fee_rate=${FPL_FEE_RATE:-1} include_unsafe=true | python3 -c "import json,sys;print(json.load(sys.stdin)['txid'])"; }
amt() { python3 -c "print('%.8f' % ($1/1e8))"; }
vout() { [ "$NET" = regtest ] && { $B getrawtransaction $1 1 | python3 -c "import json,sys;t=json.load(sys.stdin);print([o['n'] for o in t['vout'] if o['scriptPubKey'].get('address')=='$2'][0])"; return; }; $B gettransaction $1 | python3 -c "import json,sys;t=json.load(sys.stdin);print([d['vout'] for d in t['details'] if d['address']=='$2'][0])"; }
if [ -n "${DEP_FUND:-}" ]; then
  # reuse an existing depositor funding outpoint (must be AMT+PIF at this run's depositor key)
  DFT=${DEP_FUND%:*}; dv=${DEP_FUND#*:}
  KT=$(sendto $OA $((AMT+POF))); kv=$(vout $KT $OA)
  echo "FUNDING(operator) $KT $XL$KT (depositor reused $DEP_FUND)"
else
  DFT=$($B -named sendmany amounts="{\"$DA\":$(amt $((AMT+PIF))),\"$OA\":$(amt $((AMT+POF)))}" fee_rate=${FPL_FEE_RATE:-1})
  KT=$DFT; dv=$(vout $DFT $DA); kv=$(vout $KT $OA)
  echo "FUNDING $DFT $XL$DFT"
fi
FPL_DEPOSITOR_WALLET_SECRET=$(cat depositor_wallet.secret) $EX/fpl_depositor --network $NET --dir $D --funding $DFT:$dv --amount $((AMT+PIF)) --evm-address 0x70997970C51812dc3A010C7d01b50e0d17dc79C8 --timeout-secs ${FPL_TIMEOUT_SECS:-7200} > depositor.log 2>&1 &
$EX/fpl_ceremony_participant --dir $D --id fpl-signer --timeout-secs ${FPL_TIMEOUT_SECS:-7200} > fpl-signer.log 2>&1 &
$C --deposit-outpoint $DFT:$dv --deposit-amount $((AMT+PIF)) --evm-address 0x70997970C51812dc3A010C7d01b50e0d17dc79C8 \
   --kickoff-outpoint $KT:$kv --kickoff-amount $((AMT+POF)) --broadcast --scenario $SC --timeout-secs ${FPL_TIMEOUT_SECS:-7200} > coordinator.log 2>&1 &
CP=$!
# fund the operator's withdrawal payout when asked
while kill -0 $CP 2>/dev/null; do
  l=$(grep -m1 '^PAYOUT_NEEDED' coordinator.log || true)
  if [ -n "$l" ] && [ ! -f $D/payout.txt ]; then
    set -- $l; PT=$(sendto $2 $3)
    pv=$(vout $PT $2)
    echo "PAYOUT_FUNDING $PT $XL$PT"; echo "$PT:$pv" > $D/payout.txt
  fi
  if [ "$SC" = disprove ]; then
    c=$(grep -m1 '^CHALLENGE_NEEDED' coordinator.log || true)
    if [ -n "$c" ] && [ ! -f $D/challenge.txt ]; then
      set -- $c; [ -f watcher.secret ] || openssl rand -hex 32 > watcher.secret
      WCLI=${FPL_CHALLENGE_CLI:-$B}; [ "$WCLI" = rb ] && { echo "FPL_FUND_SSH set: give FPL_CHALLENGE_CLI (a local bitcoin-cli command line)"; exit 2; }; SUB=(); [ -n "${FPL_SUBMIT:-}" ] && SUB=(--submit $FPL_SUBMIT)
      WA=(--network $NET --deposit $2 --operator $3 --graphs $RUN --esplora ${ESP%/} --bitcoin-cli "$WCLI" --state $RUN/watcher_state.json "${SUB[@]}")
      o=$(FPL_OPERATOR_SECRET=$(cat watcher.secret) $EX/fpl_challenge "${WA[@]}" 2>>watcher.log || true); echo "$o" >> watcher.log
      t=$(echo "$o" | awk '/^(CHALLENGE_TXID|ALREADY_CHALLENGED)/{print $2; exit}')
      if [ -n "$t" ] && [ "$t" != spent ]; then
        echo "CHALLENGE $t"; echo "$t" > $D/challenge.txt
        FPL_OPERATOR_SECRET=$(cat watcher.secret) $EX/fpl_challenge "${WA[@]}" --vk-dir ${FPL_VK_DIR:-$FPL_PROOF_DIR} --follow --poll-secs ${FPL_WATCH_POLL:-60} >> watcher.log 2>&1 &
      fi
    fi
    w=$(grep -m1 '^DISPROVE_TXID' watcher.log 2>/dev/null || true)
    if [ -n "$w" ] && [ ! -f $D/disprove.txt ]; then set -- $w; echo "DISPROVE $2"; echo "$2" > $D/disprove.txt; fi
  fi
  sleep 10
done
RC=0; wait $CP || RC=$?
grep -E "^TX |SCENARIO|peg-in confirm|attestation|take1 after|take2 after|CHALLENGE_|ASSERTED|DISPROVE_" coordinator.log
[ "$SC" = disprove ] && { grep -E "CHALLENGE_TXID|DISPROVE_TXID|GRAPH_DONE" watcher.log || true; }
exit $RC
