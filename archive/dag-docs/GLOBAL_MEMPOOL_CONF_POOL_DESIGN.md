# Design: Global Mempool (GMP-B/C) + Capacity-Weighted Issuance + Global Conf Pool (GCP-B)

**Status:** Implemented PR-G1..G5 — **live full-stack PASS** (2026-08-10)  
**Date:** 2026-08-10  
**Goal:** Scalable, safe, stable network where intent is **global**, packing is **fair and capacity-aware**, and finality remains UNL + Bullshark — without hard lane segregation.

### Implementation status

| PR | Status | Artifact |
|----|--------|----------|
| PR-G1 GMP-B core + `falcon-gmpd` | **Done** | `falcon-dag-rs/crates/fd-gmp` |
| PR-G2 `falcon-gcpd` v0 | **Done** | same crate, `falcon-gcpd` bin |
| PR-G3 Live stack harness (external issuer) | **Done** | `deploy/gmp/11-gmp-live-stack.py` |
| PR-G4 Engine lease integration | **Done** | `falcon-dagd --gmp-url` agent + `deploy/gmp/12-gmp-engine-live.py` |
| PR-G5 GCP engine hooks | **Done** | pack/sig/cert → `--gcp-url` + `deploy/gmp/13-gmp-gcp-live.py` |

### PR-G4 engine path

```
Clients ──Submit──► falcon-gmpd
                         ▲ CapacityAdvert + Lease/Complete
                         │
              falcon-dagd (each packing val, in-process agent)
                         │ admit → multipack owned lanes / directed handoff
                         ▼
              pack / cert / Bullshark / exec
```

- CLI: `--gmp-url`, `--gmp-lease-max`, `--gmp-budget-mb`, `--gmp-max-in-flight`, `--gmp-affinity`, `--gcp-url`
- RPC dig controls: `gmp_control {enabled}`, `gmp_stats`
- Soft affinity on lease: prefer `fnv(account) % worker_count == worker_index`, overflow allowed

**PR-G5 live** (`deploy/gmp/GMP_GCP_REPORT.*`):

| Metric | Result |
|--------|--------|
| Phase A | **PASS** · exec_tps_min **15.73** · lag 0 · agreement |
| GCP registry | **total=158 certified=158** (open drained) |
| Node publishes | batches≈644 · sigs≈1600 · certs≈482 (sum across vals) |
| Capacity skew | val1/val2:158 · val3:106 leases |
| Phase B kill-agent | **6.86** TPS · pending→0 · still certifying to GCP |

**PR-G4 live** (engine leases only): ~17.6 TPS + kill-agent dig.  
**PR-G3** (external issuer): ~22 TPS.

### Scale dig with GMP (2026-08-10) — honest result

Harness: `deploy/gmp/14-gmp-scale-n.py` · report: `deploy/gmp/SCALE_N_GMP_REPORT.md`

| N | best exec_tps_min | med (50/s) | lag | agreement |
|---|-------------------|------------|-----|-----------|
| 3 | **21.8** | 21.8 | 0 | yes |
| 6 | **14.66** | 14.66 | 0 | yes |

**Verdict: SCALE_OUT_FAIL** — `exec_tps_min(N=6) < exec_tps_min(N=3)` (~33% regression).

Interpretation:
- GMP front door **does not** unlock horizontal scale-out by itself.
- N=6 under GMP was **stable** (lag 0, agreement) — better than some pre-GMP collapses (~6–12 TPS thrash), but still **slower** than N=3.
- Bottleneck remains **UNL cert width / multipack coordination**, not “clients can’t find a mempool.”

### Pack workers + UNL-only certs (lab dig 2026-08-10)

Implemented: `--packers` multipack set may include **non-UNL** nodes; only **UNL** signatures count for certificates (already true in `support_weight`; pack workers skip conf-pool Falcon signing).

Harness: `deploy/gmp/15-pack-workers-dig.py` · report: `deploy/gmp/PACK_WORKERS_REPORT.md`

| Mode | Packers | UNL role | exec_tps_min | stable |
|------|---------|----------|--------------|--------|
| A classic | 3 (=UNL) | pack+cert | **26.3** | yes |
| B mixed | 6 (UNL+pack*) | pack+cert | **20.1** | yes |
| **C pure** | **3 (pack* only)** | **cert only** | **20.2** | yes |

\*pack workers created batches (C: pack1=40, pack2=19, pack3=12) and UNL certified — **architecture works**.

**Verdict: C pure split works but REGRESS vs classic A** (~20 vs ~26 TPS).  
Same cert width (3); packing fully offloaded to workers; UNL never packs / never GMP-leases. Still not faster — extra hop pack→UNL sign adds cost without reducing cert load.

Interpretation: role split is **live and safe**. Throughput needs **cheaper certs / fatter batches / smaller effective cert committee under load**, not only more packers.

