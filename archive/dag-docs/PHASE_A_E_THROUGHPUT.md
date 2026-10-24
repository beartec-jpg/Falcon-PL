# Phase A–E throughput & stability plan (implemented)

Audit target: close **sim 500–900 TPS** vs **live 10–180 TPS** gap without abandoning Falcon-512.

## Phase A — Break the lock / crypto hot path

| Change | Where |
|--------|--------|
| `parking_lot::Mutex` on `NodeEngine` (no `.await` while held) | `fd-node/main.rs` |
| **VerifiedSigCache** + `CachingCrypto` wrapper | `fd-crypto` |
| **Trust admitted signatures** in `support_weight` (no re-Falcon on every certify) | `fd-dag` |
| Pre-verify batch sigs in `spawn_blocking` before engine mutate | `handle_inbound` |

## Phase B — Throughput defaults

| Default | Old | New |
|---------|-----|-----|
| `max_batch_parents` | 1 (linear) | **3** (DAG width) |
| `adaptive_interval` | false | **true** |
| `max_batch_txs` | 128/512 | **256** |
| CLI batch interval | 0.2s | **0.05s** |

Single-packer still on for multi-node agreement under pack-ahead.

## Phase C — Network path

| Change | Detail |
|--------|--------|
| Length-prefixed frames | `u32 LE + JSON` (legacy line still accepted) |
| Bounded inbound | capacity 16k, drop on full |
| `inv` message type | hash-only inventory announce API |

Full inventory-pull protocol is partial (API + framing ready); body still flooded once for correctness.

## Phase D — Stability

| Change | Detail |
|--------|--------|
| Prune hysteresis | TTL × 1.25 to reduce pack↔requeue bounce |
| Snapshot off hot path | `to_snapshot` under lock → `spawn_blocking` write |

## Phase E — Measurement

```bash
./scripts/live_vs_sim_bench.sh
./scripts/multinode_metrics.sh
```

KPI: **live apply TPS / sim TPS** rising; **agreement=YES**; admit ~100%; equiv=0.

## Build

`falcon-dagd/0.45.0-rust`
