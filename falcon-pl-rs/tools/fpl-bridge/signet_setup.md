# Private test signet for the Disprove scenario

Why: Disprove is non-standard (~956k WU). Bitlayer's BitVMNet signet (accepts non-standard txs)
was checked on 2026-10-20 and is offline: esplora.bitvmnet.org / mempool.bitvmnet.org fail the TLS
handshake, www.bitvmnet.org is a parked-domain lander, and its vBTC faucet (TheFaucet) needs a
login. So the run uses our own signet on the box (own signer, we control mining).

Layout (`FPL_SIG_DIR`, default /workspace/deploy/bridge-int/signet):
- `std/`   standard-policy node (rpc 38443, p2p 38444, txindex), wallet `fund` (coinbase payout).
- `miner/` miner node, `acceptnonstdtxn=1` (rpc 38543, p2p 38544, connect=std), wallet `signer`
  holding the signet key (imported as `multi(1,<key>)` from the 0600 file `signer.desc`; never printed).
- `challenge.txt` signetchallenge `5121<pubkey>51ae` (1-of-1). Same line in both bitcoin.conf.
- No seeds (dnsseed=0, bind 127.0.0.1): nobody else can join; never peers with public networks.

Bring-up:
1. bitcoind -datadir=$S/std -daemon; bitcoind -datadir=$S/miner -daemon
2. Block producer (Core 28.1 contrib/signet/miner + bitcoin-util grind, min difficulty):
   `signet_blockgen.sh 210` (initial coins) then `signet_blockgen.sh ongoing 5` (one block per ~5-8 s).
3. `esplora_shim.py --datadir $S/std --also-push-datadir $S/miner --port 8095` (base /signet/api).
4. Run: `deploy/bridge-int/run_sig_disprove.sh RUN_DIR` = `FPL_NETWORK=signet tn4_scenario.sh disprove`
   with FPL_SUBMIT -> `bitcoind_rpc` channel to the miner node (Disprove), FPL_CHALLENGE_CLI = std `fund`.

Timelocks: test values, `FPL_REGTEST_TIMELOCK_DIVISOR=8` (now also applies to Signet in the BitVM
patch): 2 weeks -> 252 blocks, 3 days -> 54, 6 h -> 5. Not the fpl-tn4-timelocks values (those are
testnet4-only, mainnet/28). Unset divisor = upstream 2 blocks.

A public non-standard-accepting signet (e.g. BitVMNet when back) works the same way:
`FPL_ESPLORA=<esplora api>/ FPL_EXPLORER=<explorer>/tx/ FPL_NETWORK=signet`, a node synced to it
(`signetchallenge=` + `addnode=`), and FPL_SUBMIT pointing at a node that accepts non-standard txs.
