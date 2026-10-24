# Falcon DAG as a Layer Upgrade under Falcon

**Status:** design sketch only — production Falcon (`qXRP`) is not modified.  
**Positioning:** optional future path; Falcon DAG remains a research spinoff until gates clear.

---

## 1. Goal

Keep Falcon’s:

- Falcon-512 transaction signatures  
- Account model, fees, rewards, bridges  
- UNL federated trust  

…while replacing (or sitting under) the **single-ledger close bottleneck** with:

1. **Narwhal-style DA** — fat batches of already-signed Falcon txs  
2. **Federated ordering** — total order of certificates only  
3. **Execution** — same deterministic apply rules Falcon already uses  

---

## 2. Mapping layers

| Falcon today | Layer-upgrade mapping |
|--------------|----------------------|
| Open ledger + consensus transaction set | **Batches** in DA DAG |
| Validation votes on ledger hash | **Signatures / certs** on batch hash (≥ UNL threshold) |
| Accepted ledger chain | **Ordered certificates** → sequential apply |
| Soft final (validated ledger) | Soft = certified batch; hard = ordered + H |
| P2P tx gossip | Unchanged / enhanced worker gossip |
| Falcon-512 tx sigs | **Unchanged** — still on every payment |

Consensus-critical change is **what** UNL votes on (batch/cert vs full ledger), not how users sign.

---

## 3. Suggested integration phases

### Phase 0 — Spinoff (current)

- Isolated `falcon-dag` prototype  
- HMAC-dev certs (→ Falcon-512 certs later)  
- No `xrpld` code changes  

### Phase 1 — Shadow mode (future)

- Falcon validators run a **side process** packing txs into batches  
- Certificates computed but **not** used for ledger close  
- Metrics only: batch size, cert latency, overlap with RPCA ledgers  

### Phase 2 — Optional amendment (future)

- Amendment e.g. `NarwhalDA`  
- When enabled: ledger close consumes **ordered cert tips** instead of classic proposal set  
- Disabled = classic RPCA (safe rollback)  

### Phase 3 — Default path (far future)

- Only after: real Falcon crypto on certs, soak, adversarial review, bridge policy on hard finality  

---

## 4. Falcon-512 and throughput

| Path | Bytes that hit “consensus critical” path |
|------|------------------------------------------|
| Classic RPCA | ~all txs × (body + **~666B Falcon sig**) each close |
| DA + order | DA: same once; **order:** cert ≈ batch_hash + UNL × Falcon-sig |

Under high load, **lengthen batch interval** so batches are fatter → order-path cost per tx drops.  
User txs remain Falcon-512; PQ security goal preserved.

---

## 5. Non-goals

- Replacing Falcon account semantics  
- Weakening UNL threshold  
- Soft-final as bridge settlement (bridges should use **hard** finality H≥8–16)  

---

## 6. Exit criteria before any Falcon code touch

1. Falcon-512 (or equivalent PQ) on batch attestations  
2. Head-to-head TPS/latency vs classic Falcon regtest  
3. Adversarial + partition + soak reports  
4. Formal amendment + dual-run shadow period  
5. Explicit ops runbook for rollback to RPCA  

Until then: **develop only in `falcon-dag`.**
