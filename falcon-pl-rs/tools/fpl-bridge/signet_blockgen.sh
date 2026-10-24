#!/bin/bash
# Private test signet block producer (our own signer; never touches public networks).
# Usage: signet_blockgen.sh N_BLOCKS|ongoing [INTERVAL_SECS]
# Env: FPL_SIG_DIR (signet root with miner/ std/), FPL_CORE_SRC (Bitcoin Core 28.1 source tree for
#   contrib/signet/miner), FPL_SIG_PAYOUT (coinbase address, default: fund wallet on the std node).
set -euo pipefail
S=${FPL_SIG_DIR:-/workspace/deploy/bridge-int/signet}; SRC=${FPL_CORE_SRC:-/workspace/scratch/core281/bitcoin-28.1}
export PATH=/home/box/.local/bin:$PATH
M="bitcoin-cli -datadir=$S/miner"
ADDR=${FPL_SIG_PAYOUT:-$(cat $S/payout.addr 2>/dev/null || true)}
[ -n "$ADDR" ] || { ADDR=$(bitcoin-cli -datadir=$S/std -rpcwallet=fund getnewaddress cb bech32m); echo $ADDR > $S/payout.addr; }
N=$1; IV=${2:-0}; i=0
while [ "$N" = ongoing ] || [ $i -lt $N ]; do
  last=$($M getblockheader $($M getbestblockhash) | python3 -c "import json,sys;print(json.load(sys.stdin)['time'])")
  now=$(date +%s); t=$(( now > last ? now : last + 1 ))
  python3 $SRC/contrib/signet/miner --cli="$M -rpcwallet=signer" generate --address $ADDR \
    --grind-cmd="bitcoin-util grind" --min-nbits --set-block-time $t 2>&1 | grep -v '^$' | tail -1
  i=$((i+1)); [ "$IV" -gt 0 ] && sleep $IV
done
