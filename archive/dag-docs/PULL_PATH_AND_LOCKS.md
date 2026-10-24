# Pull-on-demand, lock hub, mesh convergence

## #2 Pull-on-demand (done)

| Message | Role |
|---------|------|
| `inv` | Announce batch/cert **hashes** only |
| `want` | Request missing hashes from announcer |
| `batch` / `cert` | **Bodies** — creator seeds once + serves on want; **no mesh rebroadcast** |
| `sig` / tip-* / `tx` | Still flood (small) |

`PullTracker` (`fd-net`): pending wants, per-peer in-flight cap (32), timeout 800ms, retry from other announcers.

`on_remote_cert` returns missing batch hashes → opens wants (catch-up primitive).

## #1 Lock hub — live ticks wired (0.47)

`EngineParts` (`fd-node/src/parts.rs`) is the **live** daemon hub:

| Subsystem | Lock |
|-----------|------|
| mempool | `RwLock<Option<…>>` |
| dag | `RwLock<Option<…>>` |
| ordering | `Mutex<Option<…>>` |
| execution | `RwLock<Option<…>>` |
| meta | `Mutex` |

**Order:** mempool → dag → ordering → execution → meta (never reverse, never across await).

- `advance_da` takes **only dag write**.
- Composite paths use `with_all_mut` via **`Option::take`** (no placeholder DAG alloc per call — the swap+`CertificateDag::new` path was a live TPS cliff).
- RPC uses `with_engine` / `with_all_mut` (exclusive bridge, lock-order safe).
- No outer `Mutex<NodeEngine>` on DA tick / ledger tick / inbound.

### Tests

- `parts::tests::concurrent_da_and_ledger_ticks_no_deadlock`
- `parts::tests::composite_create_batch_advance_ledger_matches_sequential_baseline`
  (ordered set + state root match single-lock sequential baseline under concurrent `create_batch` + `advance_da` + `advance_ledger`)

### Before/after flood (5× NTX=200, 5 vals, hmac-dev)

Harness: `FLOOD=1 NTX=200 scripts/multinode_metrics.sh`  
Reports settle TPS, soft p50/p99, cert round-lag between nodes.

| Build | median settle_tps | mean settle_tps | median min_exec | agree | round_lag |
|-------|-------------------|-----------------|-----------------|-------|-----------|
| **before** outer `Mutex` (0.46 main) | 0.53 | 76 | 16 | YES | 0 |
| **after** hub Option-take (0.47) | 1.49 | 148 | 45 | YES | 0 |

**Peak when mesh converges:** both can land in the hundreds (before ~380–670 TPS, after ~730 TPS full 200/200).  
**Bimodal stall:** both can also sit ~0.5–4 TPS with partial apply until pack/cert chain advances — **pack/mesh convergence (#3) dominates lock contention** under flood. Hub migration did **not** invent the stall; Option-take removed the artificial hub regression from placeholder alloc.

Interpretation for #1: lock split is correctly wired and measured; remaining live gap to a reliable sim-band floor is #3 (mesh/pack), not outer-mutex CPU alone.

## #3 Convergence (in progress — see `MESH_CONVERGENCE.md`)

- Missing parents → want (via `on_remote_cert` return)
- **Fixed primary packer** `sorted_unl[0]` (not cert-round; mesh lag caused zero packers)
- Open-batch force rebroadcast under stall
- Stall signature dump in `multinode_metrics.sh`
- Failure matrix: `failure_matrix.sh` — after stall-band ≤30%

Still open: residual bimodal stalls with primary assigned + empty uncert + full mempool; soak only after that; bincode later.

## Build

`falcon-dagd/0.48.0-rust` — hub ticks + #3 packer/mesh diagnosis fixes.
