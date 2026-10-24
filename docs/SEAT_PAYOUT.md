# Seat payout: validator rewards kept per seat

Consensus change, gated at `TESTNET_2300_SEAT_PAYOUT_FROM_HEIGHT`
(`params_v2.rs`, **692_000**, shipped in 2.9.80; other networks:
`FPL_SEAT_PAYOUT_FROM_HEIGHT`, default never). Every seat must run a build
carrying the same height before it is reached.

## Height

692_000: the **same block** as Fix B (on-chain wallet keys) and the bridge
(Scott, 2026-10-22). Tip was ~682_000 at 2026-10-22 16:51 BST at ~11
blocks/min, so 692_000 is around midday Fri 23 Oct 2026 BST. Every seat must
run 2.9.80 before then: a 2.9.79 seat computes a different root from 692_000.

Order on ledger 692_000 (pack, stage and apply paths alike):

1. time fold: an epoch settle checks `tip + 1 = 692_000`, so it already
   routes validator pays to the seat buckets;
2. `rv2_ledger_hook`, then `seat_migrate_hook` (moves `claimable[seat id]`
   into the seat bucket, once);
3. the ledger's txs, verified with the Fix B on-chain key rule. Seat txs
   (`<id>#seat`) bypass wallet key binding and use the seat's registered key,
   so Fix B and the seat rule do not touch the same check.

Ring restore stays at 702_000.

## Rules from the height (ledger `h >= H`, settle uses `tip + 1`)

* **Separate buckets.** At epoch settle, pack, check and floor pays, plus the
  v2 proposer fee share, are *validator* rewards. Each is credited to the seat
  (`ValidatorInfo.seat_claimable`), or to the seat's payout account's liquid
  balance when one is set. They never go to `claimable[name]`. Watcher, AMM and
  lend pays stay in `claimable[account]`. A wallet `Claim` (website passkey)
  only moves `claimable[account]`, so it cannot take validator rewards even
  when the wallet and the seat share a name (`nodesafe`). A pay to an id that
  is not a seat keeps the old `claimable[id]` credit.
* **Seat sender.** Seat txs are sent by the pseudo account `<id>#seat`, which
  has its own sequence, so they never race the wallet's sequence. The account
  is verified only against the seat's *registered* key (BondV2 key or the
  on-chain seat key registry, `Node::registered_key`). That is deterministic.
  It is never the wallet's `meta.public_key` and never the node-local ring. A
  `#seat` sender may only send seat txs.
* **`SetSeatPayout { validator, payout_account: Option<String> }`** sets or
  clears (`None`) the payout account. Later settles credit validator rewards
  straight to that account's **balance**. The bucket that already exists stays
  where it is until `ClaimSeat`.
* **`ClaimSeat { validator, to }`** moves the whole bucket to `to`'s balance.
* **Fees** for both are paid first from the seat bucket, then from the liquid
  balance of `<id>#seat` (anyone can `Pay` it). They are never taken from the
  wallet that shares the name, because the wallet key did not sign. If the
  bucket is empty (for example, a payout account is set, so the bucket stays
  at 0) and `<id>#seat` holds no FPL, the tx fails with `insufficient balance
  for fee`.
* **Root.** Seats with a non-zero bucket or a payout account join the
  commitment as a 9th tuple element `{id: (seat_claimable, payout)}`. The map
  is empty before the height, so the root bytes do not change and history
  verifies.

## Migration (activation ledger only, before its txs)

Historic `claimable[seat id]` mixes validator and account rewards and cannot
be split. Rule: for every account whose name is a validator seat id, **the
whole `claimable[id]` moves into that seat's bucket**, once, on ledger `H`.
Other accounts are untouched. After `H`, watcher/AMM pays to an account named
like a seat accumulate in `claimable[id]` again, and the wallet can claim them.
In the worst case a wallet that shared a seat's name loses its pre-`H` account
rewards to the seat bucket. These are small (watcher heartbeats), and the seat
key holder can send them back with `ClaimSeat --to <wallet>`.

## Before the height

Seat txs are refused at mempool admission (`seat payout not active yet`), are
not relayed or exported, and are rejected at apply. A `#seat` name is an
ordinary account, settle credits `claimable[id]` exactly as before, and the
root is byte-identical.

## Operator tools

* `falcon-pl-node --payout-address <account>` (or env `FPL_PAYOUT_ADDRESS`).
  From the height, every 2 min while the node is caught up, it submits
  `SetSeatPayout` (signed with the seat key from `--keys-dir`) if the on-chain
  value differs and none is pending.
* Installer: `--payout-address <account>` (alias of `--payout`) is stored as
  `PAYOUT_ACCOUNT` and passed to node builds that support the flag.
  `falcon-node status` shows seat rewards and the payout account.
* `falcon-pl-ctl claim-seat --validator <id> --to <account> --keys-dir <seat keys>`
* `falcon-pl-ctl set-seat-payout --validator <id> (--payout-account <a> | --clear) --keys-dir <seat keys>`
* Account query (`falcon-pl-ctl account --account <id>`) adds `seat_claimable`,
  `seat_payout` and `seat_sender_sequence` for seat ids. Validator lists in
  status carry the two new `ValidatorInfo` fields when they are set. The
  health `balance` block, `falcon-node dash` and `falcon-node web` show "Seat
  rwd" / "Seat payout".

## Known follow-ups

* Genesis seats that paid `Unbond` / `CompleteUnbond` fees from
  `claimable[id]` must first `ClaimSeat` to their bond account, because
  migration empties `claimable[id]`.
* The installer's legacy epoch `auto-payout.sh` (wallet Claim then Pay) now
  forwards only account rewards. On-chain payout covers validator rewards.
