# TPS experiment series (2026-08-07)

**Artifacts:** `/tmp/real-mesh-tps-exp-20260807-174016/`  
**Topology:** 6 validators + 2 archivals on host1+host2 LAN, Falcon-512, UNL threshold 0.6  

## Configs

| Exp | Knobs | Accounts |
|-----|-------|----------|
| **exp1 baseline** | multipack, adaptive on, admit=2000, uncert=8, batch=64, open=0, slot=0.25 | 40 |
| **exp2 density** | multipack, **adaptive off**, same pack knobs | **100** |
| **exp3 single-packer** | **single-packer=true**, adaptive off, batch=256 | 100 |

## Head-to-head

| Metric | exp1 baseline | exp2 density | exp3 single-packer |
|--------|---------------|--------------|--------------------|
| Full-mesh pass (spread=0) | **~8.6 TPS** @ 10/s | **~8.7 TPS** @ 10/s | **~8.7 TPS** @ 10/s |
| Best exec_tps_min | 13.9 @ 25/s (spread 63) | 13.1 @ 25/s (spread 36) | 8.7 @ 10/s |
| Batch rate @ best | ~2.1 /s | ~2.4 /s | ~2.0 /s (val1 only) |
| Density @ best (txs/cert) | ~7.9 | ~6.1 | ~4.4 → **114** under stall |
| Agreement under load | skews | skews less early | **perfect** (spread 0 always) |
| Stop reason | error_collapse @ 80/s | plateau | plateau + busy |

## Phase detail (exec_tps_min / pass)

| Phase | exp1 | exp2 | exp3 |
|-------|------|------|------|
| 10/s | **8.6 ✓** | **8.7 ✓** | **8.7 ✓** |
| 25/s | 13.9 ✗ spread | 13.1 ✗ spread | 1.0 ✗ |
| 50/s | 2.1 ✗ | 6.0 ✗ | 3.3 ✗ |
| 80/s | 0 ✗ + errors | 2.0 ✗ | 0 ✗ + busy |

## Takeaways

1. **More accounts + adaptive off did not raise the multi-host multipack ceiling.**  
   Density stayed ~6–8 txs/cert at the useful band; pack rate still collapses above ~20 submit/s.

2. **Single-packer multi-host keeps perfect agreement** (roots agree, spread 0) but **throughput is worse** with owner-routed multi-node admits:  
   - Only val1 creates batches (confirmed).  
   - Pack rate falls to ~0.15 → 0.03 batch/s under load.  
   - Mempool balloons (~600) while `uncert=1` — open pipeline full, packing stalled waiting on cert/body path.  
   - When rare packs do land, density spikes (tens–100+ txs/cert) — **fill works when packs wait; cadence is the killer.**

3. **Tonight’s baseline is softer than earlier ~22 TPS full-mesh pass.**  
   Same knobs, same hosts: full-mesh band stuck at ~9 TPS; 25/s already skews. Likely variance (Wi‑Fi LAN, residual host load) plus the fact that “min exec TPS with spread=0” is a hard bar.

4. **Primary limiter confirmed:** certified batch rate (~2–4/s network) × thin batches (~2–8 txs) under healthy load. Raising accounts alone does not keep mempool fat *and* cert path healthy at the same time under multipack.

## What to try next (code / harness)

1. **Submit all load to primary packer** in single-packer mode (eliminate cross-node mempool handoff tax).  
2. **Min-fill pack gate** (don’t open a batch under N ready owned txs).  
3. **Instrument open→cert RTT** per batch in status.  
4. **Body-DA priority** so open_cap pipeline can safely go to 2 without skew.  
5. **Wire-only / ethernet path** if hosts are on Wi‑Fi (this lab uses `wlp2s0`).

## Reproduce

```bash
cd ~/falcon-dag/deploy/real-mesh
bash ./07-run-tps-experiments.sh
```
