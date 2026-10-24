#!/bin/bash
# Regtest e2e of the intake service + 2 remote-style seats (seat_sync over the seat API)
# + browser depositor (sign.mjs over the public API). Ends when the peg-in confirm
# has 6 confs (state awaiting_mint). Env FPL_EX.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); EX=${FPL_EX:?}
RUN=${FPL_RUN:-/workspace/deploy/bridge-int/intake-e2e}; rm -rf $RUN; mkdir -p $RUN; cd $RUN; umask 077
B="bitcoin-cli -datadir=${FPL_BTC_DATADIR:-/workspace/deploy/btc-keyless/regtest} -rpcwallet=miner"
export FPL_NETWORK=regtest FPL_ESPLORA=http://127.0.0.1:8094/regtest/api/ FPL_EX=$EX FPL_SEATS=host1,host2,seat3
export FPL_PROOF_DIR=${FPL_PROOF_DIR:-/workspace/deploy/btc-keyless/fplproof-v3}
openssl rand -hex 32 > operator.secret; openssl rand -hex 32 > browser.secret
mkdir -p lead/intake s2/keys s3/keys
python3 - <<P
import hashlib,json,secrets
t={s:secrets.token_hex(16) for s in ["host2","seat3"]}
open("s2/keys/intake-seat.token","w").write(t["host2"]); open("s3/keys/intake-seat.token","w").write(t["seat3"])
json.dump({s:hashlib.sha256(v.encode()).hexdigest() for s,v in t.items()},open("lead/intake/seats.json","w"))
P
( while true; do $B -generate 1 >/dev/null 2>&1 || true; sleep 2; done ) & MINER=$!
FPL_OPERATOR_SECRET=$(cat operator.secret) FPL_OP_HOME=$RUN/lead FPL_LOCAL_SEAT=host1 FPL_INTAKE_REGTEST_NO_LEDGER=1 \
  FPL_INTAKE_LISTEN=127.0.0.1:18790 FPL_SEAT_LISTEN=127.0.0.1:18791 FPL_MIN_SATS=10000 python3 $HERE/fpl_bridge_intake.py > intake.log 2>&1 & IN=$!
FPL_OP_HOME=$RUN/s2 FPL_SEAT_ID=host2 FPL_LEAD_SEAT_URL=http://127.0.0.1:18791 python3 $HERE/seat_sync.py > s2.log 2>&1 & S2=$!
FPL_OP_HOME=$RUN/s3 FPL_SEAT_ID=seat3 FPL_LEAD_SEAT_URL=http://127.0.0.1:18791 python3 $HERE/seat_sync.py > s3.log 2>&1 & S3=$!
trap 'kill $MINER $IN $S2 $S3 2>/dev/null; pkill -f "$RUN/" 2>/dev/null || true' EXIT
sleep 4; API=http://127.0.0.1:18790
OA=$(awk '/operator funding/{print $6}' intake.log)
amt() { python3 -c "print('%.8f' % ($1/1e8))"; }
$B -named send outputs="{\"$OA\":$(amt 600000)}" fee_rate=1 >/dev/null
PK=$(FPL_BROWSER_SECRET=$(cat browser.secret) node $HERE/../browser-depositor/sign.mjs --pubkey)
R=$(curl -s -XPOST $API/v1/deposits -d "{\"account\":\"alice\",\"depositor_pubkey\":\"$PK\"}"); echo "$R"
ID=$(echo "$R" | python3 -c "import json,sys;print(json.load(sys.stdin)['id'])"); DA=$(echo "$R" | python3 -c "import json,sys;print(json.load(sys.stdin)['address'])")
$B -named send outputs="{\"$DA\":$(amt 100038)}" fee_rate=1 >/dev/null
st() { curl -s $API/v1/deposits/$ID | python3 -c "import json,sys
try: print(json.load(sys.stdin)['state'])
except Exception: print('unreachable')"; }
t0=$(date +%s); last=
while true; do
  s=$(st); [ "$s" != "$last" ] && { echo "$(date +%T) $s"; last=$s; }
  if [ "$s" = awaiting_signature ] && [ ! -f signed.json ]; then
    curl -s $API/v1/deposits/$ID | python3 -c "import json,sys;json.dump(json.load(sys.stdin)['depositor_request'],open('req.json','w'))"
    FPL_BROWSER_SECRET=$(cat browser.secret) node $HERE/../browser-depositor/sign.mjs req.json signed.json
    curl -s -XPOST $API/v1/deposits/$ID/signatures --data-binary @signed.json | head -c 300; echo
  fi
  [ "$s" = awaiting_mint ] && break
  [ $(( $(date +%s) - t0 )) -gt 2400 ] && { echo TIMEOUT; tail -20 intake.log lead/graphs/req-$ID/*.log; exit 1; }
  sleep 5
done
curl -s $API/v1/deposits/$ID | python3 -m json.tool | grep -vE '"(pubkey)"' | head -40
ls s2/graphs/req-$ID/final.json s3/graphs/req-$ID/final.json s2/graphs/req-$ID/request.json
echo INTAKE_E2E PASS
