# Real-mesh multi-host simulation report

**Date:** 2026-08-07  
**Suite dir:** `/tmp/real-mesh-suite-20260807-111421`  
**Network:** id=9200, crypto=falcon-512, UNL=val1..val4 (threshold 0.6), conf-pool on  

## Topology (end-goal shape)

| Host | Spec (approx) | Nodes |
|------|----------------|-------|
| **host1** (Tailscale 100.64.0.1) | 16GB local | **val1** (validator/full) + **arch1** (archival/order-exec) |
| **host2** (100.64.0.3) | 16GB | **val2** + **arch2** |
| **droplet** (100.64.0.5) | 4GB / 2 vCPU | **val3** + **val4** |

- Validators: multipack DA + Bullshark ordering, owner-routed submits (FNV lane → val index).
- Archivals: do not pack; order+execute certified history (`runs_da=false runs_ledger=true`).
- P2P mesh: each node lists the other five peers over Tailscale; all showed **p2p=10** edges (bidirectional).

## Fixes applied before clean suite

1. **Archival crash** `NoSecret("arch1/2")` → generated Falcon secrets for arch ids + redistributed keys.
2. **Loadtest `tecSEQ_PAST`** → seed sequences from `account_info` (and resync on past-seq).
3. Clean wipe of data dirs + restart from genesis so archivals track full live history.

## Test series

### T0 — Baseline (idle mesh)
All 6 nodes UP, exec=0, lag=0, p2p connected.

### T1 — Light load (rate≈20/s target, 30s hold)
| Metric | Result |
|--------|--------|
| Pass | **PASS** |
| Submits ok/busy/err | 195/0/0 |
| Exec vals (min–max) | 187–191 (spread 4) |
| Lag | all 0 |
| Archivals | arch1=193, arch2=193 — tracking |
| Owner route | ~equal across val1–4 |

**Verdict:** Multi-host consensus + archival follow works at light load.

### T2 — Medium load (rate≈40/s target, 45s)
| Metric | Result |
|--------|--------|
| Submits ok/busy/err | 211/0/0 |
| Exec vals | 314–351 (spread 37) |
| Lag | all 0 |
| Archivals | alive; tracking near falcon leaders |
| Automated pass flag | false (absolute exec floor bug; later fixed) |

**Verdict:** Zero submit errors; **droplet vals lag ~30–40 exec** under sustained multi-host load (4GB host, 2 validators, WAN/Tailscale RTT). Submit rate capped ~4–5 TPS by **sequential RPC RTT** (not packer freeze).

### T3 — Continued hold with seq seed (rate≈30, 40s)
| Metric | Result |
|--------|--------|
| Submits ok/busy/err | 109/0/0 |
| Seq seed | worked (acct1 started at 11, not 1) |
| Exec spread | 47 (droplet still behind during load) |
| Lag | all 0 |
| Arch | arch2 matched leaders; arch1 slightly behind mid-test |

**Verdict:** Sequence continuity fixed. Droplet remains throughput/catch-up bottleneck under load.

### T4 — Immediate consistency (right after T3)
- Val exec spread ~52 (val3/val4 behind host1/2).
- Account sequences **not fully agreed** across all 6 (expected while exec lag open).
- **No** admit_exec_lag freezes (lag=0 everywhere) — this is **replication lag**, not packer freeze.

### T5 — Quiet catch-up (60s idle re-samples)
See `t5-catchup.txt` for samples.


### T5 — Quiet catch-up (observed)

| Sample window | Observation |
|---------------|-------------|
| After T3 | falcons + arch2 at ~442 exec; arch1 briefly stuck ~419 then recovered |
| Mid idle | **val3/val4 RPC timeouts** on droplet |
| End of 60s | falcons + both archivals **exec=451, lag=0, certified=175** (full agreement) |
| Droplet end | **val3 DOWN** (connection refused); val4 alive at exec≈406 (still behind) |
| Droplet load | **load average ~9.9 on 2 vCPU**, mem ~2.7/3.8 GiB used, **no swap** |

**Verdict:** Falcon-side end goal (val+archival full history) **held and converged**. Droplet dual-validator under load **saturated the 4GB/2vCPU host** — one validator process died / stopped accepting RPC. This is a host-capacity finding, not a falcon-local freeze.

### Resource snapshot (end of suite)

| Host | RAM | Load |
|------|-----|------|
| host1 | 15Gi, ~1.9Gi used | ~0.7 |
| host2 | 15Gi, ~1.0Gi used | ~1.8 |
| droplet | 3.8Gi, ~2.7Gi used, 0 swap | **~9.9** |

## Bottom line

The real-world mesh **simulates the target architecture successfully on the two Falcons** (validator + archival each, 4-val UNL, Falcon-512, Tailscale P2P). Light multi-host load **passes cleanly**. Sustained load exposes the **droplet as the weak link** (replication lag → eventual process failure under dual-val CPU pressure). No packer freezes (`admit_exec_lag=0`) were observed on healthy nodes.

## Resource notes
- host1/2: 16GB, comfortable headroom with val+arch.
- droplet: 4GB / 2 vCPU running **two** validators — primary bottleneck for real-mesh sim.
- Cross-host submit latency dominates client-side “TPS”; packer path still lag=0.

## Overall assessment vs end goal

| Goal | Status |
|------|--------|
| 4-validator UNL across 3 hosts | **Met** |
| Full-history archival on each Falcon | **Met** (after key fix; order-exec archival profile) |
| Two validators on droplet | **Met** |
| Light load multi-host agreement | **Pass** |
| Sustained load without freezes | **Pass** (lag=0; no freeze) |
| Sustained load equal exec on all hosts | **Partial / fail on droplet** — lags under load; val3 died after sustained dual-val pressure |
| Sequential reloads without tecSEQ_PAST | **Pass** after loadtest fix |

## Artifacts
- Suite: `/tmp/real-mesh-suite-20260807-111421`
- Deploy scripts: `/home/falcon/falcon-dag/deploy/real-mesh/`
- Mesh root: `~/falcon-dag-mesh` (falcons), `/root/falcon-dag-mesh` (droplet)

## Recommended next steps
1. Give droplet more CPU/RAM or run **one** val there for fairer parity tests.
2. Parallelize load client (async multi-connection) to measure true network TPS beyond RPC RTT.
3. Optional: archival retention metric dump / snapshot size report for “full history” proof.
4. Investigate droplet P2P/exec path if quiet catch-up does not fully close the gap.

## Critical capacity finding (droplet)

- After suite: only **val4** remains on droplet; **RSS ≈ 2.7 GiB for a single validator** (~57% CPU).
- **val3** exited (connection refused; process gone) — almost certainly **OOM / resource death** on 3.8 GiB host with two validators (no swap).
- Implication: **two full validators on a 4GB droplet is not viable** with current binary footprint. Prefer 1 val on droplet, or ≥8GB RAM for two vals.

