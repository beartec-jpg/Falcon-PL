# Falcon PL BitVM2 Bridge: Operator Guide (Bitcoin Testnet4 `BTC4`, Live Since Height `692,000`)

The Falcon PL `BTC4` BitVM2 bridge is maintained by an open network of independent operators paid in FPL (`5,000 FPL` minimum bond, 5% of epoch emissions; see [OPERATOR_REWARDS.md](OPERATOR_REWARDS.md) and [BRIDGE_INTEGRATION.md](BRIDGE_INTEGRATION.md)).

Every operator kit (`fpl-operatord`):
1. **Fronts withdrawals** from its own Bitcoin Testnet4 (`BTC4`) wallet;
2. **Watches** every other operator's claims against the Falcon PL ledger and challenges invalid kickoffs on Bitcoin Testnet4;
3. **Gossips** BIP340-signed records (`WithdrawalSeen`, `ClaimIntent`, `PayoutTx`, `Kickoff`, `Heartbeat`, `Challenge`) to registered peers;
4. **Enforces pre-claim quorum cross-check** before funding any peg-out or kickoff; and
5. **Publishes a read-only JSON status feed** (`/status`, `/health`, `/feed`).

Falcon PL / BEARTEC holds **no BTC and no operator keys**: your funds, your keys, and your machine remain under your sole control.

## Requirements

- Linux x86_64, `glibc >= 2.34` (Ubuntu 22.04+, Debian 12+, RHEL 9+), 8 cores / 16 GB RAM recommended (graph pregen takes ~2 min per graph on 8 cores), 20 GB free disk.
- Bitcoin Core 28+ synced on **Testnet4** (`-testnet4 -txindex=1`), with a descriptor wallet you control.
- A synced Falcon PL node (`v2.9.81+`, see [`docs/PUBLIC_NODE.md`](../PUBLIC_NODE.md)) for `falcon-pl-ctl` and local RPC.
- At least **`5,000 FPL`** on Testnet 2300 for the operator bond, plus Testnet4 BTC liquidity to front withdrawals and pay kickoff/take transaction fees.
- `minisign` (for release signature verification).

## Installation & Setup

1. **Install the operator kit:**
   ```bash
   curl -fsSL https://falcon-fpl.com/install-operator.sh | bash
   ```
   Verifies the signed release, installs to `~/.falcon-operator` (mode `0700`), generates your operator graph secret key locally in `keys/operator.secret` (mode `0600`, never printed or uploaded), and starts the service in **dry-run** mode.
2. **Create and fund your Bitcoin Testnet4 wallet:**
   ```bash
   bitcoin-cli -testnet4 createwallet fpl-operator
   ```
3. **Derive your operator public key and payout address:**
   Run `falcon-operator keys` and set `FPL_OP_BTC_XONLY` and `FPL_OP_PAYOUT_ADDRESS` (public values only) in `~/.falcon-operator/operator.env`.
4. **Configure `~/.falcon-operator/operator.env`:**
   Set `FPL_OP_FALCON_ACCOUNT` (your Falcon PL account for bonding and rewards), `FPL_OP_FALCON_RPC`, `FPL_OP_BITCOIN_ARGS="-testnet4 -rpcport=48332"`, `FPL_OP_BTC_WALLET`, `FPL_OP_PAYOUT_FEE_BPS` (≤30 bps), `FPL_OP_MAX_PAYOUT_SATS`, `FPL_OP_MIN_RESERVE_SATS`, `FPL_OP_PREGEN_TARGET`, and `FPL_OP_PROOF_DIR` (pointing to the ceremony Groth16 proof/VK directory).
5. **Preflight check:**
   `falcon-operator check` verifies connectivity to your `bitcoind` wallet, your Falcon PL node, `keys/operator.secret`, `fpl_pregen`, and the pinned Groth16 verifying key hashes (`c6d3509b…7843` for Disprove, `4a1cabbc…4763` for M1 Withdraw Attest).
