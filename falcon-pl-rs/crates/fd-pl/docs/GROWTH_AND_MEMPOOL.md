# Production growth architecture — validators, history, mempool

**Status:** design lock + **v2.2.4 impl** (hub role, NeedTxs, dual queues, seq-poison safe pack) — private soak → public testnet (v2.2.x → v2.3)  
**Product:** Falcon PL · Falcon Consensus  
**Related:** [LIVE_BOND_IN.md](LIVE_BOND_IN.md), [EXPANSION_PLAN_v2.2.0.md](EXPANSION_PLAN_v2.2.0.md), [AMENDMENTS.md](AMENDMENTS.md)

---

## 1. Problem

A live network does **not** start with 50 equal nodes. It starts small and grows:

1. A few operators run **genesis validators** + at least one **full-history** path  
2. New operators **bond in** without restarting the mesh  
3. Capacity (RPC, history, mempool durability, geography) is added over time  

Under load we learned that **per-node mempools + best-effort gossip** are only “global” when quiet. When asymmetric, lottery winners are progress-timeout-skipped and the tip freezes. Soft confirm (3 votes) is not the bottleneck.

**Production requirement:** stay **decentralized**, support **gradual growth**, keep **packing correct** under multi-host load.

---

## 2. Principles

| # | Principle |
|---|-----------|
| P1 | **Logical global pending set** — packers must be able to see the work the network expects sealed |
| P2 | **No single mempool or history box** — always ≥1 of each critical role; prefer ≥3 of hub-class roles before public |
| P3 | **Roles scale independently** — more light vals ≠ more full archives |
| P4 | **Growth is live bond-in** — registry via Bond ledgers; join via snapshot + residual catch-up ([LIVE_BOND_IN.md](LIVE_BOND_IN.md)) |
| P5 | **Tip advances on accept** — soft (3 OK) / hard (ratio) are confirmation, not pack gates |
| P6 | **Progress timeout is failover**, not normal path — must not punish winners who never received txs |

---

## 3. Roles (production)

Three roles. One process *may* play more than one; operators choose what they run.

### 3.1 Light validator (`val`)

| | |
|--|--|
| **Job** | Pong, lottery, pack when elected, check/vote, short chain tail |
| **State** | Tip state + pruned `chain` + **small local mempool** (pack working set) |
| **Does not need** | Full history from genesis; public submit flood |
| **Growth** | Live bond-in (`--join` + snapshot + residual) |

### 3.2 Consensus hub / mempool hub (`hub`)

| | |
|--|--|
| **Job** | Primary **SubmitTx** intake; **replicate pending** among hubs; serve **NeedTxs** to packers; optional pack/check if also bonded |
| **State** | Strong pending set (durable optional) + tip; may be pruned or archive |
| **Count** | Start **1**, production private **≥2**, public testnet **≥3** independent hubs |
| **Trust** | Hubs do **not** replace lottery; they **feed** packers |

### 3.3 Full-history / archive (`archive`)

| | |
|--|--|
| **Job** | Keep long/full chain for catch-up, audit, explorers, hard-resync when possible |
| **State** | `max_chain_ledgers=0` (or external store later); often co-located with a **hub** |
| **Count** | Start **1**, healthy private **≥2**, public **≥3** preferred |

```text
                    Clients / wallets / load
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
           Hub A           Hub B           Hub C     ← mempool + usually RPC
           (often archive) (often archive)
              │               │               │
              │   NeedTxs / TxBatch / Hello tip    │
              └───────────────┼───────────────┘
                              │
         ┌────────┬───────────┼───────────┬────────┐
         ▼        ▼           ▼           ▼        ▼
        V1       V2    …   light vals   …        Vn   ← lottery / pack / check
```

**Sealing stays on the lottery winner** (any bonded val or hub-that-is-also-val). Hubs are not a separate “permissioned packer set” unless we deliberately change consensus (Class E — out of scope).

---

## 4. Growth stages (how the network actually expands)

### Stage 0 — Genesis / private bootstrap

