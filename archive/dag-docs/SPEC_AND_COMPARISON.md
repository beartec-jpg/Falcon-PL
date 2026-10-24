# Falcon DAG — Full Spec Sheet, Pros/Cons, Falcon Comparison

**Status:** Research spinoff / optional layer-upgrade path  
**Not a replacement** for production Falcon Ledger (classic RPCA).  
**Date:** 2026-08-03  
**Viability suite verdict:** **CONTINUE** (see `VIABILITY_REPORT.json`)

---

## 1. Positioning

| Role | Description |
|------|-------------|
| **What it is** | Narwhal-style **mempool / data-availability DAG** + **federated UNL ordering** of certificates |
| **What it is not** | Drop-in replacement for Falcon `xrpld` consensus |
| **Spinoff** | Standalone chain / testnet product possible later |
| **Layer upgrade** | Long-term: sit **under** Falcon execution as a DA+ordering module while keeping Falcon accounts, fees, Falcon-512 sigs |
| **Production Falcon** | Unchanged (`qXRP`); continues classic single-ledger RPCA |

**Primary goal:** raise potential **tx throughput / effective speed** while **keeping large post-quantum signatures** (Falcon-512), by not forcing every fat signature through a single ledger-close bottleneck.

---

## 2. Architecture (two layers)

```
 Clients / wallets (Falcon-512 signed txs)
              │
              ▼
 ┌────────────────────────────────────────────┐
 │  A. Data Availability (Narwhal-style DAG)  │
 │  mempool → batches → UNL signs → certs     │
 └────────────────────┬───────────────────────┘
                      │ certificates only
                      ▼
 ┌────────────────────────────────────────────┐
 │  B. Ordering (federated / Option B)        │
 │  total order of certs → execute → state    │
 └────────────────────────────────────────────┘
```

---

## 3. Function specification (implemented modules)

### 3.1 Types (`types.py`)

| Function / type | Role |
|-----------------|------|
| `Transaction` | Payment-like tx: account, sequence, dest, amount, fee, first_seen |
| `Batch` | DA unit: tx set, parent batch hashes, creator, timestamp, partial sigs |
| `Certificate` | Proof batch is available: batch hash + ≥ UNL_THRESHOLD signers |
| `PartialSignature` | One validator’s attestation on a batch hash |
| `GENESIS_*` | Shared genesis batch/cert anchors |

### 3.2 Config (`config.py`)

| Parameter | Default | Meaning |
|-----------|---------|---------|
| `unl_threshold` | 0.80 | Weight needed to certify a batch |
| `max_batch_parents` | 6 | Certificate DAG width |
| `target_batch_interval_s` | 0.4 | Base batch cadence |
| `max_batch_txs` | 128 | Cap per batch |
| `hard_finality_h` | 6 | Ordered certs after inclusion for hard final |
| `adaptive_interval` | false | Enable load-aware cadence |
| `batch_interval_min_s` | 0.15 | Low-load (fast latency) |
| `batch_interval_max_s` | 1.2 | High-load (slower, fatter batches) |
| `adaptive_mempool_low/high` | 16 / 512 | Depth range for interpolation |
| `network_id` | 9002 | Falcon DAG regtest (not Falcon main/test) |

### 3.3 Mempool (`mempool.py`)

| Function | Behavior |
|----------|----------|
| `add` / `remove` | Insert/drop unconfirmed txs |
| `ordered` | Priority: fee density + age boost → first_seen → hash |
| `mark_done` | Drop soft-final/ordered tx and sequence competitors |

### 3.4 Certificate DAG — DA layer (`certificate_dag.py`)

| Function | Behavior |
|----------|----------|
| `ingest_batch` | Merge batch + parent edges |
| `add_signature` | Record UNL vote on batch |
| `_try_certify` | Certify when weight ≥ threshold **and** parents certified |
| `certified_tips` | Certified batches with no certified children |
| `support_weight` | Sum of UNL weights that signed |

### 3.5 Engine (`engine.py`)

| Function | Behavior |
|----------|----------|
| `submit_tx` | Mempool + tx index |
| `create_batch` | Pack executable txs vs ordered state; parents = certified tips |
| `effective_batch_interval` | Adaptive cadence from mempool depth |
| `on_remote_batch` / `on_remote_sig` / `on_remote_cert` | Gossip handlers |
| `advance_ordering` | Commit cert order + execute |
| `tick` | Periodic batch + order |

### 3.6 Ordering — Option B (`ordering.py`)

| Function | Behavior |
|----------|----------|
| `_causal_round` | Pure depth from parent links |
| `_compute_total_order` | Topo order; tie-break (round, timestamp, batch_hash) |
| `try_commit` | Recompute pure total order over certified set |
| hard finality | Ordered index + H later certs |

