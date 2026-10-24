# Multi-Packer Mesh Settlement Debug Plan

**Context:** Robustness suite (75 runs, Falcon-512, 5-node localhost) shows
multi-packer configurations stall at ~160–180 TPS with 0% full-apply on
multi-account loads. Single-packer achieves 250–280 TPS reliably (100% or
80% full-apply depending on profile).

```
cfg              rate   mean    min    max   stdev
SP_A20           100%  248.1  182.8  293.5   40.3   ← most robust
SP_A1             80%  282.2  261.2  300.9   17.2   ← fastest ≥80%
MP_A1             40%  238.7  229.6  247.8   12.8   ← best multi-pack result
MP_A5/10/20        0%   stall ~160-180 or 30s hang
```

---

## Root Cause Analysis

Six compounding bugs cause the stall. They interact — fixing only one or two
will not resolve the 30-second hang reliably.

### Bug 1 — `certified_tips()` is O(n²) under multi-packer load

**File:** `src/falcon_dag/certificate_dag.py` lines 109–116

```python
def certified_tips(self) -> list[str]:
    tips = []
    for bh in self.certificates:                      # O(certs)
        kids = self.children.get(bh, set())
        if not any(k in self.certificates for k in kids):  # O(children)
            tips.append(bh)
    tips.sort()
    return tips or [GENESIS_BATCH_HASH]
```

Each call iterates all certificates and all their children. With multi-packer,
every validator proposes its own concurrent batch each round (5 batches/round
for a 5-node cluster). After K rounds you have K×5 certified batches.
`certified_tips()` is called in `create_batch()` (every tick), in
`on_remote_cert()`, and transitively through `_certify_pending()`. After ~40
rounds (≈2 s at 0.05 s interval) the scan is over 200 certs per incoming
message. The CPU hot-path delays cert processing and stalls the settlement loop.

### Bug 2 — `_certify_pending()` scans all batches on every signature

**File:** `src/falcon_dag/engine.py` lines 139–145

```python
def _certify_pending(self) -> None:
    for bh in list(self.dag.batches.keys()):   # O(all batches)
        if bh in self.dag.certificates:
            continue
        cert = self.dag._try_certify(bh)
        if cert:
            self._on_new_cert(cert)
```

This is called on every incoming `sig` message and every `on_remote_batch`.
With 5 concurrent packers producing 5 batches per round, an incoming flood at
200 TPS generates thousands of sig messages per second. Each triggers a full
scan of all uncertified batches. This certification storm causes the settlement
thread to fall behind, certs lag, and new batches can no longer reference
certified parents.

### Bug 3 — `_try_certify()` deadlocks on a single slow parent

**File:** `src/falcon_dag/certificate_dag.py` lines 317–335

```python
def _try_certify(self, batch_hash: str) -> Certificate | None:
    ...
    for p in batch.parents:
        c = self.certificates.get(p)
        if c is None:
            return None   # ← won't certify until ALL parents certified
```

A batch cannot be certified until **every parent** is certified. With
multi-packer each validator picks `certified_tips()` as parents (up to 6).
If one parent's sigs are delayed in gossip (Falcon-512 sigs are ~666 bytes
each; 5 validators × 5 concurrent batches = 25 large gossip messages per
round), the entire child batch stalls. Dependent batches pile up, and the
whole DAG front freezes until the slow sig finally arrives or the 120-second
TTL prunes the stale batch.

**This is the direct mechanism behind 30-second hangs**: one stalled sig →
parent won't certify → every child batch blocks → ordering halts → execution
stops.

### Bug 4 — Multi-packer account conflict: stale execution state

**File:** `src/falcon_dag/engine.py` lines 209–225 (`create_batch`)

```python
state = self.execution.state.clone()   # last hard-committed state only
chosen = []
for tx in self.mempool.ordered(now):
    if not self.owns_tx(...): continue
    if not state.can_apply(tx): continue
    state.apply(tx)
    chosen.append(tx)
```

`self.execution.state` is only updated when `apply_new_ordered()` runs, which
requires certs to arrive and be ordered. Under multi-packer, cert lag (Bug 3)
means the execution state can be many rounds stale. Validator A packs
`acct3/seq=7`; Validator B's packer also sees `acct3/seq=7` as valid (the
state hasn't advanced past it). Both include the conflicting tx. When ordered
execution runs, one fails — driving the ~80% execution rate observed.

The `already` set (checking pending `batch.tx_ids`) does not fix this: it
deduplicates by `tx_id`, but two validators may include **different** tx
objects with the same `(account, sequence)` — a valid double-spend race that
only materialises at execution time.

### Bug 5 — `owns_tx()` uses non-real timestamps in the sim flood path

**File:** `src/falcon_dag/sim.py` line 209 and `engine.py` lines 66–81

