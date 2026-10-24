# Comparison: Falcon Ledger (C++ RPCA) vs Falcon DAG

**Date:** 2026-08-03  
**Audience:** engineering / product decision-making  
**Status:** Falcon Ledger = production path · Falcon DAG = research spinoff / possible future layer  

> **Falcon DAG does not replace production Falcon.**  
> It is a separate repo (`beartec-jpg/falcon-dag`) that explores higher throughput under large post-quantum signatures while keeping **UNL federated agreement**.

Related docs:

- [HEAD_TO_HEAD_TPS.md](./HEAD_TO_HEAD_TPS.md) — measured numbers  
- [LAYER_UPGRADE.md](./LAYER_UPGRADE.md) — how DAG could sit under Falcon later  
- [DESIGN.md](./DESIGN.md) — Narwhal-style design  
- [SPEC_AND_COMPARISON.md](./SPEC_AND_COMPARISON.md) — DAG function inventory  

---

## 1. One-sentence difference

| System | One sentence |
|--------|----------------|
| **Falcon Ledger** | Quantum-resistant XRPL-style chain: **one open ledger**, **RPCA** closes it, **Falcon-512** on every payment/validator identity. |
| **Falcon DAG** | Research prototype: **Narwhal-style batches** get **UNL-certified**, then a **lightweight total order of certificates** drives execution. |

---

## 2. Side-by-side overview

| Dimension | **Falcon Ledger (production)** | **Falcon DAG (research)** |
|-----------|--------------------------------|---------------------------|
| **Repo** | `qXRP` / FalconLedger | `falcon-dag` |
| **Language** | C++ (`xrpld`) | Python prototype |
| **Network ID (local)** | 1001 (testnet-style regtest) | 9002 |
| **Consensus** | Classic **single-ledger RPCA** | **DA DAG** + federated **cert ordering** |
| **What UNL votes on** | Next ledger / validation | Batch **availability** (then order certs) |
| **User tx crypto** | **Falcon-512** (standard) | Prototype payments (can carry Falcon-sized idea); certs are Falcon-512 |
| **Validator identity** | Falcon-512 | Falcon-512 cert keys (HMAC only for fast unit tests) |
| **Ledger model** | Linear chain of closed ledgers | Certificate DAG → deterministic tx total order |
| **Soft finality** | Validated ledger (~seconds) | Certified batch (can be sub-second) |
| **Hard finality** | Validated depth | Ordered cert + **H** more ordered certs |
| **Maturity** | Testnet / rehearsal / freeze path | Viability suite + regtest only |
| **Risk to mainnet Falcon** | Is the mainnet path | **None** (isolated) |

---

## 3. Architecture

### 3.1 Falcon Ledger (RPCA)

```
  Wallet (Falcon-512 signed Payment)
              │
              ▼
        Global mempool / open ledger
              │
              ▼
   RPCA rounds → agree transaction set
              │
              ▼
   Close ledger → validations → validated chain
              │
              ▼
         Apply txs → state
```

- One **open ledger** at a time under normal operation.  
- Capacity ≈ **(txs per ledger) × (ledgers per second)**.  
- Full transaction payloads (including large PQ sigs) sit in the consensus-critical path every close.

### 3.2 Falcon DAG (Narwhal-style)

```
  Wallet / client txs
              │
              ▼
   ┌─────────────────────────────┐
   │ A. Data availability (DAG)  │
   │ batches → UNL Falcon-512    │
   │ signatures → certificates   │
   └─────────────┬───────────────┘
                 │ certificates only
                 ▼
   ┌─────────────────────────────┐
   │ B. Ordering (federated)     │
   │ total order of certs        │
   │ → expand txs → execute      │
   └─────────────────────────────┘
```

- **Many batches** can be built and certified in parallel.  
- Ordering layer deals with **small certificates**, not full floods of fat txs each round.  
- Same UNL threshold idea (**≥ 80%**) for strong acceptance of availability.

---

## 4. Cryptography and post-quantum story

