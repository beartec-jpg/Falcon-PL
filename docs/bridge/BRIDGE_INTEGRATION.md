# BTC4 BitVM2 Bridge + Operator Network: Ledger Integration (LIVE Since Height `692,000`)

The Bitcoin Testnet4 (`BTC4`) BitVM2 bridge, operator registry, graph binding rules, SPV disprove slashing, and 5% operator emission share are **active on public testnet 2300 since height `692,000`** (`v2.9.80` / `v2.9.81`):

```rust
// crates/fd-pl/src/params_v2.rs
pub const TESTNET_2300_BRIDGE_FROM_HEIGHT: u64 = 692_000;
pub const TESTNET_2300_BITVM2_M1_FROM_HEIGHT: u64 = TESTNET_2300_BRIDGE_FROM_HEIGHT;
pub const TESTNET_2300_OPERATOR_REWARDS_FROM_HEIGHT: u64 = TESTNET_2300_BRIDGE_FROM_HEIGHT;
```

On network 2300, environment overrides are ignored; off-2300 (regtest/dev), `FPL_BRIDGE_FROM_HEIGHT` (or `FPL_BITVM2_M1_FROM_HEIGHT`) configures the activation height. Below height `692,000`, all operator/M1 transaction types were refused at apply and settlement remained byte-identical to prior history.

Falcon PL holds no BTC and no operator keys: independent operators front withdrawals from their own Bitcoin Testnet4 wallets and are reimbursed via presigned BitVM2 graphs.

## Live Rules at Height `692,000+`

| Component | Behavior |
|---|---|
| **M1 BitVM2 Withdraw (`Bitvm2AttestV2`)** | Verifies Bitcoin Testnet4 SPV peg-out inclusion (≥6 confirmations), exact output script/amount match, checkpointed FPL header hash, and Groth16 proof against the ceremony verifying key (`BITVM2_WITHDRAW_VK_HEX`) → transitions claim to `ClaimPosted` and records `attest_height`. |
| **Payout Finality** | `ClaimPosted` + `BITVM2_PAYOUT_FINALITY_LEDGERS` (`21,600` ledgers) → transitions to `PaidOptimistic` and credits payout volume to the operator's registry record (BTC reimbursement happens on Bitcoin Testnet4 via `Take1`/`Take2`). |
| **On-Bitcoin Challenge** | `rail_challenge` on BitVM2 claims is refused (`"challenge on Bitcoin"`); BitVM2 claims are challenged directly on Bitcoin Testnet4 using the presigned `Challenge` transaction (`fpl_challenge`). |
| **Operator Registration (`operator_register`)** | `{btc_xonly, reward_spk_hex, gossip_url, capacity_sats}` with `amount >= 5,000 FPL` minimum bond debited from the account. Re-sending with the same key tops up the bond (and re-activates after jail once `bond >= 5,000 FPL`). |
| **Heartbeat & Capacity (`operator_heartbeat`)** | `{capacity_sats}`: records one hourly liveness/watch unit and updates advertised free BTC fronting capacity (`wallet - in_flight - reserve`). |
| **Graph Binding (`operator_graph_bind`)** | `{deposit_txid, kick_off_2_txid, assert_final_txid}`: immutable, globally unique txids binding the operator's presigned BitVM2 graph to a deposit so any on-chain Disprove is slashable on FPL. |
| **SPV Disprove Slashing (`operator_slash`)** | `{operator_btc_xonly, deposit_txid, raw_tx_hex, btc_height, merkle_path, merkle_index}`: verifies SPV inclusion on the `BTC4` rail (≥6 confirmations) and checks transaction shape against the bound graph (`Disprove` spending `AssertFinal:1` & `:2` with `out0 = BitVM burn P2WSH`, or `DisproveChain` spending `KickOff2:1` with `out0 = burn`). Slashes **100%** of the operator's FPL bond, jails the operator, and pays a **50% bounty** to the registered operator whose `reward_spk_hex` matches Disprove output 1 (the remaining 50% is burned; front-running a copied transaction earns nothing). |
| **Unfronted Withdrawal Refund** | Any claim still `AwaitingPegout` after `BITVM2_PEGOUT_REFUND_LEDGERS` (`43,200` ledgers, ~48h) transitions to `Refunded`, crediting the burned `BTC4` back to the withdrawer. Operator kits stop fronting at half the window (`front_cutoff`, ~24h) so in-flight peg-outs have time to confirm and be attested. |
| **Unbonding (`operator_unbond`)** | Enters a 2-epoch cooldown during which the bond remains slashable, then returns the bond at the next epoch settlement. |
| **Operator Emissions (5% Top Carve)** | At each epoch boundary, `operator_bucket = epoch_emit * 5%` (`OPERATOR_SHARE_BPS = 500`) is carved off the top before the 55/5/20/20 split. Distributed across eligible operators by liveness, watch units, and proven payout volume; any undistributed remainder refunds the treasury. |