### Fat-batch amortization dig (2026-08-10)

Harness: `deploy/gmp/16-fat-batch-dig.py` · report: `deploy/gmp/FAT_BATCH_REPORT.md`  
N=3 multipack + GMP · load 80/s × 35s · sweep `max_batch_txs` × interval.

| max_batch | interval | exec_tps_min | cert/s | txs/batch≈ | exec/cert≈ |
|-----------|----------|--------------|--------|------------|------------|
| 16 | 0.05 | **40.2** | 2.92 | 41 | 14 |
| 64 | 0.05 | 37.9 | 2.51 | 45 | 15 |
| 256 | 0.05 | 38.4 | **1.92** | **60** | **20** |
| 512 | 0.20 | 39.6 | 2.28 | 52 | 17 |

**Verdict: FAT_BATCH_FLAT on TPS** (best ≈40 ≈ baseline64 ≈38).

**But amortization signal is real:** fatter caps cut **cert/s** (~2.9 → ~1.9) and raise **exec/cert** (~14 → ~20).  
Lane multipack + ready-seq packing means batches rarely hit 256/512 full — density plateaus ~40–60 txs/batch.

Interpretation: “sign fewer, bigger units” **reduces agreement rate** but **does not raise the ~40 TPS ceiling** under this load — bottleneck is not “too many small certs” alone (crypto/pipeline/lane structure still bound).

### Remaining

- Bundle-cert over conf pool (Merkle of many opens) if single-batch fattening plateaus
- Cert/UNL committee; Falcon sign/verify cost dig
- Multi-host HA GMP/GCP
- Capacity budget from real RSS

---

## 1. Problem

| Today | Desired |
|-------|---------|
| Per-node mempools | **One logical global intent log** (physically sharded/replicated) |
| Hard FNV owner-route silos | **Soft affinity + reissue** (no permanent starvation) |
| Per-node conf pools | **Shared conf plane (GCP-B)** among spine + packers |
| Archivals follow only | Full-history/spine **hosts intent + fair issue** |
| More packers ≠ more TPS | Packers = **workers on shared backlog**; UNL growth gated by digs |

Digs proved: N≈3 multipack is stable (~40 exec TPS min); N=6 collapses; lanes over-segregate; pack_open_cap knobs don’t fix scale-out.

---

## 2. Goals / Non-goals

### Goals
- **G1 Safe:** No double-lease of same (account, sequence); exec still enforces sequences/sigs.
- **G2 Stable:** Lease TTL + reissue; capacity caps prevent small vals from OOM-grabbing.
- **G3 Fair:** Issue ∝ advertised capacity × observed clearance (floors/ceilings).
- **G4 Global log:** All admits visible in GMP; clients need not compute owner.
- **G5 Live testable:** Lab can run GMP + N=3 packing + capacity issue + measure exec_tps_min.

### Non-goals (this implementation slice)
- Full multi-Raft production HA across regions.
- Full GCP replacing in-node conf_pool (v0 = registry service + hooks; packers still cert locally).
- Large-UNL cert cost elimination (parallel track).
- GMP-A (single global Raft leader) as product path — **explicitly out**.

---

## 3. Key Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | **GMP-B first** (multi-shard by account), API-compatible with future **GMP-C** dissemination | Horizontal admit + clear sequence authority per shard; avoid GMP-A SPOF vision |
| D2 | **Capacity-weighted leases** with self-advert + clearance demotion | Big vals do more, small less; hard to game forever via expiry tracking |
| D3 | **GCP-B direction** for conf; v0 = `falcon-gcpd` registry (PutBatch/PutSig/ListOpen) | Matches vision without blocking on full engine rewrite day one |
| D4 | **Soft affinity** only; exclusive lease TTL | Mitigates segregation while keeping locality |
| D5 | **Issuance outside packing binary first** (issuer workers call existing `submit` RPC) | Full-stack live test without forking pack path; Phase 2 can pull leases in-engine |
| D6 | **Honest metrics:** exec_tps_min + agreement | No fudge |

---

## 4. Architecture

```
Clients ──Submit──► GMP-B (sharded global mempool service)
                         │
                         │ CapacityAdvert ◄── packing validators
                         │ Lease(packer) ──► proportional to capacity
                         ▼
              Issuer / packer agent (per val or shared)
                         │ submit leased txs via existing val RPC
                         ▼
              Packing validators (local pack/cert/order as today)
                         │ optional: PutBatch/PutSig ──► GCP-B registry
                         ▼
              Bullshark + exec + archivals
```

### 4.1 GMP-B sharding

- `shard = FNV1a(account) % num_shards` (default 16 lab shards).
- Each shard: ordered queue of pending txs by (account, sequence), exclusive lease map.
- Sequence policy (v0): accept client sequence; reject duplicate (account,seq) while pending/leased; complete removes; on lease expiry requeue.

### 4.2 Capacity advert

