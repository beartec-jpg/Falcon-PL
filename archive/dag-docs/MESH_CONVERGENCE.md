# Phase 48 — Pack/mesh stall (#3) diagnosis + first fixes

## Diagnosis-first (not guess-and-soak)

When a flood trial drops into the **0.5–4 TPS** band, `multinode_metrics.sh` dumps a
**stall signature** per node:

- `mempool`, `uncertified`, `max_cert_round`, `primary_packer`, `assigned_packer`
- `open_batch_creators`, `oldest_uncertified_age_s`, `tip_fp`
- pull: `pending_pulls`, `inflight_total`, `retries`
- `hard_final`, `bullshark_anchors`

### Confirmed root cause (from 10-trial traces)

**Packer keyed on local `max_cert_round % n` desyncs under mesh lag.**

When cert frontiers diverge, nodes compute **different primaries**. Trace pattern:

```
val1: primary=val2 assigned=False
val2: primary=val3 assigned=False
val3: primary=val1 assigned=False
→ nobody packs
```

Also: secondary activation on “clear tip + mempool” dual-packs when the primary’s
open batch has not arrived yet (asymmetric DA view).

**Not** the dominant signature: pull pending stuck (usually `pending_pulls=0` at
end of stall), or all nodes yielding to rivals with open uncertified (often
`uncertified=0` with mempool still deep).

Thin `hard_final` (often 1 = genesis) tracks the same lag: without cert depth /
anchors, hard finality cannot advance past `hard_finality_h`.

## Fixes landed (0.48)

| Change | Why |
|--------|-----|
| **Fixed primary** = `sorted_unl[0]` | Same packer on every node regardless of cert lag |
| **Narrow secondary** only on stale open batches / silent clear | Avoid dual-pack race |
| **Force rebroadcast** of open uncertified self-batches (every 3 DA ticks) | Mesh catch-up so peers can sign |
| **`pack_stall` + pull stats** in metrics / flood harness | Keep diagnosing residual stalls |
| **PullTracker** unit tests: free inflight on body, timeout retry, max-retries drop | 32×body/peer ceiling stays bounded |
| Composite determinism test still green | Pack/mesh changes cannot break ordered set + state root |

## Multi-trial gate (NTX=200 flood, 5 vals, hmac-dev)

Acceptance target: median settle TPS ≥ 50, stall-band ≤ 30%, median min_exec ≥ 100
across ≥10 trials.

| Build | median settle TPS | stall-band | median min_exec | notes |
|-------|-------------------|------------|-----------------|-------|
| Pre-#3 hub (Phase 47) | ~1–2 | ~90% | ~40–50 | bimodal, packer/mesh unknown |
| **Phase 48 fixed primary** (best 10-trial) | **~226** | **~40%** | **~187** | root cause confirmed + fixed; residual bimodality |
| Aggressive force-pack experiments | ~1–2 | ~90% | low | **regressed** multi-node agreement — discarded |

Honest status: **median moved into the sim band when mesh converges**, and the
“zero packers” desync is fixed. **Stall fraction is not yet ≤30%** — remaining
partials show stable primary=val1, `uncertified=0`, deep mempool, low cert depth
(pack/cert pipeline still stalls intermittently; not mutual yield).

## What not to do next

- Do **not** soak 12h/72h yet — still measuring residual pack/cert stalls.
- Do **not** re-key packer on local cert round without a global tip view.
- Do **not** force-pack aggressively without mesh backpressure (desyncs secondaries).

## Next #3 steps

1. Why create_batch returns None with `assigned=true`, `uncert=0`, mempool ≫ 0 (chosen empty / tip stuck).
2. Cert propagation under pull path when primary is also the sole flood RPC target.
3. Then re-gate 10-trial stall-band ≤30%, then failure matrix + soak.

## Build

`falcon-dagd/0.48.0-rust`
