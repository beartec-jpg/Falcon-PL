# Tip-BFT — light ordering over certificate tips

**Phase 42.** Ordering is no longer a pure full-cert-set recompute on every tick.

## Split (Narwhal + Bullshark-class)

| Layer | Wire / state | Responsibility |
|-------|----------------|----------------|
| **DA** | `batch`, `sig`, `cert` | Availability only |
| **Tip-BFT** | `tip_proposal`, `tip_vote`, `tip_commit` | Agree which **certified tip** to commit each round |
| **Expand + exec** | local | Topo-expand the cert sub-DAG under each commit; execute DA payloads |

Consensus messages are **small tip metadata**, not full batches.

## Protocol

1. **Round `r`, view `v`**: leader = sorted UNL\[`(r-1+v) % n`\]
2. Leader **proposes** the canonical certified tip extending `last_commit`  
   (`pick_tip`: max-round tips with no certified child; max hash)
3. UNL members **vote** once per round (signed)
4. When vote weight ≥ `unl_threshold` → **TipCommit**
5. Nodes **expand** all cert ancestors of the committed tip (deterministic topo: causal round, timestamp, hash) and execute
6. **View-change**: if no proposal after a few idle ticks, `v++` (covers DA-only scheduled leaders in role-split deploys)
7. Stale proposals for a non-canonical tip are dropped when the cert frontier advances

`proposal_id` is **proposer-independent** (`round + tip + parent`) so concurrent leaders for the same tip merge votes.

## Metrics

```json
{
  "architecture": "narwhal-da | tip-bft-order-exec (split)",
  "tip_commits": 3,
  "tip_bft_round": 4,
  "tip_bft_last_commit": "…",
  "ledger_commits": 3,
  "ordered": 12,
  "executed_txs": 400
}
```

## Smoke

```bash
./scripts/tip_bft_smoke.sh
./scripts/narwhal_split_smoke.sh   # still holds with tip-BFT on order-exec
```

## Status (Phase 43)

**Bullshark is now the primary** commit path (see [BULLSHARK.md](BULLSHARK.md)).  
Tip-BFT remains as an optional secondary wire path / accelerator.

## What this is not

- Permissionless / rotating stake committees
- Separate orderer binary (same `falcon-dagd`, ledger role path only)

## What it is

A real second protocol path for tip voting; order also advances on **Bullshark anchors** derived from the cert DAG alone.