The simulation's `flood()` creates transactions with `first_seen=float(i)`
(loop index, not wall-clock time). The orphan rescue logic in `owns_tx()` then
computes `age = now - float(i)`, which is a meaningless huge number, instantly
activating the orphan clause for all transactions. Every packer can pack every
tx regardless of shard — the sharding intended to prevent conflicts is silently
disabled in simulation, making simulation results unreliable as a guide for
production tuning.

In production (real timestamps) the hard 2-second cliff means: during the
first 2 s of a flood, each packer only packs ~1/N of the mempool (correct),
but if a packer's shard contains `acct3/seq=7` while `acct3/seq=6` is in
another shard and that validator is slow, the first packer is permanently
blocked from advancing that account — starvation within a shard with no
recovery mechanism short of the 2-second cliff.

### Bug 6 — `_compute_total_order()` recomputes from scratch on every cert

**File:** `src/falcon_dag/ordering.py` lines 51–99

`try_commit()` calls `_compute_total_order()` which topologically re-sorts the
**entire** certificate DAG from scratch on every invocation. The `_causal_round`
memo dict is local to each call, so causal depth is recomputed for every cert
on every ordering tick. With multi-packer generating 5× the certs per second,
ordering runs 5× more often and each run's cost grows with the cert count —
O(certs²) total work. This amplifies Bugs 1 and 2 and causes the ordering
thread to fall behind the cert arrival rate under sustained multi-account load.

---

## The Compound Stall Mechanism (How 30-Second Hangs Occur)

```
1. 5 packers propose concurrently → 5 batches reference same certified tips
2. Each batch needs 3/5 Falcon-512 sigs → 15 large gossip messages per round
3. One gossip delivery is slow or dropped
4. Parent batch can't certify (Bug 3) → child batches pile up uncertified
5. _certify_pending() storms on every incoming sig (Bug 2) → CPU saturated
6. certified_tips() scan grows (Bug 1) → batch creation stalls
7. _compute_total_order() cost grows (Bug 6) → ordering falls behind
8. Packers re-propose on stale parents → uncertified backlog explodes
9. System hangs until the slow sig arrives OR 120s TTL prunes the batch
10. Backlog clears all at once → burst, then next hang
```

This explains the observed pattern: stall ~160–180 TPS (80% exec) then a
brief burst, then stall again.

---

## Fix Plan

### Fix 1 — Incremental `certified_tips()` — O(1)

Replace the full scan with an incrementally maintained `_tip_set: set[str]`.

**Changes in `certificate_dag.py`:**

- Add `self._tip_set: set[str] = {GENESIS_BATCH_HASH}` in `__init__`.
- In `ingest_batch()` (when `is_new`): add the new `batch_hash` to
  `_tip_set`, then remove each of its parents from `_tip_set` (they are no
  longer leaf tips).
- In `_try_certify()` on success: no change needed for the tip set itself
  (tips are tracked at ingestion time); however if a cert arrives for a batch
  we've never ingested (remote-cert-first path in `on_remote_cert`), add it to
  `_tip_set` and remove its parents.
- `certified_tips()` returns `sorted(bh for bh in self._tip_set if bh in self.certificates)`.

Complexity: O(parents) on each batch ingestion, O(1) lookup per tip query.

### Fix 2 — Targeted child certification — replace `_certify_pending()`

Instead of scanning all batches, track which batches are "threshold-ready but
parent-blocked". When a parent gets certified, only retry its direct children.

**Changes in `engine.py` and `certificate_dag.py`:**

- Remove the `_certify_pending()` full-scan calls.
- After `_on_new_cert(cert)`, call `_certify_children(cert.batch_hash)`:

```python
def _certify_children(self, batch_hash: str) -> None:
    for child_hash in list(self.dag.children.get(batch_hash, ())):
        if child_hash in self.dag.certificates:
            continue
        cert = self.dag._try_certify(child_hash)
        if cert:
            self._on_new_cert(cert)
            self._certify_children(child_hash)  # recurse depth-first
```

Complexity: O(children) per cert event instead of O(all batches).

### Fix 3 — Persistent causal-round memo in `OrderingEngine`

**Changes in `ordering.py`:**

- Promote the `memo` dict to `self._causal_memo: dict[str, int] = {}` in
  `__init__`.
- In `_compute_total_order()`, pass `self._causal_memo` instead of a local
  `{}` so previously computed depths are reused across calls.
