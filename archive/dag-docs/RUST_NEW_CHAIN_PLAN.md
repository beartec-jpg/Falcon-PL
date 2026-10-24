# Falcon DAG — Full Plan: New Chain Rebuild in Rust

**Status:** Planning baseline for a **completely new chain**  
**Language recommendation:** **Rust**  
**Python PoC:** executable spec + regtest oracle (keep; do not delete)  
**Production Falcon Ledger (`qXRP`):** **not modified** — separate product  

**Date:** 2026-08-03  

---

## 0. Decision summary

| Decision | Choice |
|----------|--------|
| Product | **New chain** (spinoff), not a quick ops layer on Falcon |
| Language | **Rust** (performance + memory safety + long-term systems fit) |
| Architecture family | Narwhal-style **DA DAG** + **federated UNL ordering** |
| PQ crypto | **Falcon-512** (user txs + batch attestations) |
| Trust model | **UNL** (≥80% weight), not PoW / not pure stake-BFT (stake can come later) |
| Python role | Spec, differential tests, research harness |
| When to start Rust | **Now** — PoC is enough to freeze v1 schematics |

---

## 1. What we are building (schematic)

### 1.1 System context

```
┌──────────────┐     JSON-RPC / gRPC      ┌─────────────────────────────┐
│  Wallets /   │ ───────────────────────► │  falcon-dagd (Rust node)    │
│  explorers   │ ◄─────────────────────── │                             │
└──────────────┘     queries / submit     │  ┌─────────┐  ┌──────────┐  │
                                          │  │ Mempool │  │ Execution│  │
┌──────────────┐     P2P (libp2p/TCP)     │  └────┬────┘  └────▲─────┘  │
│ Other        │ ◄──────────────────────► │       │            │        │
│ falcon-dagd  │   batches / sigs / certs │  ┌────▼────┐  ┌───┴─────┐  │
└──────────────┘                          │  │   DA    │  │ Ordering│  │
                                          │  │  (DAG)  │─►│ (certs) │  │
                                          │  └─────────┘  └─────────┘  │
                                          │  ┌──────────────────────┐  │
                                          │  │ Store (hot + cold)   │  │
                                          │  └──────────────────────┘  │
                                          └─────────────────────────────┘
```

### 1.2 Consensus data plane (core)

```
  Tx (Falcon-512 signed)
       │
       ▼
  Global mempool ──► Batch (parents = certified tips)
       │                    │
       │                    ▼ gossip
       │              PartialSignature (Falcon-512 on batch hash)
       │                    │
       │                    ▼ ≥ UNL 80% + parents certified
       │              Certificate
       │                    │
       └────────────────────┼────────────────────┐
                            ▼                    │
                     Ordering (deterministic     │
                     topo of cert DAG)           │
                            │                    │
                            ▼                    │
                     Execute txs in order        │
                            │                    │
                            ▼                    │
                     Account state + finality    │
                     (soft=certified, hard=H)    │
```

### 1.3 Layers (crates / modules)

| Layer | Responsibility | Must not do |
|-------|----------------|-------------|
| **P2P** | Gossip inventory, batch/sig/cert bodies | Consensus decisions |
| **Mempool** | Fee order, conflicts, anti-starvation | Final order of history |
| **DA** | Batch validity, UNL sigs, certify, tips | Apply balances |
| **Ordering** | Pure total order of certificates | Network I/O |
| **Execution** | Apply txs, first-valid-wins | Gossip |
| **Finality** | Soft/hard flags, H depth | Reorder after hard |
| **Store** | Persist + prune + checkpoints | Invent consensus rules |
| **RPC** | Submit/query for wallets | Trust unauthenticated peers for state |

**Invariant:** Ordering + execution = **pure functions** of (certified set + votes + genesis). Same inputs → same outputs on all honest nodes.

---

## 2. Network model

### 2.1 Identity and trust

| Item | Spec |
|------|------|
| Node identity | Falcon-512 keypair (or hybrid: identity key + cert key) |
| UNL | Static list of `(node_id, public_key, weight)` for bootstrap |
| Threshold | `unl_threshold = 0.80` (weight sum of valid votes) |
| Quorum meaning | **Availability** for batches; order is deterministic given certs |
| Future | Optional open-UNL / stake weights (out of v1 scope) |

### 2.2 Topology

| Item | v1 choice |
|------|-----------|
| Transport | **libp2p** (Noise + Yamux) *or* length-prefixed TCP (match PoC first) |
| Discovery | Static `peers` list for private testnet; mDNS optional for local |
| Mesh | Full mesh OK for ≤15 validators; later gossipsub |
| Message types | `Tx`, `Batch`, `PartialSig`, `Certificate`, `SyncRequest/Response` |
| Auth | Sign critical messages; reject unknown UNL for consensus votes |

