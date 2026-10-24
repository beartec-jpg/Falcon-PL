# Unjail and escalating rebond

British English. Operator-facing note for jail recovery on Falcon PL.

## Why

A jail used to be permanent: the seat left the lottery and there was no on-chain path back. The bond left after a small fixed slash sat idle. We now:

1. **Seize the full active bond** (and any pending unbonding) into the **prize pot** (`epoch_work.fee_reward_pool`) when a seat is jailed — prefer the prize pot over burn.
2. Require an explicit **`Unjail`** transaction that posts a **fresh, escalated rebond**.
3. Keep a persistent **`jail_count`** so repeat offences get steadily more expensive.

Nothing auto-unjails. An operator (or any funded account) must submit the tx.

## Cost formula

```text
unjail_cost = min_bond × 2^jail_count
```

| State | `jail_count` | Cost to enter / return |
|-------|--------------|-------------------------|
| Fresh bond-in (new seat, 2.9.62+ / from `H_act`) | 0 | `MIN_BOND_V2` (50_000) via `BondV2` (key + PoP + admission approval); legacy `Bond` cannot create seats |
| Fresh bond-in (pre-2.9.62 / grandfathered) | 0 | `min_bond` (1_000) via `Bond` |
| Historical jail (pre-field, still jailed) | stored `0`, priced as `1` | 2_000 via `Unjail`; ledger persists `jail_count=1` |
| After 1st jail | 1 | 2_000 via `Unjail` |
| After 2nd jail | 2 | 4_000 via `Unjail` |
| After 3rd jail | 3 | 8_000 via `Unjail` |
| … | n | `min_bond × 2^n` (saturates at shift 63) |

`jail_count` increments **on jail**, not on unjail, and is **never reset**. Snapshot round-trips preserve `jailed` and `jail_count`.

## Rules

| Rule | Detail |
|------|--------|
| Jail → prize pot | Full `bond + unbonding` credited to `fee_reward_pool`; `bond`/`unbonding` cleared |
| Apply order | On each ledger: epoch settle → **slash** → complete ready unbonds → txs. Slash before unbond completion so a cooldown that matures on the slash ledger cannot escape into `bond_account`. Pack applies the same order (slash before selecting txs) so a `Bond` to a pending offender is rejected, matching receivers. |
| Commitment root | `jail_count` is part of the validator commitment tuple (with bond / unbonding / jailed / bond_account) |
| Unjail only path | `Bond` while jailed still errors (`jailed: cannot bond`) |
| Amount check | `Tx.amount ≥ unjail_cost`; excess tops up the new bond |
| Funds | Debited from the signer's hot balance; insufficient balance rejects |
| Lottery | Jailed seats stay out until a successful `Unjail` lands in a sealed ledger |
| Historical `jail_count=0` | Seats jailed before the field existed price as count 1; successful Unjail writes `jail_count=1` into the root |
| Coordinated upgrade | Full-bond seize + `jail_count` in the root change commitment hashes — roll the mesh together |

## Operator flow

```text
1) Confirm status: validators[].jailed == true, note jail_count and unjail_cost
2) Fund the payer (faucet / pay) with at least unjail_cost + fee
3) falcon-pl-ctl unjail --id <vid> --from <account> --amount <unjail_cost>
4) Wait for the tip to advance; status should show jailed=false and bond ≥ amount
5) Ensure the node is ponging at tip so it re-enters the lottery
```

## Out of scope

- Live testnet: do **not** unjail v12 (or any jailed seat) until this build is reviewed, merged, and rolled out on purpose.
- No admin bypass and no automatic unjail after a timeout.