| Topic | Falcon Ledger | Falcon DAG |
|-------|---------------|------------|
| Payment signatures | Falcon-512 (~650–670 B class) | Prototype; design assumes large PQ user sigs |
| Consensus / attestation | Validations + proposals (Falcon identities) | **Falcon-512 detached certs** on batch hashes |
| Public key size | ~897 B Falcon | ~897 B Falcon (UNL public keys) |
| Goal: keep PQ | **Yes — core product** | **Yes — cert amortization under PQ cost** |

**Why DAG is interesting under Falcon-512**

- Classic RPCA: every close still **argues about sets of fat transactions**.  
- DAG: DA path carries fat data once; **order path cost ≈ cert size**, which grows with **number of UNL signers**, not number of txs in the batch.  

Example from local measurements:

- Mean Falcon-512 **cert signature** ≈ **655 B**  
- **4-of-5** certificate ≈ **~2.6 KB** total  
- At 128 txs/batch → order-path **~20 B/tx** for cert material (vs hundreds of bytes per tx if every PQ sig rode a thin consensus object)

---

## 5. Performance (local head-to-head, 2026-08-03)

**Caveats (read first)**

- Localhost only.  
- Falcon DAG is **Python**; Falcon Ledger is **C++**.  
- Do **not** market “DAG faster than Falcon” from sim numbers.  
- Numbers are for **architecture and cost**, not capacity planning for mainnet.

### Measured on the same host

| Metric | Falcon Ledger (C++ RPCA) | Falcon DAG |
|--------|--------------------------|------------|
| Validators | 5 (`xrpld` proposing) | 5 Python nodes / sim |
| Idle close cadence | **~3.0 s / ledger** | Batch interval ~0.15–1.2 s (adaptive optional) |
| Converge time | **~2.0 s** last close | N/A (cert + order) |
| Falcon-512 payment flood | **~96 TPS** sequential submit | N/A (simple prototype txs) |
| Flood settle (approx) | **~22 TPS** incl. wait | Live settle **~10–180 TPS** (state-dependent) |
| Algorithmic sim TPS | N/A | **~500–900 TPS** with Falcon-512 **certs** |
| Theoretical RPCA fill | ~**333 TPS** if 1000 txs @ 3 s close | Scales with batch fill × cert rate (design) |

Sources: `docs/HEAD_TO_HEAD_TPS.md`, `scripts/falcon_cpp_flood.py`, `scripts/head_to_head_tps.py`.

### Crypto micro-cost (DAG cert backend)

| Backend | Sign (mean) | Sig size |
|---------|-------------|----------|
| HMAC-dev (unit tests only) | ~0.005 ms | 32 B |
| Falcon-512 | ~0.39 ms | ~655 B |

→ Falcon-512 cert signing is ~**75×** slower than HMAC in the prototype — which is why **unit tests** use HMAC and **regtest** uses Falcon-512.

---

## 6. Finality model

| Level | Falcon Ledger | Falcon DAG |
|-------|---------------|------------|
| **Soft** | Ledger validated by UNL | Tx included in a **certified** batch (≥80% UNL signed batch) |
| **Hard** | Further validated ledgers / operational policy | Certificate **ordered** + **H** more ordered certs (default H=6) |
| Bridges / high value | Prefer hard / deep validation | Prefer **hard** (H = 8–16 recommended in design) |

---

## 7. Ops, maturity, and risk

| Topic | Falcon Ledger | Falcon DAG |
|-------|---------------|------------|
| Production readiness | High (relative) | Research only |
| Tooling | Fleet, faucet, bridges, rewards, dashboards | Local regtest, soak, viability suite |
| Crash recovery | Full node DB (NuDB etc.) | JSON snapshots (prototype) |
| DoS controls | Mature fee / queue machinery | Caps on uncertified batches, per-creator, mempool |
| Equivocation | Consensus rules | Creator double-propose on same parents detected |
| Mainnet impact if research continues | Only via future amendment / dual-run | Zero while isolated |

---

## 8. Pros and cons

### Falcon Ledger (C++ RPCA)

**Pros**

- Production-shaped, battle-tested consensus model (XRPL lineage).  
- Falcon-512 already integrated for wallets, validators, P2P identity.  
- Full economic stack (treasury, rewards, lending, bridges, etc.).  
- Ops and security review path already advanced.

