# Real-mesh 3-val suite report + recommendations

**Suite:** `/tmp/real-mesh-3val-suite-20260807-112828`  
**When:** 2026-08-07 ~11:28–11:34 UTC  
**Network:** id=9201, crypto=falcon-512, UNL=`val1,val2,val3`, threshold=0.6  

## Topology under test

| Host | Spec | Processes |
|------|------|-----------|
| host1 | 16GB | val1 (full/validator) + arch1 (order-exec/archival) |
| host2 | 16GB | val2 + arch2 |
| droplet | 4GB / 2 vCPU, **no swap** | val3 only |

## Test matrix

| ID | Test | Target | Result |
|----|------|--------|--------|
| T0 | Baseline | all up, lag=0 | **PASS** (exec already 281 from prior smoke) |
| T1 | Light load | rate=15 / 30s | Submits **PASS** (255/0/0); **exec spread FAIL** (val3 stuck) |
| T2 | Medium | rate=30 / 40s | Partial: 141 ok, **69 tefALREADY**; val3 lagging |
| T3 | High | rate=50 / 40s | **val3 OOM mid-test**; 48 ok, 93 err |
| T4 | Hold + seq seed | rate=25 / 35s | **FAIL** — val3 dead (403 URLError) |
| T5 | Quiet consistency | 45s idle | **Falcon+arch PASS** exec=683 spread=0, seqs AGREE; val3 DOWN |
| T6 | Burst | rate=80 / 20s | N/A — no third val |
| T7 | Final | settle | Falcon mesh solid; droplet gone |

## Results that matter

### What is solid (Falcons + archivals)

- **`admit_exec_lag = 0`** on every healthy node for the whole suite (no packer freeze).
- After load, **val1 = val2 = arch1 = arch2 = 683 exec**, **certified=235**, account sequences **fully agree**.
- Falcon RSS under peak load stayed modest: ~**130–320 MiB per process**, host load ~1 on 16GB.
- Archivals track live history correctly once the packing majority is healthy.

### What failed (droplet / val3)

| Time | val3 RSS (approx) | Host free | Notes |
|------|-------------------|-----------|--------|
| T0 | **670 MiB** | 2.7 GiB | Already hot: **~95% CPU**, exec flat at 281 |
| T1 end | **~1.0 GiB** | 2.3 GiB | Falcons 517 exec, **val3 stuck ~288** (spread 229) |
| T2 end | **~2.4 GiB** | 1.0 GiB | Catch-up to ~457; **tefALREADY** on owner-route |
| T3 mid | **~3.5 GiB** | **89 MiB** | Kernel **OOM-killed** val3 (`dmesg` confirmed) |
| T4+ | process gone | recovered | Connection refused |

Root cause is **not consensus freeze**. It is:

1. **Memory growth under catch-up / open-DAG work** on a 4GB host with no swap.  
2. **CPU saturation** (~95% of one core) so exec lags while peers keep packing.  
3. Lag → more catch-up state → more RAM → OOM (death spiral).  
4. Load client still **owner-routes ~⅓ of submits to val3**, amplifying pressure and producing **`tefALREADY`** when sequences race a lagging/dead owner.

Secondary signals on val3 logs: repeated **`open-body-repool abandoned`** (own batches with 0 foreign UNL sigs) — body/cert propagation under stress needs attention, but the hard stop was OOM.

## Scorecard vs end goal

| Goal | Status |
|------|--------|
| Multi-host UNL (3 vals) | Met until droplet death |
| Val + archival per Falcon | **Strong pass** |
| One val on 4GB droplet under sustained multi-host load | **Fail (OOM)** |
| No packer freezes | **Pass** on healthy nodes |
| Full-history archivals agree with vals | **Pass** |
| Owner-route + multi-host load | Works until owner node falls over |

---

## Recommendations

### 1. Upgrades (hardware / placement) — highest impact

| Priority | Action | Why |
|----------|--------|-----|
| **P0** | **Droplet → ≥8GB RAM / 4 vCPU** (prefer 16GB if it stays a packing validator) | Single val hit **3.5 GiB RSS** and OOM’d on 3.8 GiB. 4GB is below peak footprint under catch-up. |
| **P0** | Add **1–2 GiB swap** on droplet as safety net | Avoids hard kill; still need real RAM for performance. |
| **P1** | Or **move val3 onto host2** (2nd process): f1=val1+arch1, f2=val2+val3+arch2, droplet=optional RPC-only | Falcons have 14+ GiB free; dual process stayed &lt;400 MiB each in this suite. Droplet then becomes non-critical. |
| **P2** | Prefer **same-region / direct Tailscale** paths; avoid relay for val P2P | Reduces body/sig RTT that feeds open-batch backlog. |

