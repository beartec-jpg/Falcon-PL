# Falcon PL 2.10.0: activation height

Network 2300 switches two consensus changes on at **block height 755,000**
(`params_v2::TESTNET_2300_V2_10_FROM_HEIGHT`, aliased as
`TESTNET_2300_EXTERNAL_ADDRS_FROM_HEIGHT` and `TESTNET_2300_EMPTY_BLOCK_FROM_HEIGHT`).
Height semantics follow the 2.9.80 gates: a ledger at height `A` is validated
with the tip at `A - 1`, so the rules are on for ledgers `A >= 755,000`.

| Change | Before 755,000 | From 755,000 |
|---|---|---|
| `SetExternalAddrs` tx / `Account.external_addrs` / `account_query` | refused at admission, never relayed or exported, rejected at apply (no fee, no sequence bump) | accepted and applied |
| Empty fallback block (leader with no executable tx seals a zero-tx ledger) | not sealed, empty ledger rejected, as 2.9.81 | sealed and accepted; empty-block time effects and no proposer credit |

A seat still on 2.9.81 at the activation height forks off at the first empty
block or the first `SetExternalAddrs`. **Every seat and outside operator must be
on 2.10.0 before the height.**

Everything else in 2.10.0 (installer, autoupgrade, ack suppression, log rate
limit, fdash claim hotkey, ctl commands, local mempool eviction) is node-local.

## Hello version

2.9.x nodes refuse a Hello whose `product_version` is not `2.9.<n>`. To keep a
rolling upgrade connected, 2.10.0 sends `HELLO_WIRE_VERSION = "2.9.100"` in its
Hello (binaries, status and logs still say `2.10.0`) and accepts both `2.9.x`
and `2.10.x`.

## State hash / snapshots

`Account.external_addrs` is `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`:
while empty (always, before activation) the account JSON, the world-state root
and the snapshot encoding are byte-identical to 2.9.81.

## Rate sensitivity

ETA = remaining blocks / blocks per hour. Measured 2026-10-28 16:27 UTC: tip
733,611, ~223 blocks/h (last 4 h), so ~Sun 2026-11-01 15:30 UTC; 215/h gives
Sun 19:56 UTC, 200/h Mon 2 Nov 03:24 UTC, 250/h Sun 06:00 UTC. Re-measure with
`falcon-pl-ctl status`.
