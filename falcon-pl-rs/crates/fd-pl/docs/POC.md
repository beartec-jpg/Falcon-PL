> **HISTORICAL.** This file is not the live Falcon PL **2300** dest-lock specification.
> Current: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md) and [README.md](README.md).
> Falcon Ledger **1001** lock-mint is shut down.

# Falcon PL (FPL) — Proof of Concept Report

**Product:** Falcon PL (Falcon Participation Ledger)  
**Ticker:** FPL  
**Consensus:** Falcon Consensus  
**Signatures:** Falcon-512  
**Status:** Proof of concept (research prototype)  
**Code:** `crates/fd-pl` · workspace default  
**Date of load campaign (reference):** 2026-08-10 → 2026-08-11  

---

## 1. Executive summary

Falcon PL is a **greenfield** ordered-ledger prototype. It is **not** an XRP Ledger fork and **not** the earlier Falcon DAG research stack. It runs **Falcon Consensus**: bonded participants join a **packer lottery**; the elected packer seals a **non-empty**, time-budgeted ledger; every node verifies; soft/hard OK and depth confirm; a silent packer is **skipped** so the next ranked packer can seal. Transactions and packer seals use **Falcon-512**.

### PoC claims (supported by tests below)

| Claim | Evidence |
|-------|----------|
| Falcon-512 end-to-end on multi-host mesh | Dual-host testnet (3 validators, F1+F2), scheme `falcon-512` |
| Shared pending set | Tx gossip on new insert; mempool sizes converge |
| Clean operation under market-shaped ramps | ~4.3 h soak: 20→50→80→**150** tx/s cycles, **~885k submit / 0 err** |
| Intake well above prior Falcon Ledger API wall (~30 TPS) | Break ramp **clean keep-up through 500** tx/s peaks |
| Soft throughput wall before hard crash | Strain **~600–800** (tip lag, backlog); **OOM ~900** (resource, not consensus “lie”) |
| Tip cadence ≠ XRPL ~3 s | Observed mean tip interval **~0.2 s** under soak (soft path + pipeline) |

### Explicit non-claims

- Not a formal BFT security proof.  
- Not production networking (JSON-TCP fanout, not DHT).  
- Not unbounded history safety (in-memory chain → OOM under extreme long load).  
- Not Falcon Ledger (qXRP) / RPCA compatibility.

---

## 2. Product identity

| | |
|--|--|
| **Product** | Falcon PL |
| **Long name** | Falcon Participation Ledger |
| **Ticker** | FPL |
| **Consensus name** | **Falcon Consensus** |
| **Crypto** | Falcon-512 (NIST PQC Level 1 class) |

**Name split (required):**

| Name | Means |
|------|--------|
| Falcon-512 | Signature scheme |
| Falcon Consensus | Lottery packer + soft/hard OK + ranked progress timeout + verify/slash |
| Falcon PL / FPL | Product / chain |
| Falcon Ledger (qXRP) | Separate XRPL-fork product |

Full naming: [IDENTITY.md](IDENTITY.md).

---

## 3. Falcon Consensus (model)

### 3.1 Participants

- **Bonded, non-jailed** validators form the eligible set.  
- Bonding is how a party **joins the lottery** (participation story).  
- All such nodes may **verify** and cast **OK** votes (checkers).

### 3.2 Per-height pipeline