Status JSON exposes `bridge_operators {active, from_height, params, operators, graphs, slashed, pegout_refund_ledgers, burned_total}`.

## Graph Binding Refusal Rule (Depositor & Operator Kit)

A peg-in is accepted only if `final.json` contains ≥1 peg-out graph for the same `PEG_IN_CONFIRM` txid and, for **every** graph:
1. The operator's BTC x-only key is registered and `Active` on Falcon PL;
2. A graph binding `deposit:xonly` exists on-chain, posted by the account that registered that key; and
3. Its `kick_off_2_txid` and `assert_final_txid` match the witness-stripped txids computed from `final.json`.

- **Depositor verification:** `tools/fpl-bridge/check_graph_bindings.py <graph dir>` (or `fpl-operatord check-deposit <dir>`) before broadcasting `PEG_IN_CONFIRM` (exit `0` = OK, `2` = REFUSE, `1` = error).
- **Operator kit enforcement:** Deposits failing the rule are never used for fronting; submitted slashes are re-checked against `slashed` and retried if not yet applied.

## Operator & CLI Tools

- `falcon-pl-ctl operator --account A --amount <bond|0> --body '<TxBody JSON>' [--keys-dir D --scheme falcon --network-id 2300]`
- `tools/fpl-bridge/slash_evidence.py --cli "bitcoin-cli -testnet4 ..." --txid <disprove> --operator-xonly <x> --deposit <confirm txid>` — builds the `operator_slash` JSON body from Bitcoin Testnet4 RPC.
- `fpl-operatord`: submits hourly heartbeats (with free BTC capacity), graph bindings for presigned graphs, `Bitvm2AttestV2` proofs, and `operator_slash` evidence (`bin/fpl-operator-slash.sh`).

## Prover & Verifying Key Pinning

- `fpl-operatord` resolves the Groth16 proof set via `FPL_OP_PROOF_DIR` (`vk.bin`, `proof.bin`, `public.bin`).
- Pinned VK hashes:
  - Disprove VK SHA-256: `c6d3509b…7843` (`FPL_OP_DISPROVE_VK_SHA256`)
  - Withdraw / M1 Attest VK SHA-256: `4a1cabbc…4763` (`BITVM2_WITHDRAW_VK_HEX` / `FPL_OP_WITHDRAW_VK_SHA256`)
- Off regtest, there is no fallback to mock verifying keys: a missing or mismatched proof directory refuses pregen and attestation duties.
- Pregen pools are tagged with `pregen/vk.sha256`; `fpl-operatord check` verifies the proof directory and VK digests.

## Regtest & Testnet4 Disprove Verification

`tn4_scenario.sh disprove [SATS]` exercises the full dispute path: unauthorized `KICK_OFF_1` → watcher `Challenge` (`fpl_challenge`) → operator `START_TIME` / `KICK_OFF_2` / `ASSERT_INITIAL` / `ASSERT_COMMITS` (with corrupted proof) / `ASSERT_FINAL` → watcher `--follow` `Disprove` → verification that `Take1` and `Take2` cannot succeed. Set `FPL_NETWORK=regtest` (`deploy/bridge-int/run_reg_disprove.sh`) to run locally on regtest.
