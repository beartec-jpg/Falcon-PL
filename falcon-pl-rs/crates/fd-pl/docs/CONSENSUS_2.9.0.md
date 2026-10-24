# Falcon Consensus 2.9.0 — on-chain 4-of-6 QC

**Product:** Falcon PL (proof of **participation**, not stake-yield)  
**Status:** locked for v2.9.0

## Pay vs agree

| | Rule |
|--|------|
| Who is paid | Pack / check **work** at epoch settle (emissions + fees) |
| Bond | Lottery ticket + slash target, not APY |
| When a ledger is *the* ledger | **4 of this height’s 6** committee members signed it, and that cert is on the next header |

## Committee

- Lottery `v6` = `f(tip_hash, bonds, view)` (2.8.1). Same tip ⇒ same 6.
- 1 packer + 5 checkers, frozen at tip commit.
- ONLINE / pong = pack permission only.

## Commit QC (tip)

- Need **4 distinct committee members** (packer counts).
- n &lt; 6: keep the old small-set rule (`soft_votes_needed`).
- 3-of-5 checker tally is **not** enough to move balances.
- Those 4 (voter, sig) form a `LedgerCert`. The **next** ledger carries `parent_cert`.
- Height 1: `parent_cert = None`.

## Skip / view-change

- Same bar: **4 of the 6**.
- After `proposal_timeout`, **every committee seat** may vote view-change even with an empty local mempool (packer silence).

## Equivocation

- Packer: two bodies at one height → slash (existing).
- Checker: two OK hashes at one height → slash (new).

## Hard QC

- Still `ceil(n × 0.6)` of bonded. Confirm flag only. Does not pay like staking. Does not replace the 4-of-6 cert.