### 3.7 Execution (`execution.py`)

| Function | Behavior |
|----------|----------|
| `apply_new_ordered` | Replay txs from ordered certs; first valid wins |
| `refresh_soft_final` | Soft = any certified batch inclusion |

### 3.8 Node / network / sim

| Module | Role |
|--------|------|
| `network.py` | TCP JSON-line gossip |
| `node.py` | Validator process + HTTP RPC |
| `sim.py` | In-process multi-validator simulator |

### 3.9 RPC surface (regtest)

| Method | Returns |
|--------|---------|
| `ping` | liveness |
| `server_info` | DA / ordering / execution metrics |
| `submit` | inject payment |
| `canonical` | ordered batches + executed txs |
| `dag_status` | engine metrics |

### 3.10 Finality

| Level | Definition | Typical use |
|-------|------------|-------------|
| **Soft** | Tx in a **certified** batch (≥80% UNL signed batch) | Ordinary payments |
| **Hard** | Cert ordered + **H** further ordered certs | Bridges, high value |

---

## 4. Viability test results (this run)

Suite: `scripts/viability_suite.py` → `docs/VIABILITY_REPORT.json`

| Case | Result | Notes |
|------|--------|-------|
| Validators 3–11 | PASS | Agreement at 200 txs |
| Agreement 5×200 / 5×800 | PASS | Full exec + order match |
| Sim TPS 100–3000 txs | PASS | Peak **~4.3k TPS** in-process (not network-bound) |
| 1/5 silent | PASS | Still certifies (80%) |
| 2/5 silent | PASS | No certify (correct) |
| Double-spend | PASS | One winner, all agree |
| Signature amortization model | PASS | Order-path bytes/tx fall with batch size |
| Fat vs thin batches | PASS | Fatter batches → more txs/cert |
| Live 5-node regtest | PASS | Multi-node agreement under load |
| Falcon binary probe | PASS | `xrpld` present; live Falcon stress not run (isolated) |

**Verdict: CONTINUE** — medium-high confidence for **research spinoff / layer-upgrade**, not production swap.

### Measured numbers (prototype, single machine)

| Metric | Value | Caveat |
|--------|-------|--------|
| Peak in-process sim TPS | ~4300 | No real crypto, no WAN |
| 3000-tx sim TPS | ~1300 | CPU/sim overhead grows |
| Live regtest wall TPS | tens–low hundreds | Python prototype + localhost |
| Falcon published (ROADMAP) | 850k+ payments / 71h, ~3s converge | Classic RPCA testnet |

Prototype TPS is **not** comparable 1:1 to Falcon production C++ — it only shows the **algorithmic path** scales under agreement tests.

---

## 5. Pros and cons

### Pros

1. **Separates heavy data from ordering** — large Falcon-512 sigs travel on the DA path once; consensus orders small certificates.
2. **Parallel batch production** — no single global “open ledger” queue for ingestion.
3. **UNL remains the only agreement source** — same federated trust model as Falcon.
4. **Soft finality can be very fast** — certification without waiting for a full classic close.
5. **Hard finality tunable** via H.
6. **Natural fit for PQ signatures** — amortization improves as batches get fatter.
7. **Spinoff-friendly** — can evolve without risking production Falcon.
8. **Layer-upgrade path** — could later feed ordered certs into Falcon execution.

### Cons

1. **Two-layer complexity** — more code, more failure modes than single RPCA.
2. **Prototype crypto** — real Falcon signatures not yet on batch/cert path.
3. **Option B ordering is DAG-deterministic**, not classic RPCA rounds — different ops/security analysis.
4. **Soft final ≠ settlement final** — apps must understand soft vs hard.
5. **Python PoC** — not production performance; C++/integration still required for Falcon-class TPS.
6. **Duplicate txs in parallel batches** — harmless but waste bandwidth unless workers shard.
7. **No full Byzantine suite yet** — silent nodes tested; equivocation/DoS not.
8. **Not integrated** with Falcon fees, amendments, rewards, bridges.

---

## 6. Comparison to production Falcon

| Dimension | **Falcon (production)** | **Falcon DAG (this project)** |
|-----------|-------------------------|--------------------------------|
| Repo | `qXRP` / FalconLedger | `falcon-dag` |
| Consensus | Classic **single-ledger RPCA** | **Narwhal DA** + federated cert order |
| Ledger model | One open ledger → close → validate | Many batches → certificates → order |
| Signature scheme (intent) | **Falcon-512** always-on | Same **intent**; amortize large sigs via batches |
| Soft finality | Validated ledger (~seconds) | Certified batch (can be sub-second) |
| Hard finality | Fully validated chain depth | Ordered cert + H |
| Throughput bottleneck | Ledger size × close rate | DA bandwidth + cert rate + exec |
| Ops maturity | Testnet, rehearsal, freeze path | Research regtest only |
| Risk to mainnet | Production chain | **Zero** (isolated) |
| Best use now | Real value, rewards, bridges | R&D, benchmarks, future layer |