6. **Fill the pregenerated graph pool:**
   The daemon maintains `FPL_OP_PREGEN_TARGET` pregenerated graphs in `pregen/ready/` (`nice 10`; each pool entry is consumed once per deposit).
7. **Register & bond on Falcon PL (`692,000+`):**
   Run `falcon-operator register` to print the `operator_register` transaction body and the `falcon-pl-ctl operator` command (`5,000 FPL` minimum bond). Once registered on-chain, `status.bridge_operators.operators` lists your x-only key and `gossip_url`.
8. **Join deposit ceremonies:**
   Each new `BTC4` deposit presigns one peg-out graph per active registered operator (`fpl_ceremony_participant`), and `fpl-operatord` posts `operator_graph_bind` on Falcon PL so the graph is verifiable under the depositor refusal rule.
9. **Go live:**
   Verify dry-run behavior via `falcon-operator status` (`curl 127.0.0.1:19480/status`), then set `FPL_OP_MODE=live` and run `falcon-operator restart`.
   - For each open `BTC4` withdrawal (`Bitvm2Claim` in `AwaitingPegout`), the winning operator funds `PEG_OUT` (paying the user on Bitcoin Testnet4), submits `Bitvm2AttestV2` on Falcon PL, and executes `PEG_OUT_CONFIRM → KICK_OFF_1 → START_TIME → KICK_OFF_2 → TAKE_1` for reimbursement.
   - Never copy or reuse `keys/operator.secret` or `pregen/` entries across multiple machines (equivocating on WOTS commitments allows watchers to disprove and slash your bond).

## Challenging & Disproving Invalid Claims

- Every operator kit watches all peers' `Kickoff` / `PegOut` / `PayoutTx` activity against the Falcon PL ledger and local `bitcoind` (`txindex=1`).
- If an operator broadcasts an invalid kickoff (missing withdrawal, mismatched binding, wrong payout script, underpaid amount, or missed deadline), `fpl-operator-challenge.sh` invokes `bin/fpl_challenge` to fund and broadcast the presigned `CHALLENGE` transaction on Bitcoin Testnet4 and gossips `ChallengeTx`.
- With `FPL_OP_PROOF_DIR` / `FPL_OP_CHALLENGE_VK_DIR` configured, `fpl_challenge --follow` watches for the operator's `Assert` transactions and broadcasts `Disprove` or `DisproveChain` if the Groth16 verification trace fails.
- Because Bitcoin Testnet4 `Disprove` transactions are non-standard (~3.6–3.8 MB witness), configure standard and non-standard submission endpoints in `$FPL_OP_HOME/submit.json`:
  ```json
  {
    "standard": [{"kind": "esplora", "url": "https://mempool.space/testnet4/api"}],
    "nonstandard": [
      {"kind": "miner_http", "name": "tn4-miner", "url": "https://your-miner-endpoint.example/submit", "auth_env": "FPL_MINER_AUTH"},
      {"kind": "bitcoind_rpc", "name": "tn4-nonstd-node", "url": "http://127.0.0.1:48332", "user_env": "BTC_RPC_USER", "pass_env": "BTC_RPC_PASS"}
    ]
  }
  ```
- Once a `Disprove` or `DisproveChain` transaction reaches 6 confirmations on Bitcoin Testnet4, `bin/fpl-operator-slash.sh` submits `operator_slash` on Falcon PL to slash 100% of the cheating operator's bond and claim the 50% disprover bounty.

## Fronting Capacity & Selection

- Every poll, `fpl-operatord` computes free fronting capacity:
  `available_sats = wallet_trusted_balance - in_flight_committed_sats - FPL_OP_MIN_RESERVE_SATS` (default reserve: `50,000` sats).
- Capacity is advertised in signed hourly `Heartbeat` records (`operator_heartbeat` on-chain) and `Balance` gossip records.
- Among operators that pass the 2/3 peer binding cross-check and have advertised sufficient `available_sats` for a withdrawal, the earliest `ClaimIntent` (tie-broken by lowest x-only key) serves the withdrawal.