| Piece | Minimum |
|-------|---------|
| Bonded vals | **3** (soft=3 is meaningful) |
| Archive | **1** (often val #1) |
| Hub | **1** (can be same process as archive+val) |
| Light-only vals | 0 |

- Genesis registers seed vals only.  
- Single process may be `archive + hub + val` (today’s host1-style).  
- Acceptable for day-1 private; not for public.

### Stage 1 — Private multi-host harden (v2.2.x)

| Piece | Target |
|-------|--------|
| Bonded vals | 3 → 7–13 via **live bond-in** |
| Archive | **2** (two machines) |
| Hub | **2–3** (may equal archives) |
| Light vals | rest of bonded set |

- Submits prefer **hubs** (clients multi-POST 2-of-N).  
- Light vals pull pending if local empty before progress-timeout.  
- Snapshots persist tip + chain tail + true genesis ([AMD-20260812](AMENDMENTS.md)).  
- All-skip re-open on wire (skip cascade fix).

### Stage 2 — Pre-public freeze

| Piece | Target |
|-------|--------|
| Vals | N fixed for freeze sheet |
| Archive | **≥3** |
| Hub | **≥3** diverse operators |
| Params | freeze `params_version`, soft=3, hard ratio, timeouts |

### Stage 3 — Public testnet (v2.3.0)

- Public network_id / genesis  
- Published seeds = hubs + a few vals  
- Anyone can bond-in as **light val**  
- Archives/hubs remain a smaller, documented set (still multi-operator)

---

## 5. Mempool model (production)

### 5.1 Not this

- One central mempool process for the whole world  
- Every light val mesh-floods every tx to every peer (today under stress)  
- Winner skipped only because *observers* have txs the winner never saw  

### 5.2 This

**Logical global pending set**, stored **redundantly on hubs**, **cached on vals**:

| Path | Behaviour |
|------|-----------|
| Submit | Client → **≥2 hubs** (or any val as fallback) |
| Hub↔hub | Full pending replication (push + anti-entropy) |
| Hub→val | Push recent high-fee / or **pull NeedTxs** when packing |
| Val local | Small mempool for pack; prune after seal / TTL |
| Val↔val | Seals, votes, pongs, skips — **not** full tx mesh as primary |

Optional: hub (or archive host) runs **`falcon-pl-mpd`** as a second process for isolation; light vals keep in-process pool only.

### 5.3 Packer algorithm (winner)

```text
1. may_pack (lottery + pong + not lagging)
2. candidates = local mempool
3. if candidates thin: NeedTxs from 1–2 hubs (timeout budget << progress_timeout)
4. pack_as within pack_budget_ms
5. accept + ProposeLedger + LedgerOk
6. peers accept + vote → soft at 3, hard at ratio
```

Progress timeout only if tip still flat **after** winner had a fair chance (including pull).  
Do not treat “I have mempool” alone as proof the winner is faulty — require **applicable** next-seq work (`has_applicable_tx`). Seq-poison (future sequences only, e.g. stale load-seq after re-genesis) must **wait**, never self-skip every lottery winner (v2.2.4 / AMD-20260812-4).

---

## 6. Consensus vs data plane (keep separate)

| Plane | Messages | Who |
|-------|----------|-----|
| **Consensus** | Hello/Pong, ProposeLedger, LedgerOk, SkipPacker, NeedLedgers | All vals |
| **Mempool / DA** | SubmitTx, NeedTxs, TxsBatch, hub anti-entropy | Hubs + packers |
| **History** | Long NeedLedgers / snapshot export | Archives |

This is how the network stays quiet as **N vals grows** but **hub count stays O(1)–O(few)**.

---

## 7. Confirmations (unchanged product rule)

| | Rule | Notes |
|--|------|--------|
| Soft | **3** distinct OK votes | Fixed; good for small genesis |
| Hard | **ratio** of bonded checkers (e.g. 0.6) | Scales with N |
| Tip advance | On **accept**, not on soft/hard | Soft/hard are client-facing finality |

Timers (illustrative private defaults):

| Timer | Role |
|-------|------|
| `seal_interval_ms` ~400 | Loop wake |
| `pack_budget_ms` ~1000 | Fill one ledger |
| `progress_timeout_ms` ≥ pack_budget + network slack | Failover only |

---

## 8. Operator journey (growing network)

```text
Day 0   Genesis: 3 vals, 1 archive+hub (can be one host each or co-located)
Day 1   Second archive+hub on another host
Week 1  Bond-in light vals (snapshot + --join); submits still to hubs
Month+  Third hub; more light vals; publish seeds for public cut
```

**Adding a light val:** [LIVE_BOND_IN.md](LIVE_BOND_IN.md)  
**Adding a hub/archive:** deploy process with hub flags + peer hubs; no need to re-genesis.

---

## 9. Implementation roadmap (v2.2.x)

| Step | Work | Depends |
|------|------|---------|
| **M0** | Document roles (this file); ops submit-to-seeds | — |
| **M1** | Reliable SubmitTx path (queues; no drop of txs before seals) | M0 |
| **M2** | `NeedTxs` / `TxsBatch`; packer pull-before-timeout | M1 |
| **M3** | Hub role flag + hub mesh anti-entropy | M2 |
| **M4** | Optional `falcon-pl-mpd` on hub hosts | M3 |
| **M5** | Metrics: mempool_len per role, pull latency, skip reason codes | M2+ |
| **M6** | Public seed list + freeze sheet | Stage 2 |

Existing today (reuse): live bond-in, snapshot chain tail, pong lottery, lagging-skip, all-skip re-open, soft=3.

---

## 10. Security / liveness notes

- **Censorship:** clients multi-submit; any val may still accept SubmitTx as escape hatch.  
- **Hub cartel:** ≥3 hubs under different operators before public; vals can pack from local if hubs fail (degraded).  
- **Lottery fairness:** bond + pong only (locked); hubs do not get extra weight unless they bond.  
- **History availability:** ≥2 archives before relying on hard-resync from h=1; snapshot state-sync remains primary for joiners.

---

## 11. One-page summary

| Stage | Vals | Archive | Hub | Mempool |
|-------|------|---------|-----|---------|
| Bootstrap | 3 | 1 | 1 (same box OK) | Local + gossip |
| Private grow | 3→N live join | 2+ | 2–3 | **Hub-centric + val pull** |
| Public | N open bond-in | 3+ | 3+ | Hub mesh + light cache |

**Production-grade growth path:**  
start with a **small bonded set + one full-history hub**, add **hubs/archives for data plane capacity**, add **light validators via bond-in for consensus capacity**, keep **one logical mempool via hub replication + packer pull** — never a single global process, never force every val to carry full history or full submit flood.

---

*Amend via AMD if roles or timeouts become Class B parameters; Class C protocol adds go in 2.2.x notes until public freeze.*