### 2.3 Network IDs / environments

| Network | network_id | Purpose |
|---------|------------|---------|
| Dev / unit | 9000 | Ephemeral |
| Local testnet | **9002** | Docker / bare metal multi-node (matches PoC) |
| Public testnet | **9003** | Packaged profile (`deploy/testnet-9003/`) |
| Mainnet | TBD | Far future |

**Never** reuse Falcon Ledger mainnet/testnet IDs (1001/1026 family).

### 2.4 Ports (local testnet default)

| Service | Port pattern |
|---------|----------------|
| RPC | 18001 + i |
| P2P | 19001 + i |
| Metrics (Prometheus) | 9101 + i |

---

## 3. State model

### 3.1 Consensus objects (DA)

```text
Batch {
  batch_hash: Hash256,          // commitment of content
  parents: Vec<Hash256>,        // 1..=MAX_BATCH_PARENTS certified tips
  creator: NodeId,
  timestamp: u64,               // millis, monotonic enough for tie-break
  tx_ids: Vec<Hash256>,
  // bodies stored separately or inline for small testnet
  txs: Vec<Transaction>,
  signatures: Map<NodeId, FalconSig>,
}

Certificate {
  cert_id: Hash256,
  batch_hash: Hash256,
  signers: Vec<NodeId>,
  parents: Vec<Hash256>,
  creator: NodeId,
  timestamp: u64,
  round: u64,                   // 1 + max(parent rounds)
}
```

### 3.2 Transaction (v1 simple payments; extensible)

```text
Transaction {
  tx_id: Hash256,
  account: AccountId,
  sequence: u64,                // per-account nonce
  destination: AccountId,
  amount: u64,
  fee: u64,
  // v1: optional; later required Falcon-512 over canonical bytes
  signature: Option<FalconSig>,
  public_key: Option<FalconPk>,
}
```

### 3.3 Execution state (the “ledger state”)

```text
AccountState {
  balances: Map<AccountId, u64>,
  sequences: Map<AccountId, u64>,   // next expected sequence
}

// Derived, not free-form:
// after applying ordered txs from genesis (or checkpoint)
```

**Important:**  
- **DAG / batches / certs** = how history is agreed.  
- **AccountState** = compact state.  
- You do **not** need to keep every batch body in RAM forever.

### 3.4 Ordering state

```text
OrderingState {
  ordered: Vec<BatchHash>,      // total order including genesis
  hard_final: Set<BatchHash>,   // ordered with H successors
  soft_final_txs: Set<TxHash>,  // in any certified batch
  executed_txs: Vec<TxHash>,    // successfully applied
}
```

### 3.5 Mempool state

```text
Mempool {
  txs: Map<TxHash, Transaction>,
  by_account_seq: Map<(AccountId, Seq), Set<TxHash>>,
  done: Set<TxHash>,            // soft-final or executed competitors dropped
}
```

Priority (deterministic):  
`fee_density + age_boost → -first_seen → tx_hash`

### 3.6 Storage layout (on disk)

```text
data/
  identity/           # node Falcon keys (0600)
  unl/                # UNL public keys + weights
  hot/
    state.db          # AccountState + meta (redb/rocksdb)
    tips.db           # certified tips, recent certs
    mempool.db        # optional persist
  cold/
    batches/          # batch bodies by hash (content-addressed)
    certs/            # certificates
  checkpoints/
    ckpt_N.ron        # state root + ordered height + hash
  wal/                # write-ahead for crash safety
```

### 3.7 Memory model (anti-buildup) — **v1 requirements**

| Rule | Spec |
|------|------|
| Uncertified TTL | Drop uncertified batches older than `uncertified_ttl_s` |
| Uncertified caps | Global + per-creator caps (DoS) |
| Sig rate limit | Per-validator sliding window |
| After hard final + checkpoint | **Drop batch bodies** from hot path; keep hash + cert header |
| Checkpoint interval | Every N hard-final certs or T seconds |
| Archive nodes | Optional full cold history |
| Hot RAM | tips + recent window + AccountState + mempool |

**Throughput under load ≠ unbounded RAM** if these are enforced.

### 3.8 How volume is absorbed (clarify)

| Mechanism | What happens under high load |
|-----------|------------------------------|
| Parallel DA | Multiple batches certified concurrently |
| Adaptive interval | **Longer** batch interval → **fatter** batches → higher TPS |
| Soft final | Can still be fast (cert time) |
| Hard final | Needs H ordered certs (tunable) |

So: not only “same close speed, more txs.” Often **slower cadence, more txs per cert**, plus parallel certs.

---

## 4. Settings / configuration surface