1. **Lottery** — Deterministic ranked packers (#1, #2, …) from network id, next height, tip hash, skip set, and bonds. Same tip + registry ⇒ same ranking.  
2. **Pack** — Winner packs applicable mempool txs under `pack_budget_ms` (default 1000 ms). **Empty ledgers rejected.**  
3. **Sign** — Packer signs ledger with Falcon-512 (or HMAC-dev in harness).  
4. **Verify** — Parent, height, expected packer, signature, non-empty, state root.  
5. **Soft / hard OK** — Soft (e.g. ≥3 OK votes) enables pipeline progress; hard (e.g. ≥60% of checkers) strengthens confirm; tip depth adds confirmations.  
6. **Progress timeout** — If tip does not advance and mempool has work for `progress_timeout_ms` (default 1000 ms), current winner is skipped and #2 becomes lottery winner.  
7. **Equivocation** — Same packer + height + different hash ⇒ slash/jail.

### 3.3 Mempool

- New txs **gossiped** to peers (deduped by `tx_id`).  
- Optional **max_mempool_txs** (default 50_000): reject new unique txs when full (backpressure; not a substitute for history bounds).

### 3.4 What “1s pack” means

`pack_budget_ms` is a **maximum** packing duration for one ledger, **not** a fixed close timer. Under soak load, mean tip interval was **~0.2 s** (many small ledgers + soft pipeline). Falcon Ledger (RPCA) closes are **~3–3.5 s** — a different consensus clock.

---

## 4. Architecture vs prior work

| Dimension | Falcon PL (this PoC) | Archived Falcon DAG research | Falcon Ledger (qXRP) |
|-----------|----------------------|------------------------------|----------------------|
| Code path | `crates/fd-pl` | `archive/falcon-dag-research/` | Separate repo (rippled fork) |
| Data structure | Ordered hash-linked **ledgers** | Certificate **DAG** + order | Single open ledger (XRPL) |
| Agreement | **Falcon Consensus** (lottery packer) | Narwhal/Bullshark/tip-BFT experiments | **RPCA** |
| Crypto | Falcon-512 | Mixed / research | Falcon-512 always-on |
| Economy | Minimal (fees→packer in proto) | Research | PoP, treasury, epochs, … |

---

## 5. Implementation snapshot

| Component | Implementation |
|-----------|----------------|
| Language | Rust (`fd-pl` crate) |
| Wire | JSON lines over TCP; persistent peer writers |
| Nodes | `falcon-pl-node` |
| Ops | `falcon-pl-ctl`, load bins, `scripts/pl-*.sh|py` |
| Defaults (testnet) | `pack_budget_ms=1000`, `progress_timeout_ms=1000`, `max_mempool_txs=50000`, `seal_interval_ms≈300` |

---

## 6. Experimental results

### 6.1 Environment

| Item | Value |
|------|--------|
| Scheme | falcon-512 |
| Validators | v1, v2 (host F1), v3 (host F2) |
| Ports | 19101–19103 |
| Logs | `~/falcon-pl-testnet-falcon/logs/` |

### 6.2 In-process crypto / pack (bench)

Order-of-magnitude on build host (representative):

- Sign ~**5–6k**/s, verify ~**20k**/s (Falcon-512) — in line with public Falcon reference ballpark.  
- Continuous pack+accept (in-process) multi-k tx/s possible when network is not the bottleneck.

### 6.3 Multi-host soak (cut short ~4.3 h of planned 6 h)

| Metric | Result |
|--------|--------|
| Elapsed | **~4.28 h** (operator cut short for break ramp) |
| Tip | **0 → ~74k** |
| Phase submit_ok | **~885k** |
| Phase submit_err | **0** |
| Profile | quiet 20 → ramp 50 → elevated 80 → **spike 150** → cool → repeat |
| Tip agree | Generally yes; occasional **transient 1-height lag** |
| Mean tip interval | **~0.21 s** under soak phases |
| Mempool full | Not hit |

Logs: `logs/endurance-6h/` (`summary_cut_short.json`, `endurance.csv`, `phases.jsonl`).

### 6.4 Break ramp (200 → 1000)

| Peak rate (tx/s) | submit_ok | err | Δtip / ~2 min hold | Outcome |
|------------------|-----------|-----|--------------------|---------|
| 200 | 24,001 | 0 | +498 | Clean keep-up |
| 300 | 36,001 | 0 | +711 | Clean |
| 400 | 48,001 | 0 | +734 | Clean |
| 500 | 59,992 | 0 | +771 | Clean |
| 600 | 70,643 | 0 | +240 | Soft wall (backlog, lag) |
| 700 | 80,067 | 0 | +229 | Soft wall continues |
| 800 | 86,963 | 0 | +229 | Soft wall continues |
| 900 | mid-run | — | — | **Host OOM** (see §6.5) |
| 1000 | not reached | | | |

Logs: `logs/final-ramp/` (`phases.jsonl`, `ramp.log`).

### 6.5 Failure analysis (900 tx/s)

| Question | Answer |
|----------|--------|
| Consensus correctness failure? | **No** — nodes still sealing large ledgers until kill |
| Hardware / resource? | **Yes** — Linux **OOM killer** |
| Evidence | Two `falcon-pl-node` ~**7.5 GB RSS** each on ~**15 GB** host; `oom-kill` of v2 then cascade |
| Mempool cap | Did **not** trip 50k full first; **in-memory full chain history** dominated RAM |

**Conclusion:** Break at extreme load was **resource exhaustion**, not a demonstrated safety violation of Falcon Consensus rules.

### 6.6 Comparison to Falcon Ledger (qXRP) testnet load (May 2026)

Source: FalconLedger `QXRP_CHAIN_REPORT_2026-05-29.md` (separate product).

| | Falcon Ledger testnet | Falcon PL PoC |
|--|----------------------|---------------|
| Consensus | RPCA | Falcon Consensus |
| Long soak | ~71 h @ ~0.3–6 TPS | ~4.3 h @ 20–150 cycles |
| Hard intake note | ~**30 TPS** sustained → HTTP **503** / fee escalate | Submit **500** clean; strain **600+**; OOM **900** |
| Ledger close clock | ~**3–3.5 s** | Tip steps ~**0.2 s** (different model) |
| Volume | ~853k accepted / 71 h | ~885k submit / ~4.3 h (hotter profile) |

Fair private takeaway: **same Falcon-sized signatures**, different engine; FPL PoC exercised **much higher continuous submit rates** on a thin stack; Falcon Ledger proved **longer RPCA soak** and full product surface.

---

## 7. Limits and next engineering

| Priority | Item |
|----------|------|
| P0 | Bound or prune **in-memory history** (prevent OOM on long/hot runs) |
| P1 | Production-grade gossip / peer management |
| P1 | Formalize adversarial tests (partition, double-sign network-wide) |
| P2 | Fee market / replace-by-fee under full mempool |
| P2 | Larger validator sets and geographic latency |
| P3 | Economy (rewards, bond parameters) if productized |

---

## 8. How to reproduce

```bash
cd falcon-dag-rs
cargo build -p fd-pl --release
cargo test -p fd-pl

# Dual-host Falcon testnet
PL_SCHEME=falcon-512 PL_ROOT=$HOME/falcon-pl-testnet-falcon \
  bash crates/fd-pl/scripts/pl-testnet-v1.sh start

# Soak / break (see script headers)
python3 crates/fd-pl/scripts/pl-6h-endurance.py --help
python3 crates/fd-pl/scripts/pl-final-ramp.py --help
```

---

## 9. Verdict

Falcon PL (FPL) is a **successful proof of concept** for:

1. Branding and package: **Participation Ledger + Falcon Consensus + Falcon-512**.  
2. Multi-host operation with **real Falcon** signatures, gossip mempool, and standby packer.  
3. Sustained multi-host load **well above** single-digit TPS and above the historical Falcon Ledger **~30 TPS API** note, with a clear soft wall (~600+) and a hard **resource** wall (~900 on 15 GB dual-node host).

It is **not** production software. Next work should treat memory and networking as first-class, then re-run break tests with history caps so OOM does not mask consensus limits.

---

*Document control: PoC campaign Aug 2026 · archive of prior DAG research: `archive/falcon-dag-research/`.*