### 2. Tuning (config knobs) — do on next bring-up

Current deploy uses aggressive multipack / deep open work. On the **weak host** (or all vals until memory is profiled):

| Knob | Current | Suggested (droplet / constrained) | Intent |
|------|---------|-------------------------------------|--------|
| `--admit-ahead-cap` | 2000 | **256–512** | Cap mempool growth when behind |
| `--max-uncertified-per-creator` | 8 | **4** | Less open-DAG state |
| `--max-batch-txs` | 64 | **32** (droplet) | Smaller batches, less burst RAM |
| `--snapshot-interval` | 15 | **30–60** on weak host | Less IO thrash while catching up |
| `--open-body-repool-after` | 8 | keep 8; **monitor** abandoned counts | Repool storms correlated with lag |
| `--pack-open-cap` | 0 (auto) | try explicit **2–4** if open work balloons | Bound concurrent own opens |
| Profile | full validator | keep full; do **not** put archival on 4GB | Archival is for 16GB Falcons only |

**Falcon-side:** current knobs are fine for lab; no urgent retune.

**UNL threshold 0.6 with N=3** (need 2/3) is correct. After droplet death, Falcons alone still had 2/3 and continued — good for liveness, bad if you expected the third geography to stay critical without monitoring.

### 3. Hardening (ops)

1. **systemd units** per node with:
   - `Restart=on-failure` / `RestartSec=5`
   - `MemoryMax=3G` (droplet) so OOM is controlled before whole host thrashes
   - `LimitNOFILE=65536`
2. **Watchdogs:** alert if `executed_txs` gap vs peer &gt; N for &gt; T seconds; alert RSS &gt; 2.5G; alert process missing.
3. **Load client fixes** (high value for lab honesty):
   - Skip owner if RPC unhealthy or exec lag &gt; threshold (fallback flood / alternate admit).
   - Treat **`tefALREADY`** like past-seq: **resync `account_info.sequence`**, don’t burn the hold window.
   - Parallel async submits (current sequential RPC understates true packer TPS).
4. **Log rotation** on droplet (`val3.log` is fine now; will grow under long soak).
5. **Keys:** already 0600; keep `unl_public` read-only deploy; never ship secrets in git.
6. **Do not** run dual validators on 4GB (already proven fatal).

### 4. Code / product hardening (from suite signals)

| Issue | Evidence | Suggested work |
|-------|----------|----------------|
| Unbounded memory under catch-up | RSS 0.7→3.5 GiB then OOM | Profile open batches, parked certs, foreign_sig maps, conf-pool; add hard caps + metrics |
| open-body-repool abandon storms | Many lines on val3 | Improve body pull priority for own opens; metric `open_body_repool_abandoned` in dig/status |
| Owner-route to lagging node | tefALREADY + stuck exec | Health-aware admit path; or allow non-owner admit with forward-to-owner |
| Archival not a packer but in signers | pack_stall open samples listed `arch1` in signers | Confirm archival sigs are intentional for order-exec; document weight (arch not on UNL threshold) |
| Loadtest pass criteria | flags FAIL while falcons healthy | Report **quorum health** separately from **full-mesh health** |

### 5. Recommended next lab configs

**A. Stable 3-geo lab (honest multi-host)**  
- Droplet **8–16GB**, single val, swap on, systemd MemoryMax.  
- Keep Falcon val+arch.  
- Re-run T1–T4; expect spread &lt;20 and no OOM.

**B. Stable 2-host packing + light edge (cheap)**  
- Packing UNL only on host1+host2 (N=2 needs threshold rethink — better keep N=3 with val3 on host2).  
- Droplet = RPC / faucet only (no pack), tiny RAM.

**C. Performance dig (measure real TPS)**  
- Parallel load client; submit only to local owners; disable cross-TS submit RTT as the bottleneck.  
- Separate “client TPS” from “executed TPS”.

## Bottom line

- **Falcons are good enough** for full **validator + archival** each — proven, lag-free, consistent.  
- **4GB droplet is not enough** for a Falcon-512 packing validator under multi-host catch-up; it will **lag, then OOM**.  
- Consensus path on the healthy majority is healthy; the suite’s red flags are **capacity, memory growth, and client routing**, not freezes.

**Do next:** resize droplet (or relocate val3 to a Falcon) → re-run T1–T4 → then dig memory caps if RSS still climbs past ~1.5–2 GiB at steady state.