### 4.1 Consensus-critical (must match all honest nodes)

| Setting | Default | Notes |
|---------|---------|-------|
| `network_id` | 9002 | Local testnet |
| `unl_threshold` | 0.80 | Weight fraction |
| `max_batch_parents` | 6 | DAG width |
| `max_batch_txs` | 128 | Cap per batch |
| `hard_finality_h` | 6 | Hard depth |
| `worker_sharding` | true | Narwhal workers |
| `shard_orphan_age_ms` | 2000 | Steal orphaned shards |
| `require_sig_verify` | true | Always on for real nets |
| `crypto_scheme` | `falcon-512` | No HMAC on real testnet |
| Genesis balances / accounts | fixed file | Shared genesis |

### 4.2 Performance / adaptive (same defaults recommended on all nodes)

| Setting | Default |
|---------|---------|
| `target_batch_interval_ms` | 400 |
| `adaptive_interval` | true |
| `batch_interval_min_ms` | 150 |
| `batch_interval_max_ms` | 1200 |
| `adaptive_mempool_low` | 16 |
| `adaptive_mempool_high` | 512 |
| `starvation_boost_per_s` | 0.01 |

### 4.3 DoS / local policy

| Setting | Default |
|---------|---------|
| `max_known_batches` | 50_000 |
| `max_uncertified_batches` | 2_000 |
| `max_uncertified_per_creator` | 64 |
| `max_mempool_txs` | 50_000 |
| `max_sigs_per_validator_per_s` | 50 |
| `uncertified_ttl_s` | 120 |
| `max_hard_final_history` | 10_000 |

### 4.4 Node-local only

| Setting | Example |
|---------|---------|
| `data_dir` | `./data/val1` |
| `rpc_bind` | `127.0.0.1:18001` |
| `p2p_bind` | `0.0.0.0:19001` |
| `peers` | list of multiaddrs |
| `identity_key_path` | `./keys/node.sk` |
| `unl_path` | `./unl.json` |
| `log_level` | `info` |
| `metrics_bind` | `127.0.0.1:9101` |

### 4.5 Config file format (Rust)

- **TOML** primary (`falcon-dag.toml`)  
- Optional JSON for UNL/genesis (easy generation)  
- Version field `config_version = 1`

---

## 5. Protocol algorithms (freeze as v1)

Port **exactly** from Python PoC (differential tests against Python):

1. **Batch build** — fee order, `can_apply` vs ordered state, shard ownership + orphan age  
2. **Certify** — support weight ≥ threshold, parents certified, causal round  
3. **Order** — pure topo order of certs: `(causal_round, timestamp, batch_hash)`  
4. **Execute** — sequential apply; first valid wins conflicts  
5. **Finality** — soft on certify; hard when H ordered after  
6. **Equivocation** — creator cannot double-propose same parent set  
7. **Prune** — TTL uncertified; optional hard-final history cap  

---

## 6. Rust project structure (proposed)

```text
falcon-dag-rs/                 # new repo or monorepo crate root
  Cargo.toml                   # workspace
  crates/
    fd-types/                  # hashes, tx, batch, cert, serde
    fd-crypto/                 # Falcon-512 (liboqs / pqcrypto)
    fd-mempool/
    fd-dag/                    # DA: tips, certify, prune, rate limit
    fd-order/                  # pure ordering
    fd-exec/                   # account state apply
    fd-store/                  # redb/rocksdb + cold files
    fd-p2p/                    # gossip codec + peer manager
    fd-rpc/                    # JSON-RPC (axum/jsonrpsee)
    fd-node/                   # wiring + main binary falcon-dagd
    fd-genesis/                # genesis build tools
  deploy/
    Dockerfile
    docker-compose.yml         # 5-val local testnet
    configs/
  tests/
    differential/              # vs Python vectors
    e2e/
  docs/                        # link back to Python design docs
```

**Binary name:** `falcon-dagd`  
**Image:** `falcon-dag:testnet-v0.1`

### Suggested crates (deps)

| Concern | Crate |
|---------|--------|
| Async runtime | `tokio` |
| RPC | `axum` or `jsonrpsee` |
| Serialization | `serde`, `bincode` or `prost` for P2P |
| Store | `redb` (simpler) or `rocksdb` |
| Hashing | `sha2` / `blake3` (pick one; freeze) |
| Falcon-512 | `pqcrypto-falcon` or liboqs FFI |
| CLI | `clap` |
| Metrics | `prometheus` + `tracing` |
| P2P | start custom TCP; later `libp2p` |

---

## 7. Phased build plan

### Phase 0 — Freeze (1 week)

- [x] Python PoC + design docs  
- [ ] Tag Python `v0.3.3` as **spec freeze** for algorithms  
- [ ] This document accepted  
- [ ] Generate **test vectors** from Python (batches, certs, order, execute) → JSON fixtures  