```json
{
  "validator_id": "val1",
  "mem_budget_mb": 2048,
  "mem_used_mb": 400,
  "open_uncertified": 1,
  "max_in_flight_leases": 64,
  "recent_clearance_tps": 12.0,
  "lease_expiry_rate": 0.0,
  "ts": 1234567890.1
}
```

**Weight (v0):**

```
headroom = max(0, mem_budget_mb - mem_used_mb) / max(1, mem_budget_mb)
clear   = clamp(recent_clearance_tps / clear_ref, 0, 2)
penalty = clamp(lease_expiry_rate, 0, 1)
weight  = max(floor, headroom * (0.5 + 0.5*clear) * (1 - 0.5*penalty))
quota_i = total_issue * weight_i / sum(weights)
```

Ceil any single validator at `max_share` (default 0.4). Floor at `min_share` (default 0.05) if advert fresh.

### 4.3 Lease state machine

```
Pending --Lease--> Leased --Complete--> Done
                     |
                     +--TTL/Nack--> Pending (reissue)
```

Invariant: at most one active lease covers a given pending tx_id.

### 4.4 GCP-B v0

HTTP registry:

- `PUT /v1/batch` — register open batch meta  
- `PUT /v1/sig` — record sig  
- `GET /v1/open` — list open (not certified)  
- `PUT /v1/cert` — mark certified  

Does **not** replace in-node conf_pool in v0; provides shared observability and foundation for later engine integration.

---

## 5. APIs (HTTP JSON)

### GMP (`falcon-gmpd`, default `:18500`)

| Method | Path | Body | Result |
|--------|------|------|--------|
| POST | `/v1/submit` | `{account,sequence,destination,amount,fee}` | `{tx_id, shard, status}` |
| POST | `/v1/capacity` | CapacityAdvert | `{ok, weight}` |
| POST | `/v1/lease` | `{validator_id, max_n}` | `{lease_id, txs:[...], ttl_s}` |
| POST | `/v1/complete` | `{lease_id, batch_hash?}` | `{ok}` |
| POST | `/v1/nack` | `{lease_id, reason}` | `{ok}` |
| GET | `/v1/status` | — | backlog, shards, weights |

### GCP (`falcon-gcpd`, default `:18501`)

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/v1/batch` | Register open batch |
| POST | `/v1/sig` | Merge sig record |
| POST | `/v1/cert` | Mark certified |
| GET | `/v1/open` | List open batches |
| GET | `/v1/status` | Counts |

---

## 6. Safety

- Exclusive lease per tx; reissue only after expiry/nack.
- GMP does not execute; packing+UNL+Bullshark still required for finality.
- Capacity lies mitigated by clearance/expiry weighting + max_share.
- Global backpressure: if pending > `max_pending`, submit returns busy.

---

## 7. Live full-stack test plan

1. Start `falcon-gmpd` + `falcon-gcpd`.
2. Start N=3 multipack mesh (existing real-mesh).
3. Start **capacity reporter** per val (poll server_info → POST /v1/capacity).
4. Start **issuer** loop: for each val with weight, Lease → submit each tx to that val’s RPC.
5. Client flood: Submit to GMP only (no owner-route).
6. Pass if:
   - exec_tps_min > 0 and grows with load in warm band  
   - all vals + archivals agree after settle  
   - lag=0  
   - killing one packer’s issuer still clears backlog via others (fair reissue dig)

---

## 8. PR Plan

| PR | Title | Scope |
|----|-------|--------|
| **PR-G1** | `fd-gmp` core + `falcon-gmpd` | Sharded mempool, capacity, leases, HTTP |
| **PR-G2** | `falcon-gcpd` v0 | Conf registry HTTP |
| **PR-G3** | Live stack harness | capacity reporter, issuer, client, dig script |
| **PR-G4** | Engine integration (later) | Packers lease in-process; retire dual-path submit |
| **PR-G5** | GCP engine hooks (later) | Publish open batches/sigs to GCP-B |

**This delivery implements PR-G1..G3 through live tests.**

---

## 9. Open questions (resolved for v0)

| Q | v0 choice |
|---|-----------|
| GMP-A? | **No** |
| GMP-B vs C? | **B (sharded)** now; C-compatible APIs later |
| Who writes GMP? | Any client / lab loadgen |
| Sequence authority? | Client-provided seq; GMP enforces uniqueness while pending |
| Archivals vote? | Unchanged (not required for GMP v0) |

---

## 10. Success metrics for this implementation

| Metric | Target |
|--------|--------|
| Unit tests | GMP lease exclusivity, reissue, weight floors |
| Live: submit only to GMP | ≥ warm exec_tps_min ~5+ on N=3 |
| Live: agreement after settle | exec spread ≤ 5, lag=0 |
| Live: capacity skew | Higher mem_budget val receives more leases |
| Docs | This design + runbook in deploy/gmp/ |
