# Multi-Lane Architecture (source of truth)

**Sequence/nonce model** (not UTXO). **Lane = one UNL validator**, not one account.

## Mental model (correct and implemented)

```
                 Global mempool (admit on any node; gossip fills locals)
                            │
           deterministic FNV(account) % |UNL|
           ┌────────────────┼────────────────┐
           ▼                ▼                ▼
     Lane = val1      Lane = val2      Lane = val3
     owns shard 0     owns shard 1     owns shard 2
     pack own tip     pack own tip     pack own tip
     DA cert vote     DA cert vote     DA cert vote
     (sig weight)     (sig weight)     (sig weight)
           │                │                │
           └────────────────┼────────────────┘
                            ▼
              Collect finalized (certificate + body) from ALL lanes
                            ▼
                    Bullshark (order only — AFTER finalization)
                            ▼
              Apply in agreed order → final ledger
              (execution-time sequence gate = safety authority)
```

## What is already done (do not re-open as "TODO refactor")

| Item | Status | Code |
|------|--------|------|
| Deterministic account → lane owner | Done | `account_shard` / `owns_tx` (FNV % \|UNL\|) |
| Independent lane packing | Done | Owner packs only; parents = own finalized tip + other *finalized* tips |
| No Bullshark during packing | Done | `select_batch_parents` never uses `preferred_parent` |
| Lane cert voting = DA quorum | Done | `try_certify` + `unl_threshold` in fd-dag |
| Collect finalized from all lanes | Done | `collect_finalized_lane_certs` |
| Bullshark after finalization | Done | `advance_da` then `advance_ledger` |
| Apply in agreed order | Done | Expand under anchors → `apply` / `apply_ordered` |
| Conflict filter at pack | Done | `lane_dedup_conflicts` — lowest `tx_id` per (account, sequence) |

Architecture string: `global-mempool | multi-lane-da | bullshark | pull-path`.

## Sequence/nonce model (not UTXO)

- There is **no UTXO set**. State is per-account **next sequence** (nonce).
- **Conflict / "double-spend"** = two txs claim the same `(account, sequence)`.
- Plans written in UTXO language must be rewritten, or a UTXO layer added (large scope).

### Conflict handling (defense in depth)

| Stage | Role |
|-------|------|
| Pack / split | `lane_dedup_conflicts` — deterministic survivor per (account, sequence) |
| Admit | `can_admit` sequence window |
| **Execution (safety authority)** | `apply` / `apply_ordered` — only exact next sequence wins over Bullshark order |

**Do not** remove the execution-time gate in favor of "split-time only." Split-time filtering is necessary for determinism but **not** sufficient under partition/equivocation of a lane owner. Property tests: `double_spend_first_wins`, partition merge double-spend cases.

## Lane identity

| Concept | Definition |
|---------|------------|
| Lane | One UNL validator (`val1` … `valN`) |
| Owner of account | `sorted_unl[FNV-1a32(account) % n]` |
| Packer | Owner only |
| Lane tip | Last **finalized** (cert + body) batch created by this node |
| Parents | Own lane tip + other lanes' **finalized** tips (Bullshark DAG connectivity). Never foreign *open* batches |

**Not** "one lane per account." Per-account lanes would be a separate redesign (many lanes per validator).

## Global mempool realism

- Mempool is **per-node**, eventually consistent via gossip (`WireMsg::tx`).
- Submit to a non-owner relies on flood / `rebroadcast_mempool_txs` to reach the lane owner.
- Under load/latency this causes packing gaps — a primary contributor to the stall band.

## Real remaining work (not the DA/Bullshark split)

### Phase B — Lane-owner reliability and routing — **DONE**

1. **Routing:** multi-lane rebroadcast uses **directed** delivery to `account_owner_id` (not mesh flood).
2. **Metrics:** `shard_flow.directed_to_owner`, `stuck_not_owned_pending`, `stuck_owner_age_max_s`, `flood_fallback`.
3. **Owner down:** accounts on that lane **stall until recovery** (no automatic failover). See `lane_owner_down_stalls_accounts()`.


### Phase C — Stall band / full mesh reliability

1. Instrument mempool→pack→sig→cert→finalize→order latency **per stage, per lane**.
2. Pull-path tune (in-flight, timeout/retry); avoid harmful certified-body reseed storms.
3. Drive stall band toward target (see `MESH_CONVERGENCE.md`).
4. Functional gate: `scripts/lane_model_3node.py` — all nodes same **executed** and **certified** after light load (Falcon 3-node runs can still FAIL on exec desync / cert diverge).

### Phase D — Bullshark under stress

- Property tests: equivocating lane owner, delayed anchor support (f+1), rider commits after stall.
- All validators identical `ordered` and `executed`.

### Phase E — Scaling validation

- Measure TPS vs validator/lane count on real mesh.
- Cross-lane parenting cost; no unmeasured "N×200 TPS" claims.

### Phase F — Optional large scope

- Per-account lanes or true UTXO layer — only if product requires it; separate project.

## Tests (lane model)

| Suite | Role |
|-------|------|
| `scripts/lane_model_3node.py` | **Primary** functional: 3 nodes, global mempool → lanes → Bullshark → ledger |
| Property tests in `engine.rs` | Double-spend first-wins, partition merge |
| Old multipack MP_A10 floods | **Not** the definition of "lane model correct" |

## Config

| Knob | Lane model |
|------|------------|
| `--single-packer=false` | Multi-lane (one lane per UNL node) |
| `--unl val1,val2,val3` | 3-lane smoke |
| Bullshark | Only after finalized collect |

## Historical note

Earlier "TODO: decouple packing from Bullshark / collect finalized / apply ordered" was **already implemented**. Remaining pain is **networking reliability and honest scaling**, not re-doing the phase split.

## Implementation review (summary)

External review conclusion (accepted): architecture matches the plan; main gaps are
(1) sequence vs UTXO framing, (2) mesh/cert propagation under load, (3) conflict
safety correctly resting on apply-time gates not split-only filters.

## Phase C (mesh pull) — implemented

- Pull missing bodies with honest announcers (cert source + broadcast "any"), not every peer as fake announcer.
- Throttled inv of recent finalized batches we hold (discovery without body flood).
- `lane_model_3node` Falcon: ledger exec **agrees** across nodes (cert may still lag by 1).