### Goal alignment: faster/more TPS **with** large signatures

| Approach | Helps large Falcon sigs? |
|----------|---------------------------|
| Classic RPCA only | Every close still moves full tx+sig set in consensus critical path |
| **Falcon DAG** | DA moves fat payloads; **ordering path cost per tx falls** as batch size grows (model + fat-batch test) |
| Smaller sigs (ed25519) | Faster but **abandons PQ goal** — rejected for Falcon |

---

## 7. Adaptive / variable ledger (batch) timing — is your thinking right?

### Your idea (paraphrased)

> Variable timing that changes with load; at high load **slow down** so throughput rises.

### Verdict: **Yes — that is the right direction**, with precise wording:

| Load | Cadence | Interval | Effect |
|------|---------|----------|--------|
| **Low** | **Faster** batches | **Shorter** interval | Low latency soft-final; batches half-empty OK |
| **High** | **Slower** batches | **Longer** interval | **Fuller** batches → more txs per cert → **higher TPS**, better sig amortization |

So:

- Interval **increases** under load (time between batches goes up).  
- Frequency **decreases** under load.  
- Throughput **increases** because each certificate “buys” more transactions (and more Falcon sig bytes already paid for on the DA path).

**Not** the right mental model: “close faster under load to get more TPS” — that often **hurts** when signatures are huge, because you pay certificate/round overhead more often with thinner batches.

### Implemented knob

```text
adaptive_interval = true
mempool depth low  → batch_interval_min_s  (fast)
mempool depth high → batch_interval_max_s  (slow / fat)
```

Evidence: viability case `adaptive_fat_vs_thin_batches` — max 256 txs/batch packs more **txs per certificate** than max 16.

### Interaction with Falcon-512

Larger signatures make **fat batches under load** more valuable:

- Ordering cost ≈ O(certificates × UNL sigs), not O(transactions × Falcon sigs).
- High load + longer interval → larger n → smaller **order-path bytes per tx**.

---

## 8. Spinoff vs layer upgrade

| Path | When to choose | What you keep |
|------|----------------|---------------|
| **Spinoff chain** | Want independent product/experiment | Own network_id, own economics optional |
| **Layer upgrade under Falcon** | Want Falcon UX + higher capacity later | Falcon-512 wallets, accounts, fees; swap/open DA+order under the hood |
| **Keep classic Falcon only** | Mainnet safety first | Current RPCA; no DAG risk |

**Recommended stance now:** continue Falcon DAG as **spinoff research** with a documented **layer-upgrade design target**. Do **not** replace production RPCA until C++/crypto/adversarial bar is met.

---

## 9. Continue? — decision checklist

| Gate | Status |
|------|--------|
| Deterministic multi-validator agreement | **Met** (sim + live regtest) |
| UNL threshold behavior (4/5 vs 3/5) | **Met** |
| Conflict resolution | **Met** |
| PQ-sig amortization story | **Met** (model + fat batch) |
| Adaptive load timing story | **Met** (direction proven) |
| Real Falcon crypto on certs | **Not met** |
| Head-to-head TPS vs Falcon C++ regtest | **Not met** (baseline docs only) |
| Byzantine + soak + restart | **Not met** |
| Production integration design | **Not met** |

**Decision: viable to CONTINUE** as research spinoff / future layer — **not** viable to replace Falcon.

---

## 10. Next tests (if continuing)

1. **Falcon C++ regtest TPS baseline** on dedicated ports (isolated from mainnet machines) — `load-test.py` / existing `falcon-regtest.sh`.
2. **Wire real Falcon-512** batch and certificate signatures; measure CPU cost.
3. **WAN latency model** (50–200 ms) in sim; re-run TPS and soft-final latency.
4. **Adversarial:** equivocation, invalid batches, cert withholding.
5. **Adaptive interval** closed-loop metrics: latency vs TPS Pareto curve.
6. **Layer-upgrade sketch:** how ordered certs map to Falcon ledger entries without breaking amendments.

---

## 11. How to re-run

```bash
cd /home/scott/falcon-dag
source .venv/bin/activate
pytest tests/ -v
./regtest/start-regtest.sh   # if not already up
python3 scripts/viability_suite.py
# → docs/VIABILITY_REPORT.json
```

---

## 12. One-line summary

**Falcon DAG is a viable research spinoff / future layer to increase throughput under large Falcon signatures by certifying fat batches and ordering tiny certificates; production Falcon RPCA stays as-is; under high load, slow the batch cadence so batches get fatter — that is the correct adaptive-timing idea.**
