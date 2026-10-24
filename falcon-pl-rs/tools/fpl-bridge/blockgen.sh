#!/bin/bash
# Mines on the own-miner node (acceptnonstdtxn=1) so non-standard disproves
# submitted there get mined. Before each block, push the standard node's
# mempool to the miner (regtest stand-in for good P2P connectivity).
B="bitcoin-cli -datadir=/workspace/deploy/btc-keyless/regtest"
M="bitcoin-cli -datadir=/workspace/deploy/btc-keyless/${MINER_DATADIR:-regtest-miner}"
A=$($B -rpcwallet=miner getnewaddress)
while true; do
  have=$($M getrawmempool | tr -d '[]", ')
  for t in $($B getrawmempool | tr -d '[]", '); do
    case "$have" in *"$t"*) ;; *) $B getrawtransaction $t | $M -stdin sendrawtransaction >/dev/null 2>&1 ;; esac
  done
  $M generatetoaddress 1 $A >/dev/null 2>>/tmp/blockgen.log
  sleep ${1:-2}
done
