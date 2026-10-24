# Protocol AMM & Lending (no free key path)

**Build:** `falcon-dagd/0.10.0-rust`  
**Trust:** Pool / LendingMarket balances move **only** via these TxKinds. Free `payment`/`transfer` from those accounts remains **rejected**.

---

## Architecture

```text
User Hot (key) ──signed protocol tx──► Executor rules
                         │
         ┌───────────────┼────────────────┐
         ▼               ▼                ▼
   Pool account    LendingMarket     User balances
   (sealed type)   (sealed type)     (Hot/Vault)
                         │
                    Oracle median
                    (price votes)
```

- User signs **their** side of the op (sequence, fee).
- Reserves debit/credit on Pool/Market accounts **inside** `apply` helpers — not via `Payment` from a pool key.
- Invariants are pure functions of state + tx (replayable).
- Snapshots (v3) persist pools, markets, indices, collateral, oracle votes/prices.

---

## AMM (constant product)

| Tx | Who signs | Effect |
|----|-----------|--------|
| `open_account_type(pool)` | pool shell account | Seal account as Pool |
| `create_pool` | liquidity provider | Register pair, deposit A/B, mint LP |
| `add_liquidity` | LP | Pro-rata deposit, mint LP |
| `remove_liquidity` | LP | Burn LP, withdraw A/B from pool |
| `swap` | trader | `amount` of `asset` in → other out; fee in curve |

**Invariant:** \(x \cdot y = k\) (fee on input increases \(k\)).  
**LP asset:** `LP-{poolAccountId}`.  
**Default fee:** 30 bps.  
**Protocol fee skim:** ~1/6 of the trading fee (`protocol_fee_share_bps` ≈ 1667) of input asset is credited to the **sealed treasury vault** — never via a free pool key path.

### Slippage
- Swap: `min_out`
- Remove: `min_out` (asset A min), `amount_b` (asset B min)

---

## Lending (indices + multi-collateral + oracle)

| Tx | Who signs | Effect |
|----|-----------|--------|
| `open_account_type(lending_market)` | market shell | Seal as LendingMarket |
| `create_market` | anyone (pays fee) | Register asset + LTV; optional `collateral_assets` |
| `supply` / `withdraw_supply` | supplier | Deposit/withdraw via supply index (shares) |
| `borrow` | borrower | Lock collateral (`asset_b` + `amount_b`), receive borrow asset |
| `repay` | borrower | Repay borrow; full repay returns all collateral assets |
| `liquidate` | liquidator | If under LTV: repay debt, seize multi-asset coll + bonus |
| `oracle_vote` | voter | Post price (1e6 native per unit); median becomes effective |

**Oracle:** `amount` = price × 1e6. Native always 1.0. Unknown assets fall back to 1:1 until voted.  
**Interest:** Aave-style `supply_index` / `borrow_index` (scale 1e9). Accrues each full window of 100 applied txs; rate = base + slope × utilization.  
**Default LTV:** 50%. **Liq bonus:** 10%.  
**Multi-collateral:** `create_market` with `collateral_assets: ["FDAG","GOLD"]`; borrow with `asset_b` + `amount_b`.

---

## What stays impossible

| Action | Result |
|--------|--------|
| `payment` from Pool / LendingMarket | **Rejected** |
| Operator “admin withdraw” | **No such path** |
| Change pool policy via key | **No** |
| Soft-unlock sealed accounts | **No** |

---

## RPC kinds

`create_pool`, `add_liquidity`, `remove_liquidity`, `swap`,  
`create_market`, `supply`, `withdraw_supply`, `borrow`, `repay`, `liquidate`,  
`oracle_vote`

Also: `pool_info`, `market_info`, `oracle_info`, `economy` (pools/markets/oracle/skim).

---

## Smoke

```bash
./scripts/defi_smoke.sh
```

---

## Snapshot

Snapshot **v3** includes policies, pools, markets, supply/borrow shares, multi-asset collateral, oracle prices/votes, index state, and `protocol_fees_skimmed` so restart keeps DeFi continuity.
