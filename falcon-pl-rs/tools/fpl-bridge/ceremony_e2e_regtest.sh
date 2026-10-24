#!/bin/bash
# End-to-end regtest run of ONE depositor-signed deposit (design 2026-10-18):
# separate processes for the depositor CLI signer (fpl_depositor, ephemeral
# key, policy checks) and the FPL signer (fpl_ceremony_participant, ephemeral
# key) + coordinator (no keys of the n-of-n). Then deposit + confirm.
# Regtest-only test secrets (depositor wallet / operator funding keys).
set -euo pipefail
EX=${EX:-/workspace/Falcon-PL/falcon-pl-rs/vendor/BitVM/target/release/examples}
B="bitcoin-cli -datadir=${FPL_BTC_DATADIR:-/workspace/deploy/btc-keyless/regtest}"
ESP=http://127.0.0.1:8094/regtest/api/
# depositor wallet key: only in the fpl_depositor process env (fresh per run)
DW=$(openssl rand -hex 32)
export FPL_OPERATOR_SECRET=3076ca1dfc1e383be26d5dd3c0c427340f96139fa8c2520862cf551ec2d670ac
D=$(mktemp -d /tmp/fpl_dep_e2e.XXXX); echo "exchange dir $D"
# PREGEN_POOL=dir: use pre-generated connector C (fpl_pregen, run from same cwd)
PG=""; [ -n "${PREGEN_POOL:-}" ] && PG="--pregen-pool $PREGEN_POOL"
C="$EX/fpl_ceremony_coordinator --dir $D --network regtest --esplora $ESP"
info=$($C --print-funding-addresses); echo "$info"
DA=$(FPL_DEPOSITOR_WALLET_SECRET=$DW "$EX/fpl_depositor" --print-address); OA=$(echo "$info" | awk '/^operator/{print $2}')
PIF=$(echo "$info" | awk '/^peg_in_fee/{print $2}'); POF=$(echo "$info" | awk '/^peg_out_fee/{print $2}')
AMT=2097152
vout() { $B getrawtransaction $1 1 | python3 -c "import json,sys;t=json.load(sys.stdin);print([o['n'] for o in t['vout'] if o['scriptPubKey'].get('address')=='$2'][0])"; }
dt=$($B -rpcwallet=miner sendtoaddress $DA $(python3 -c "print(($AMT+$PIF)/1e8)")); dv=$(vout $dt $DA)
kt=$($B -rpcwallet=miner sendtoaddress $OA $(python3 -c "print(($AMT+$POF)/1e8)")); kv=$(vout $kt $OA)
sleep 4
FPL_DEPOSITOR_WALLET_SECRET=$DW "$EX/fpl_depositor" --dir $D --funding $dt:$dv --amount $((AMT+PIF)) --evm-address 0x70997970C51812dc3A010C7d01b50e0d17dc79C8 --timeout-secs 1800 > $D/depositor.log 2>&1 &
"$EX/fpl_ceremony_participant" --dir $D --id fpl-signer --timeout-secs 1800 > $D/fpl-signer.log 2>&1 &
unset DW
T0=$(date +%s)
$C $PG --deposit-outpoint $dt:$dv --deposit-amount $((AMT+PIF)) --evm-address 0x70997970C51812dc3A010C7d01b50e0d17dc79C8 \
   --kickoff-outpoint $kt:$kv --kickoff-amount $((AMT+POF)) --broadcast --timeout-secs 1800 | tee $D/coordinator.log
wait
echo "signing session wall time: $(( $(date +%s) - T0 ))s"
tail -1 $D/depositor.log; tail -1 $D/fpl-signer.log
echo "connector C spks (for watcher publication):"; cat $D/connector_c_spks.txt
echo "transcript: $D/transcript.json"
