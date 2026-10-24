# Narwhal-Style Mempool DAG + Federated Ordering

*(Design Specification for Falcon DAG — research fork of Falcon)*

> Production Falcon Ledger is **not** modified by this project.

**PR1 contract:** [ARCHITECTURE_CONTRACT.md](ARCHITECTURE_CONTRACT.md) · **roadmap:** [IMPLEMENTATION_GUIDE.md](IMPLEMENTATION_GUIDE.md)

---

### 1. Goal

Separate data dissemination from consensus ordering so that:

- Transaction data can be spread and certified in parallel at high rate
- The consensus layer only orders small certificates
- Effective throughput rises significantly
- Individual confirmation latency can stay low

This is a Narwhal-style architecture adapted to a federated UNL (RPCA-style) network.

---

### 2. Architecture Overview

Two distinct layers:

**A. Data Availability Layer (Narwhal-style DAG)**  
- Nodes reliably broadcast batches of transactions
- Batches form a DAG via parent references
- A batch becomes **certified** once enough UNL validators have signed it
- Certified batches are proof that the transaction data is available

**B. Ordering Layer (Bullshark + expand)**  
- **Bullshark (Phase 43):** even-round leaders; commit when round r+1 has ≥ f+1 support (DAG-only)
- **Tip-BFT (Phase 42, secondary):** optional tip propose/vote/commit wire
- Under each commit: deterministic expand of the cert sub-DAG  
  (parents-before-children; tie-break by causal round, timestamp, batch hash)
- Final ledger state is obtained by executing transactions in that order

---

### 3. Data Structures

**Batch** — hash, txs, parent batch hashes, creator, timestamp, partial signatures  

**Certificate** — batch hash, ≥ UNL_THRESHOLD signers, parents, metadata  

**Mempool DAG** — batches, certificates, certified tips  

**Ordering State** — ordered certificates, executed txs, soft/hard finality sets  

---

### 4. Data Availability Layer

1. Validator creates a batch from mempool (executable vs ordered state; fee priority)
2. Batch references recent **certified** tips as parents
3. Batch is gossiped
4. Validators that receive a valid batch sign it
5. At ≥ UNL_THRESHOLD (80%) signatures **and** parents certified → **certificate**
6. Certificate is gossiped
7. Certified batches form a DAG

---

### 5. Ordering Layer (Bullshark)

- Even-round **anchors** commit with f+1 next-round support (signers + creators)
- Expand under the commit: topo of cert ancestors; tie-break `(causal_round, timestamp, batch_hash)`
- Expand certificates → transactions → execute sequentially
- First valid tx wins conflicts
- Details: [BULLSHARK.md](BULLSHARK.md), [TIP_BFT.md](TIP_BFT.md)

---

### 6–8. Lifecycle, Conflicts, Finality

1. Tx enters mempool → batch → certified (soft final) → ordered → executed  
2. Hard final: certificate ordered + **H** further ordered certificates  
3. Soft final: in a certified batch  

---

### 9. Parameters

| Parameter | Value |
|-----------|-------|
| UNL_THRESHOLD | 80% |
| MAX_BATCH_PARENTS | 6 |
| TARGET_BATCH_INTERVAL | ~400 ms |
| MAX_BATCH_TXS | 128 |
| H | 6 |
| network_id | 9002 |

---

### 10–12. Properties & Implementation

- Data moves at network speed; consensus orders certificates only  
- Agreement remains fully UNL / federated  
- Modules: certificate_dag, engine, ordering, execution, node, sim, regtest  

**Supersedes:** Ghost Falcon (GHOSTDAG multi-ledger) — abandoned.
