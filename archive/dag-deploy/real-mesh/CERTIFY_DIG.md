# try_certify dig (N=6 thrash)

## What the N=6 profile showed

Under multipack load (~50/s, 30s):

| Counter (val1 Δ) | Value | Meaning |
|------------------|-------|---------|
| local_admits_ok | 245 | Admit fine |
| batches_created | 5 | Pack starves |
| certified | 22 | Some certs land |
| **try_certify_attempts** | **16452** | Thrash |
| **try_certify_ok** | **0** | Misleading |
| foreign_sig_merged | 105 | Sig merge traffic |
| ingest_merge_ok | 4359 | Body/sig merge work |

`try_certify_ok=0` while `certified` rose meant **most real certs never went through `try_certify_counted`**, or counted attempts were almost all **futile re-entries**.

---

## Code path (how certs actually form)

```text
remote PartialSignature
    → on_remote_sig
        → dag.add_signature  ──► dag.try_certify  (uncounted)
        → try_certify_counted (often redundant)

conf_pool_maintenance (every check tick)
    → force_open_cert_attempts on EVERY own open
        → try_certify_counted  (even when weight low / parents missing)

ingest_batch / conf_pool sign
    → dag.try_certify  (uncounted)
```

### `CertificateDag::try_certify` gates

```rust
// crates/fd-dag/src/lib.rs
1. already certified? → None
2. support_weight < unl_threshold? → None   // equal weights 1/N; need ≥0.6
3. every parent certified? → else None      // PARENT CHAIN
4. else insert Certificate
```

**Equal UNL weights:** each val has weight `1/N`.

| N | weight/val | sigs needed for 0.6 |
|---|------------|---------------------|
| 3 | 0.333 | **2** |
| 6 | 0.167 | **4** |

Larger N ⇒ more Falcon sigs per batch before local try_certify can succeed, more foreign merges, more parent-frontier races.

### Why attempts exploded

1. **`force_open_cert_attempts` every maintenance tick** on all own opens, including not-ready batches.  
2. **`on_remote_sig` always `or_else(try_certify_counted)`** even after `add_signature` already certified (retry hits “already certified” → None, and **still counted as attempt** before this dig).  
3. **Parent-missing** batches with enough weight still re-attempted forever until parents catch up (multipack widens parent frontier).  
4. **Pack bound** (few batches) means less progress while the cert loop keeps spinning on stale opens.

Not Falcon re-verify thrash: comments already fixed re-verify on every try; the waste is **call volume + parent/weight fail loops**.

---

## Fixes landed (this dig)

In `crates/fd-node/src/engine.rs`:

1. **`try_certify_counted`**
   - No-op (no attempt count) if already certified / no batch / **weight low** / **parents missing**
   - Classify skips: `try_certify_weight_low`, `try_certify_parents_missing`, `try_certify_already`, …
   - Only count attempts when the batch is actually cert-ready

2. **`force_open_cert_attempts`**
   - Same weight/parent early-outs before force counter

3. **`on_remote_sig`**
   - Skip `try_certify_counted` if `add_signature` already certified

**Expected effect:** CPU drop under N=6 load; dig counters become truthful; **does not by itself fix** “need 4-of-6 sigs + parent order” (that’s protocol scale).

---

## Remaining scale-out work (real capacity)

| Issue | Why it blocks scale-out |
|-------|-------------------------|
| Quorum sigs ∝ N | 2-of-3 vs 4-of-6 Falcon collect/verify |
| Parent-certified barrier | Multipack widens waiting set |
| Pack rate | Still few batches vs admits under load |
| Co-located vals | 3 processes/host share CPU |

Architectural directions (not done here):

- Cheaper / batched cert aggregation  
- Narrow pack schedule (pulse already sketched in engine) so parent frontier stays linear  
- Separate “voting UNL” size from “machine count” carefully  
- Bound open uncertified set hard  

Success metric remains: **`exec_tps_min(N=6) > exec_tps_min(N=3)`** on Phase A curve.

---

## How to re-check after rebuild

```bash
cd ~/falcon-dag/falcon-dag-rs   # full tree (has --node-profile)
cargo build -p fd-node --bin falcon-dagd --release
cp target/release/falcon-dagd ~/falcon-dag-mesh/bin/
rsync -az ~/falcon-dag-mesh/bin/falcon-dagd 100.64.0.3:~/falcon-dag-mesh/bin/
python3 ~/falcon-dag/deploy/real-mesh/09-profile-and-n3-steady.py
```

## Post-deploy re-profile (2026-08-10)

Source: full tree `~/falcon-dag/falcon-dag-rs` (workspace `~/falcon-dag-rs` lacked `--node-profile`).

| metric (N=6, 50/s × 30s) | **before** fix | **after** fix |
|--------------------------|----------------|---------------|
| try_certify_attempts (avg) | **13979** | **0** |
| try_certify_ok (avg) | 0 | 0 |
| certified (avg) | 20.8 | 19.5 |
| batches_created (avg) | 4.2 | 4.0 |
| exec_tps_min | 7.3 | 6.1 |
| CPU val1 mean % | 56.7 | 52.4 |
| CERT_BOUND hint | yes | **gone** |

**Interpretation**

- Fix **worked for thrash**: futile counted attempts eliminated (was the dig lie + wasted loop entry).
- Real certs still form (~20/node) via **uncounted** `dag.try_certify` / remote install paths — so `try_certify_ok=0` is expected when ready-path is rare and force_open early-outs on weight/parents.
- **Throughput not fixed** — still PACK_BOUND + SIG_GOSSIP + exec lag. Scale-out remains a pack/sig/parent-frontier problem, not “spin try_certify harder.”

N=3 multipack still healthy post-redeploy: multi-client **~41 exec_tps_min**, final roots agree.

Next code focus: **pack/sign rate** and **foreign-sig merge cost**, not more try_certify spinning.