**Cons**

- Single open-ledger pipeline can become a **global queue** under load.  
- Shortening closes globally has tradeoffs; fat PQ sigs raise per-tx consensus cost.  
- Throughput improvements are mostly **bigger ledgers / faster closes / engineering**, not a new data plane.

### Falcon DAG

**Pros**

- Separates **data availability** from **ordering** (Narwhal-style).  
- Parallel batch production; no single full-ledger queue for ingestion.  
- Certificates amortize UNL attestation under **large Falcon-512** material.  
- Soft finality can be fast; hard finality tunable.  
- Safe to develop: **does not touch** production Falcon code.  
- Clear spinoff **or** later **layer-upgrade** path.

**Cons**

- Python prototype — not production TPS or security bar.  
- Two-layer system: more complexity, more failure modes.  
- Soft ≠ settlement final (apps must understand levels).  
- Not integrated with Falcon fees, rewards, bridges, amendments.  
- Needs real dual-run, adversarial soak, and likely a C++ port before any Falcon integration.

---

## 9. When to use which

| Scenario | Prefer |
|----------|--------|
| Mainnet / real value / user wallets | **Falcon Ledger** |
| Validator rewards, bridges, DEX as shipped | **Falcon Ledger** |
| Research on PQ + higher throughput | **Falcon DAG** |
| Experiment with adaptive batch timing under load | **Falcon DAG** |
| “Ship a faster chain tomorrow” | **Falcon Ledger** (optimize RPCA path) |
| “Prove Narwhal-style DA under our UNL” | **Falcon DAG** |

---

## 10. Relationship options (not mutually exclusive)

```
                    ┌──────────────────────┐
                    │  Falcon Ledger       │
                    │  (C++ RPCA product)  │
                    └──────────▲───────────┘
                               │ optional future
                               │ amendment / dual-run
                    ┌──────────┴───────────┐
                    │  Falcon DAG layer    │
                    │  DA + cert order     │
                    └──────────────────────┘

         OR standalone spinoff chain (own network_id)
```

| Path | Description |
|------|-------------|
| **A. Keep separate** | Falcon stays RPCA; DAG remains research forever or becomes its own chain |
| **B. Layer upgrade** | DAG provides certified batches; Falcon execution/accounts stay; RPCA closes on certs not raw floods |
| **C. Replace RPCA** | **Not recommended** without years of parity work |

Current recommendation: **A + explore B in design only** (`LAYER_UPGRADE.md`).

---

## 11. Feature checklist

| Capability | Falcon Ledger | Falcon DAG (now) |
|------------|---------------|------------------|
| Multi-validator UNL consensus | Yes | Yes (cert + order) |
| Falcon-512 user txs | Yes | Prototype path |
| Falcon-512 consensus artifacts | Yes (identities/validations) | Yes (batch certs) |
| Parallel open work | Limited (one ledger) | Yes (batches) |
| Adaptive load cadence | Fee/queue engine | Adaptive batch interval |
| Snapshots / restart | Full DB | JSON snapshot |
| Soak / viability suite | Existing fleet tools | Yes (`viability_suite`, `soak_test`) |
| Bridges / AMM / rewards | Yes | No |
| Production ops | Yes | No |

---

## 12. Bottom line

| Question | Answer |
|----------|--------|
| Are they the same product? | **No** |
| Which is production Falcon? | **Falcon Ledger (C++ RPCA)** |
| What is Falcon DAG for? | **Research**: higher throughput under large PQ signatures via DA + cert ordering |
| Did local TPS show DAG should replace Falcon? | **No** — different maturity and language |
| Should development continue on DAG? | **Yes, as spinoff / future layer research** |
| Shared north star | **Post-quantum security + federated UNL + better capacity** |

**Short verdict:**  
**Falcon Ledger** is the chain you run and ship.  
**Falcon DAG** is the architecture lab for “keep Falcon-512, stop making every fat byte fight through one linear ledger close.”  
Use them as **complementary**, not as competitors for today’s mainnet.
