# Real Narwhal DA / ledger split

## The point

| Layer | Responsibility | Not responsible for |
|-------|----------------|---------------------|
| **DA worker** | Mempool → batches → gossip → **availability certificates** | Ordering, execution, balances |
| **Light ledger (order-exec)** | Order **certificates only** → execute payloads from DA store | Packing batches from mempool |

That split **is** the architecture. One process may run **both** roles, but as **separate code paths and ticks**, not one muddled loop.

## CLI

```bash
falcon-dagd --role full          # default: both paths, separate ticks
falcon-dagd --role da-worker     # pack/sign/cert only (no execute)
falcon-dagd --role order-exec    # order certs + execute only (no pack)
```

## Metrics

```json
{
  "role": "da-worker",
  "runs_da": true,
  "runs_ledger": false,
  "batches_created": 12,
  "certified": 10,
  "executed_txs": 0,
  "ledger_commits": 0
}
```

```json
{
  "role": "order-exec",
  "runs_da": false,
  "runs_ledger": true,
  "batches_created": 0,
  "ordered": 11,
  "executed_txs": 400,
  "ledger_commits": 10
}
```

## Code map

| Path | Methods |
|------|---------|
| DA | `submit_tx`, `create_batch`, `advance_da`, sign remote batches |
| Ledger | `advance_ledger` → `ordering.recompute(certificates)` then exec from `dag.batches` |
| Never | DA must not call `advance_ledger`; ledger must not call `create_batch` |

## Smoke

```bash
./scripts/narwhal_split_smoke.sh
```

## Ordering (Phase 42–43)

| Phase | Mechanism | Docs |
|-------|-----------|------|
| **43 primary** | **Bullshark** — even-round anchors, f+1 support from DAG | [BULLSHARK.md](BULLSHARK.md) |
| **42 secondary** | Tip-BFT wire: `tip_proposal` → `tip_vote` → `tip_commit` | [TIP_BFT.md](TIP_BFT.md) |

```bash
./scripts/bullshark_smoke.sh
./scripts/tip_bft_smoke.sh
```

## What this is not yet

- Separate binaries for worker vs orderer (same binary, strict roles)
- Full paper Bullshark fallback/random leaders (steady-state even rounds)
- Permissionless committee (still UNL)

## What it *is*

A hard split of **responsibilities** matching the Narwhal story: workers make data available; the ledger **Bullshark-commits** certificate anchors (DAG-derived) and then executes.