- Only invalidate `_causal_memo` entries if the cert set ever shrinks (which
  it doesn't under normal operation — certs are monotonically added).

Additionally, make ordering incremental: rather than recomputing the full
topological order from scratch, extend the existing `self.ordered` list with
newly certifiable batches only (those whose parents are already in
`ordered_set`).

### Fix 4 — In-flight account sequence tracking

**Changes in `engine.py` / `certificate_dag.py`:**

Add `self._pending_seq: dict[str, int]` (account → highest sequence included
in any **ingested, uncertified** batch). Update it:

- In `ingest_batch()`: for each tx in the batch, if `tx.sequence > pending_seq.get(tx.account, 0)`, update.
- When a batch is certified/pruned: recompute from remaining uncertified batches (or decrement, bounded scan).

In `create_batch()`, before `state.can_apply(tx)`, also skip any tx whose
`tx.sequence <= self._pending_seq.get(tx.account, 0)` for non-owning packers.
This prevents two packers from both packing the same `(account, sequence)`.

### Fix 5 — Fix `flood()` simulation timestamps

**Changes in `sim.py`:**

In `flood()`, replace `first_seen=float(i)` with a real monotonic timestamp:

```python
now = time.time()
tx = Transaction.create(src, s, dst, 1, fee=10 + i % 5, first_seen=now + i * 1e-6)
```

This makes simulation results consistent with production `owns_tx()` behaviour
and restores the shard isolation the orphan logic is supposed to provide.

For the orphan policy itself: replace the hard cliff with a linear ramp so
non-primary workers adopt a tx with probability `min(1.0, age / shard_orphan_age_s)`
rather than a binary all-or-nothing switch. This eliminates shard-gap starvation.

### Fix 6 — Raise `max_uncertified_per_creator` for multi-packer

At the default 0.05 s batch interval with 5 packers and 1–2 s Falcon-512 cert
latency, a single validator can easily have 20–40 uncertified batches in-flight.
The current default of 64 is adequate but leaves little headroom. Increase the
default to **128** and document that multi-packer deployments should set it to
`≥ ceil(cert_latency_s / batch_interval_s) × 2`.

### Fix 7 — Add stall-detection metrics

**Changes in `engine.py` `metrics()`:**

Expose:

| Metric | Description |
|---|---|
| `uncertified_tip_age_s` | Age (seconds) of the oldest uncertified batch that is blocking a current tip's certification |
| `cert_lag_s` | Rolling mean of (cert_time − batch_creation_time) for last 100 certs |
| `ordering_lag` | `len(certified) − len(ordered)` — certs waiting to be ordered |
| `pending_storm_count` | Number of `_certify_pending` calls since last tick (should be bounded post-Fix 2) |

These metrics make the stall directly observable without reproducing the 30 s
hang, and support the "trace a specific stall" requirement.

---

## Implementation Priority

| Priority | Fix | Expected Impact |
|---|---|---|
| P0 | Fix 2 (targeted child cert) | Eliminates certification storm — most direct cause of the hang |
| P0 | Fix 3 (persistent memo + incremental ordering) | Eliminates O(certs²) ordering cost |
| P1 | Fix 1 (incremental tips) | Eliminates O(n²) tip scan from hot path |
| P1 | Fix 4 (in-flight sequence tracking) | Brings multi-account exec rate to ≥95% |
| P2 | Fix 5 (sim timestamps) | Makes test results match production |
| P2 | Fix 6 (config limit) | Prevents false DoS rejection under burst |
| P3 | Fix 7 (metrics) | Observability; needed to confirm fixes in prod |

P0 fixes alone should eliminate the 30-second hang. P1 fixes should push
multi-packer multi-account from 0% to ≥80% full-apply. All fixes together
target ≥90% full-apply at 250+ TPS for MP_A10 and MP_A20 profiles.

---

## Files to Change

| File | Changes |
|---|---|
| `src/falcon_dag/certificate_dag.py` | Fix 1 (`_tip_set`), Fix 4 (`_pending_seq`) |
| `src/falcon_dag/engine.py` | Fix 2 (`_certify_children`), Fix 4 (pack guard), Fix 7 (metrics) |
| `src/falcon_dag/ordering.py` | Fix 3 (persistent memo, incremental order) |
| `src/falcon_dag/sim.py` | Fix 5 (real timestamps in `flood()`) |
| `src/falcon_dag/config.py` | Fix 6 (`max_uncertified_per_creator` default 128) |

No changes to types, crypto, network, or execution layers are required.
All fixes are backward-compatible — existing single-packer configs are
unaffected.

---

## Verification Approach

Re-run `scripts/robust_tps_suite.py` (or equivalent) after each P0/P1 fix:

1. After Fix 2+3: confirm 30-second hangs are gone for MP_A5.
2. After Fix 1+4: confirm MP_A10/A20 full-apply rate rises from 0% to ≥80%.
3. After all fixes: full 75-run suite; target MP_A10 ≥80% full-apply at
   ≥220 TPS mean, MP_A20 ≥80% at ≥200 TPS mean.
4. Confirm SP_A20 (100% baseline) is unchanged.
