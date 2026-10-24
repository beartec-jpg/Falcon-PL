# Epoch committees (open validators without full-N voting)

## Goal

Grow an **open validator registry** while keeping per-decision agreement cost **O(k)**, not O(N).

## Model

```
Open registry (stake-bonded validators), size N → ∞
        │
        │ each epoch: sample committee C of size k
        ▼
Cert + Bullshark (or equivalent) only among C
        │
        ▼
Finality for that epoch’s batches
```

## Why this is not a “small fixed UNL”

| Fixed UNL | Committee |
|-----------|-----------|
| Same friends forever | Membership **rotates** |
| Closed list | Open **eligibility** (with bond) |
| Political centralization risk | Statistical security if sample is honest-majority |

## Risks (and mitigations)

| Risk | Mitigation |
|------|------------|
| Sybil open join | Stake / bond / unbonding delay |
| Corrupt committee | Large honest stake; random beacon; slash |
| Offline committee | View-change / secondary sample |
| Grinding for selection | VRF / commit-reveal beacon |

## Relation to shards

- **Shards** scale **throughput** (parallel ledgers).  
- **Committees** scale **open membership** on a ledger without growing vote width.  
- Production systems often use **both**.

## Falcon adoption path (not built yet)

1. Keep shard v0 meshes (capacity).  
2. Add `validator_registry` + stake stub.  
3. Epoch tick samples k ids into `active_unl` for cert weight.  
4. Pack workers remain optional outside active committee.

## Status

**Design only** (2026-08-10). Lab proof of capacity is **sharding**; committees are the open-membership track.