### Phase 1 — Core pure logic in Rust (2–4 weeks)

- [ ] `fd-types`, `fd-crypto` (Falcon-512 sign/verify)  
- [ ] `fd-mempool`, `fd-dag`, `fd-order`, `fd-exec`  
- [ ] **Differential tests**: same fixtures as Python → identical order + state  
- [ ] No network yet — library only  

**Exit:** pure consensus matches Python on golden vectors.

### Phase 2 — Node binary + local P2P (3–5 weeks)

- [ ] `falcon-dagd` with TOML config  
- [ ] TCP JSON or binary gossip (compatible enough for 5 nodes)  
- [ ] JSON-RPC: `ping`, `server_info`, `submit`, `canonical`, `dag_status`  
- [ ] `docker compose` 5-node local testnet  
- [ ] Snapshot/WAL crash recovery  

**Exit:** `docker compose up` → 5 nodes agree under load (like Python regtest).

### Phase 3 — Hardening (3–6 weeks)

- [ ] Prune + checkpoints implemented and tested  
- [ ] DoS limits, rate limits, equivocation  
- [ ] Soak 24–72h  
- [ ] Failure: kill 1, kill 2, partition/heal  
- [ ] Metrics dashboards  

**Exit:** stable private testnet artifact.

### Phase 4 — Chain product surface (parallel / later)

- [ ] Real Falcon-512 **user** tx format  
- [ ] Genesis ceremony tooling  
- [ ] Explorer API  
- [ ] Public testnet network_id  
- [ ] Tokenomics / fees (if any) — separate design  

### Phase 5 — Optional bridge to Falcon Ledger

- Only after Phase 3; dual-run / bridge — **not** in the critical path of “new chain.”

---

## 8. Local testnet target (Rust)

```bash
# eventual UX
docker compose -f deploy/docker-compose.yml up -d
curl -s http://127.0.0.1:18001 -d '{"method":"server_info","params":[{}]}'
# 5 validators, Falcon-512 certs, network_id=9002
```

Parity checklist vs Python regtest:

| Check | Required |
|-------|----------|
| 5 nodes proposing/certifying | Yes |
| Falcon-512 certs | Yes |
| Agreement under flood | Yes |
| Double-spend one winner | Yes |
| Kill 1 progress / kill 2 stall | Yes |
| Restart catch-up | Yes |
| Soft/hard finality metrics | Yes |

---

## 9. Testing strategy

| Layer | Method |
|-------|--------|
| Unit | Rust tests per crate |
| Differential | Python golden vectors → Rust |
| Property | proptest (order determinism, no double apply) |
| E2E | docker compose + scripts ported from Python |
| Soak | 24–72h with Prometheus |
| Fuzz | batch/sig decode, mempool priority |

**Python stays** as oracle until Rust differential suite is green for 2+ releases.

---

## 10. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Falcon-512 CPU cost | Fat batches under load; measure early |
| Memory growth | Prune/checkpoint as **v1** |
| Spec drift from Python | Differential tests, freeze tag |
| Underestimating P2P | Start simple TCP; don’t block on full libp2p |
| Scope creep (DEX, bridges) | Phase 4 only after Phase 3 |

---

## 11. What “done” means for the rebuild

| Milestone | Definition |
|-----------|------------|
| **M1** | Rust pure logic ≡ Python vectors |
| **M2** | `falcon-dagd` image + 5-node local testnet |
| **M3** | 72h soak, failure suite green |
| **M4** | Public private-testnet announcement (optional) |

You are **ready to start M1** now.

---

## 12. Immediate next actions

1. **Approve this plan** (especially: Rust, network_id scheme, prune-as-v1).  
2. Create repo/workspace `falcon-dag-rs` (or `crates/` under this monorepo).  
3. Export golden vectors from Python (`scripts/export_vectors.py`).  
4. Scaffold Cargo workspace + `fd-types` + `fd-crypto`.  
5. Implement order+exec first (smallest pure loop), then DA, then P2P.

---

## 13. Bottom line

| Question | Answer |
|----------|--------|
| Full plan for schematics / network / state / settings? | **This document** |
| Rebuild in Rust recommended? | **Yes** for a serious new chain |
| Ready to start Rust? | **Yes** — use Python as spec |
| Memory if “ledgers” fly in fast? | **Prune + checkpoint + hot/cold** (required v1) |
| Is volume “same speed, more txs”? | **Partly** — also parallel certs + fatter/slower batches under load |

**Recommendation:** Treat this plan as the baseline, freeze the Python algorithms, and begin the Rust workspace for a **new chain local testnet**, without touching production Falcon Ledger.
