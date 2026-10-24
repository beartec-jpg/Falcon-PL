# BTC pool freeze — 16 Oct 2026 (M0)

Scott approved this on 16 Oct 2026. **The live BTC bridge is custodial.** Pool funds can be moved by keys on one operator host (host1), and Bitcoin has no Disprove path. Nothing here claims non-custody.

## State
- **BTC withdrawals:** paused. walletd `FPL_BTC_KICKOFF_ENABLED` is unset; site `btc_withdrawals_enabled=false`.
- **New BTC deposits:** paused. Site `btc_deposits_enabled=false` (Falcon-faucet-wallet #111). Deposits already sent can still be claimed.
- **`falcon-pl-bitvm2-operator`:** stopped and disabled on host1 (16 Oct 2026, 03:42 UTC). Its 15-second retry loop read the root setup-1 key on every attempt.
- **Pool keys:** not touched. No sweep. Migrating to a presigned BitVM2 graph is separate work; see "Next" below.

## BitVM2 instance pool `tb1p2xuekx55w9llxe023y070lf32kk0z873nv6pse0awg75ll7l930suzcgn5`
Tree: NUMS key path plus two leaves, `kickoff` (`<claimer_xonly> OP_CHECKSIG`, one key) and `setup_nofn` (both setup keys are on host1). Custodial (#51).

Unspent outputs (mempool.space testnet, 16 Oct 2026):

| Outpoint | Sats | Kind |
|---|---:|---|
| `5f1d2c35fc7b18638f208fc95ff6ed8589e215b26a543db337b96a97353d689e:0` | 6,000 | instance seed (2026-09-08) |
| `0e973584f71dbcc89f1bbf2397ac9b9d068f7ff43120295e7c7ba1403c13f10e:0` | 20,000 | FALC peg-in |
| `3f27e639bf9581efe1c4846c4fe020ba3a468b7cd2c18f43b15a7d5a0cf6acbf:0` | 100,000 | FALC peg-in |
| `ddb637b05c27b02ca426479e95a597de0e8f3684388c29a2fa15697460402b98:0` | 100,000 | FALC peg-in |
| `fe8b43fd127955856288c8f06142515d08b20fd636eabd421179325de2dd2502:0` | 150,000 | FALC peg-in |
| `608c0ab68c4b55034c7d2530e7945d6172fb83bb1632da197fea88da38f9a56a:0` | 150,000 | FALC peg-in (last, 2026-10-10) |
| **Total** | **526,000** | 6 UTXOs |

The pool has been spent only once: `d34ea79cf6651e9a1d4d635c9de06f8b2dce177ed95e9933ddf41f65a1bee43e` (2026-09-09), through the single-key kickoff leaf.

Other BTC addresses, which are also custodial or stranded:
- NUMS pool `tb1pd6ltq2yu…sdq34`: 6 UTXOs, 486,370 sats. The watch key is operator-only.
- Odd-Y v1 `tb1pq9mgl…cq6098`: 1 UTXO, 141,000 sats. Unspendable under BIP341.

## Stuck operator jobs: closed as paid (Scott chose Option A, 16 Oct 2026)
Both were in `kickoff_pending`, retrying every 15 seconds. FPL shows each burn as `awaiting_pegout` with no peg-out recorded. Bitcoin shows both were already paid, so no FBTC refund was made and no FPL balance changed. Jobs are marked `closed_paid` in `run/bitvm2-operator.json` (backup: `run/bitvm2-operator.json.bak-20261016-m0`).

| Note | FPL burn | BTC payment |
|---|---|---|
| `06420da2e61ceb95…` | sally, 10,000 sats, h326847, dest `tb1qpjttdgpgz4s24vka49gzt3t5fwp0t7ljtzmhe0` | `3478b5b47921e6e3d8b01c9ec84ca847e65afb76fe7a810e11a0157b6a8441c6`: operator (dave) paid 10,000 sats to the dest on 2026-09-07 (A1 fronting) |
| `8016366c35285587…` | dave, 5,000 sats, h335053, dest `tb1qmn6cajwxf0x4n2vz9qpupncl39wukz24dz6g5f` | `d34ea79c…` paid a 5,000-sat dest-lock from the pool; the user took 4,000 in `9dd5ff003502fce48cceabe5d8ba7382f6e1e2896f73c0e159704df836b7d0e0` (2026-09-09) |

The FPL claim status still says `awaiting_pegout`. The operator is disabled, so nothing acts on it.

## What earlier BTC exit tests actually exercised
- A1 operator fronting from the `dave` hot key (tb1q6lq0…ntz7l), with `FPL2` OP_RETURN markers, up to 2026-09-07.
- One pool payout through the single-key kickoff leaf (`d34ea79c…`, 2026-09-09).
- No Challenge, Assert or Disprove transaction has ever been broadcast on Bitcoin.

## Next: what non-custody needs
Per-deposit presigned Claim/Challenge/Assert/Disprove graph, set up by independent parties who generate keys in memory and wipe them. Deposits locked only to that graph. Real header verification in place of `header_hash_placeholder` (`bridge.rs:1368,1568`). A Disprove that anyone can broadcast with no key. Operator repaid only through the graph. After that, sweep this pool into the graph and destroy the host1 keys. Withdrawals stay off until the testnet acceptance test passes (a dishonest claim is disproved on chain, and no key exists that can move funds).
