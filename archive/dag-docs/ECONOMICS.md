# Falcon DAG — Economic substrate (Phase A)

**Native asset:** `FDAG`  
**Treasury account:** `treasury`  
**Status:** implemented in Rust `fd-exec` / typed txs (v0.7)

## Goals delivered

| Item | Spec |
|------|------|
| Typed txs | `payment`, `transfer`, `issue`, `mint`, `burn` |
| Multi-asset | `balances[account][asset]` |
| Fees | paid in `FDAG`; default **50% burn / 50% treasury** (`fee_burn_bps` / `fee_treasury_bps`) |
| Token issue | issuer creates asset code + optional `max_supply`; initial mint |
| Mint / burn | issuer mint; holder burn |
| Genesis | `acct1..N` each get `genesis_native_per_account` FDAG |
| Emissions | optional schedule: every N ordered non-genesis batches mint to `to` |
| State root | Merkle over balances, sequences, assets, fee meta |

## Genesis supply

```
native_supply_0 = genesis_accounts × genesis_native_per_account
```

Defaults: 20 accounts × 1e9 = **20_000_000_000 FDAG**.

## Fee split

For fee `F`:

- `burn = F * fee_burn_bps / 10000` (removed from supply)
- `treasury += F * fee_treasury_bps / 10000`
- remainder (if bps sum &lt; 10000) → burn

## Tx kinds (RPC `submit.kind`)

| kind | Fields |
|------|--------|
| `payment` | native amount → dest; fee in FDAG (legacy hash) |
| `transfer` | `asset` amount → dest; fee in FDAG |
| `issue` | `asset` code, `amount` initial, `max_supply?`, issuer = account |
| `mint` | issuer mints `asset` to dest |
| `burn` | holder burns `asset` |

## RPC

- `economy` — supply, burn, treasury, assets, `state_root`
- `account_info` — balances + sequence
- `asset_info` — issuer + supply

## Not in Phase A

AMM, lending, oracles, CID marketplaces — need this substrate first.
