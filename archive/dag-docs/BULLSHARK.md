# Bullshark ordering (Phase 43)

Primary light-ledger commit rule over the **Narwhal certificate DAG**.  
Zero extra consensus messages: commits are derived from certificate structure alone.

## Rule (simplified / Sui-style)

| Piece | Definition |
|-------|------------|
| **Even rounds** | Only even causal rounds elect leaders |
| **Leader** | `unl[(r/2 − 1) % n]` for even `r` |
| **Anchor** | Leader’s certificate in that round (lowest batch hash if several) |
| **Vote** | A certificate in round `r+1` that lists the anchor as a **parent** |
| **Commit** | ≥ **f+1** distinct UNL creators vote (`f = ⌊(n−1)/3⌋`) |
| **Order** | Expand all cert ancestors of each committed anchor (deterministic topo) |
| **Ride the DAG** | When committing a later anchor, also commit earlier anchors that are ancestors and already have f+1 support |
| **Multipack Option A** | Expand sort: `(causal_round, creator, round, hash)`; multi-packer also expands highest ready tip **per creator** (lex) so mesh receipt order never selects the ordered set. Env: `BULLSHARK_TIEBREAKER=ACCOUNT_ID_LEXICOGRAPHIC` (default). Batches are multi-tx — **creator** is the account-shard / packer-chain key. |

```
round:  0(genesis)  1     2(anchor)     3(votes)     4(anchor) …
leader:    —        —    val1            —          val2
commit: when ≥ f+1 round-3 certs parent val1’s round-2 cert
```

## What is *not* required

- `tip_proposal` / `tip_vote` / `tip_commit` (those remain as an **optional** secondary path)
- A separate orderer binary
- Full paper Bullshark waves with fallback/random leaders (steady-state even rounds only)

## Code map

| Module | Role |
|--------|------|
| `fd_order::BullsharkEngine` | `try_commit_from_dag`, `preferred_parent`, support counting |
| `NodeEngine::advance_ledger` | Bullshark first → expand → tip-BFT secondary → execute |
| DA `create_batch` | Prefer uncommitted even-round leader as parent (liveness) |

## Metrics

```json
{
  "architecture": "narwhal-da | bullshark-order-exec (split)",
  "bullshark_anchors": 2,
  "bullshark_last_anchor": "…",
  "bullshark_committed_rounds": [2, 4],
  "ordered": 15,
  "executed_txs": 200
}
```

## Smoke

```bash
./scripts/bullshark_smoke.sh
./scripts/narwhal_split_smoke.sh   # order-exec still executes under Bullshark
```

## Relation to tip-BFT (Phase 42)

| | Bullshark | Tip-BFT |
|--|-----------|---------|
| Messages | none (DAG only) | propose / vote / commit |
| Commit driver | even anchors + f+1 support | UNL tip vote threshold |
| Role | **primary** | secondary / accelerator |

Both expand under commits with the same deterministic topo.
