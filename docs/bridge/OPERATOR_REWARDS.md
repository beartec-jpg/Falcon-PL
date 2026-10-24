# BitVM2 Operator Registry & Emissions Reward (`v2.9.81`, Active Since Height `692,000`)

**Status:** **LIVE on Testnet 2300 since height `692,000`** (`TESTNET_2300_BRIDGE_FROM_HEIGHT = 692_000`, `TESTNET_2300_OPERATOR_REWARDS_FROM_HEIGHT = 692_000`). See [BRIDGE_INTEGRATION.md](BRIDGE_INTEGRATION.md) for ledger transaction types and slashing verification.

## Operator Network Duties

The Bitcoin Testnet4 (`BTC4`) BitVM2 bridge is maintained by an open network of bonded operators paid in FPL for three duties:
1. **Liveness:** Keeping the operator kit online and submitting signed hourly heartbeats (`OperatorHeartbeat`, 168 slots per 7-day epoch).
2. **Watching & Challenging:** Monitoring all operators' claims against the Falcon PL ledger, cross-checking withdrawal bindings with peers before anyone fronts BTC, and challenging/disproving invalid claims on Bitcoin Testnet4. Each live heartbeat slot earns 1 watch unit; a confirmed on-chain Disprove earns bonus watch units (`disprove_watch_units = 336`) plus a **50% bounty** from the slashed operator's FPL bond.
3. **Fronting Payouts:** Fronting `BTC4` withdrawals directly from the operator's own Bitcoin wallet and claiming reimbursement from the presigned BitVM2 deposit graph (`KickOff1 → Take1` or `Challenge → Assert → Take2`).

Falcon PL / BEARTEC holds no BTC and no operator keys. Operator keys remain on the operator's machine.

## Qualification & Bonding Rules (Preset B — Live)

- **Registration (`OperatorRegister`):** Registers `{btc_xonly, reward_spk_hex, gossip_url, capacity_sats}` and locks a minimum bond of **`5,000 FPL`** (`min_bond = 5_000`). The BTC x-only public key serves as both the BitVM2 graph key and the BIP340 gossip signing key.
- **Liveness & Watching Eligibility:** Operator status must be `Active` (not jailed, unbonding, or removed), bonded (`>= 5,000 FPL`), with at least `min_liveness_slots` (`151` of `168` hourly slots, ~90%) recorded during the epoch.
- **Payout Eligibility:** Proven `BTC4` payouts reaching `PaidOptimistic` during the epoch naming the operator (`min_completed_payouts = 1`).

## Live Reward Split (`692,000+`)

- **5% Top Carve (`OPERATOR_SHARE_BPS = 500`):** At each 7-day epoch settlement, `operator_bucket = epoch_emit * 500 / 10_000` (5%) is carved off the top of `epoch_emit` before the standard 55/5/20/20 split runs on the remaining 95%.
  - **Effective total epoch emission split:**
    - **`BTC4` BitVM2 Operators:** **5.00%**
    - **Validators:** **52.25%** (55% of 95%)
    - **Watchers:** **4.75%** (5% of 95%)
    - **AMM LPs:** **19.00%** (20% of 95%)
    - **Lending LPs:** **19.00%** (20% of 95%)
- **Inside the 5% Operator Bucket (Preset B):**
  - **25% (`liveness_share_bps = 2,500`):** Split equally among liveness-eligible operators.
  - **25% (`watching_share_bps = 2,500`):** Split pro-rata by watch units (live heartbeat slots + `336` bonus units per confirmed Disprove).
  - **50% (Payout share):** Split pro-rata by proven `BTC4` payout volume (`PaidOptimistic` sats), capped per operator at **25%** (`cap_per_operator_bps = 2,500`) of the operator bucket.
  - Any unpaid remainder stays in (is refunded to) the treasury.

## Slashing & Disprove Bounty

- **Confirmed Disprove (`operator_slash`):** When a `Disprove` or `DisproveChain` transaction against a bound operator graph is proven on the `BTC4` rail (≥6 confirmations):
  - **100% (`slash_bps = 10,000`)** of the operator's FPL bond is slashed.
  - **50% (`disprove_bounty_bps = 5,000`)** of the slashed bond is awarded to the registered disprover whose `reward_spk_hex` matches output 1 of the Bitcoin Disprove transaction; the remaining **50%** is burned.
  - The dishonest operator forfeits all epoch volume and watch units, is jailed for `26` epochs (`disprove_jail_epochs = 26`), and is permanently removed after `2` disproves (`max_disproves = 2`).
  - On Bitcoin Testnet4, the operator also loses reimbursement of the fronted BTC.

## Parameter Summary (`OperatorParams::default()` — Live Preset B)

| Parameter | Live Value (Preset B) | Notes |
|---|---|---|
| `min_bond` | **5,000 FPL** | Required to register and remain active |
| `emission_share_bps` | **500 (5%)** | Carved off the top of `epoch_emit` |
| `liveness_share_bps` | **2,500 (25% of bucket)** | Equal among operators with ≥151/168 slots |
| `watching_share_bps` | **2,500 (25% of bucket)** | Pro-rata by watch units |
| Payout share | **5,000 (50% of bucket)** | Pro-rata by `PaidOptimistic` sats |
| `slots_per_epoch` / `min_liveness_slots` | **168 / 151 (90%)** | Hourly heartbeat slots per 7-day epoch |
| `disprove_watch_units` | **336** | Equivalent to 2 full epochs of watch units |
| `cap_per_operator_bps` | **2,500 (25% of bucket)** | Per-operator cap on the payout share |
| `max_payout_fee_bps` | **30 (0.30%)** | Maximum withdrawal fee bps |
| `slash_bps` | **10,000 (100%)** | Full bond slash on proven Disprove |
| `disprove_bounty_bps` | **5,000 (50% of slash)** | Paid to the disprover; other 50% burned |
| `disprove_jail_epochs` | **26** | Jail duration after a Disprove |
| `max_disproves` | **2** | Permanent removal threshold |
| `unbond_cooldown_epochs` | **2** | Bond remains slashable during cooldown |
