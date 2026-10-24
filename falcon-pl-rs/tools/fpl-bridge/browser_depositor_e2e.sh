#!/bin/bash
# Regtest e2e of the BROWSER-DEPOSITOR mode (Scott 2026-10-20 key plan):
#   n-of-n = three operator seats only (host1, host2, seat3; here
#   simulated by three box participants with RAM-only keys), depositor key =
#   browser wallet, simulated by browser-depositor/sign.mjs (@scure/btc-signer,
#   the same code the site runs). The intake (fpl_depositor_ext) holds no secret.
# Flow: fund the browser key's P2WSH address -> prepare (PSBTs) -> browser signs
#   -> finish (verify, peg_in_signed.json) -> coordinator presigns with the 3 seats,
#   broadcasts PegInDeposit + PegInConfirm, then runs take1 (operator fronts the
#   withdrawal payout, KickOff, Take1).
# Usage: browser_depositor_e2e.sh [AMOUNT_SATS]   env FPL_EX (examples dir), FPL_RUN.
set -euo pipefail
AMT=${1:-100000}
HERE=$(cd "$(dirname "$0")" && pwd)
EX=${FPL_EX:?FPL_EX}
RUN=${FPL_RUN:-/workspace/deploy/bridge-int/browser-dep-1}; mkdir -p $RUN; cd $RUN; umask 077
B="bitcoin-cli -datadir=${FPL_BTC_DATADIR:-/workspace/deploy/btc-keyless/regtest} -rpcwallet=miner"
ESP=http://127.0.0.1:8094/regtest/api/
SEATS=host1,host2,seat3
D=$RUN/exchange; mkdir -p $D
[ -f operator.secret ] || openssl rand -hex 32 > operator.secret
[ -f browser.secret ] || openssl rand -hex 32 > browser.secret   # stands in for the browser's key (never printed)
export FPL_OPERATOR_SECRET=$(cat operator.secret) FPL_PROOF_DIR=${FPL_PROOF_DIR:-/workspace/deploy/btc-keyless/fplproof-v3}
export FPL_REGTEST_TIMELOCK_DIVISOR=${FPL_REGTEST_TIMELOCK_DIVISOR:-}
C="$EX/fpl_ceremony_coordinator --dir $D --network regtest --esplora $ESP --no-binding-check --seats $SEATS"
info=$($C --print-funding-addresses); echo "$info"
OA=$(echo "$info" | awk '/^operator/{print $2}'); PIF=$(echo "$info" | awk '/^peg_in_fee/{print $2}'); POF=$(echo "$info" | awk '/^peg_out_fee/{print $2}')
PK=$(FPL_BROWSER_SECRET=$(cat browser.secret) node $HERE/browser-depositor/sign.mjs --pubkey)
DA=$($EX/fpl_depositor_ext address --network regtest --depositor-pubkey $PK)
echo "browser depositor pubkey $PK address $DA"
amt() { python3 -c "print('%.8f' % ($1/1e8))"; }
vout() { $B getrawtransaction $1 1 | python3 -c "import json,sys;t=json.load(sys.stdin);print([o['n'] for o in t['vout'] if o['scriptPubKey'].get('address')=='$2'][0])"; }
FT=$($B -named sendmany amounts="{\"$DA\":$(amt $((AMT+PIF))),\"$OA\":$(amt $((AMT+POF)))}" fee_rate=1)
dv=$(vout $FT $DA); kv=$(vout $FT $OA); echo "FUNDING $FT"
# miner: 1 block / 2 s while the run is alive
( while true; do $B -generate 1 >/dev/null 2>&1 || true; sleep 2; done ) & MINER=$!
trap 'kill $MINER 2>/dev/null || true' EXIT
for s in ${SEATS//,/ }; do $EX/fpl_ceremony_participant --dir $D --id $s --timeout-secs 3600 > seat-$s.log 2>&1 & done
EVM="fpl:alice"
$C --deposit-outpoint $FT:$dv --deposit-amount $((AMT+PIF)) --evm-address $EVM \
   --kickoff-outpoint $FT:$kv --kickoff-amount $((AMT+POF)) --broadcast --scenario take1 --timeout-secs 3600 > coordinator.log 2>&1 &
CP=$!
$EX/fpl_depositor_ext prepare --network regtest --dir $D --depositor-pubkey $PK --funding $FT:$dv --amount $((AMT+PIF)) --evm-address $EVM --seats $SEATS --timeout-secs 600
FPL_BROWSER_SECRET=$(cat browser.secret) node $HERE/browser-depositor/sign.mjs $D/depositor_request.json $RUN/signed.json
$EX/fpl_depositor_ext finish --dir $D --signed $RUN/signed.json
python3 -c "import json;r=json.load(open('$D/depositor_request.json'));print('DEPOSIT_TXID',r['deposit_txid']);print('PEG_IN_CONFIRM_TXID',r['peg_in_confirm_txid'])"
while kill -0 $CP 2>/dev/null; do
  l=$(grep -m1 '^PAYOUT_NEEDED' coordinator.log || true)
  if [ -n "$l" ] && [ ! -f $D/payout.txt ]; then
    set -- $l; PT=$($B -named send outputs="{\"$2\":$(amt $3)}" fee_rate=1 | python3 -c "import json,sys;print(json.load(sys.stdin)['txid'])")
    echo "PAYOUT_FUNDING $PT"; echo "$PT:$(vout $PT $2)" > $D/payout.txt
  fi
  sleep 5
done
RC=0; wait $CP || RC=$?
grep -E "^TX |SCENARIO|peg-in confirm|attestation|take1 after" coordinator.log || true
echo "SCRIPT_EXIT $RC"; exit $RC
