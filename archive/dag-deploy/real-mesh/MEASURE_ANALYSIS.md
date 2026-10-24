# 6v+2a component measurement + load ramp

**Suite:** `/tmp/real-mesh-measure-20260807-140126`  
**Topology:** host1 = val1–3 + arch1 · host2 = val4–6 + arch2 · LAN · net 9210 · falcon-512  
**Method:** baseline → function probes → stepped ramp (parallel owner-routed submits) until exec TPS plateau  

---

## 1. What we measured (by component)

| Component | How measured | Healthy signal |
|-----------|--------------|----------------|
| **RPC / host path** | server_info RTT, submit once per owner | low ms, tesSUCCESS |
| **Admit** | `local_admits_ok` delta vs submits | admits ≈ owner submits, 0 err |
| **Mempool** | `mempool` depth after phase | drain to ~0 under sustainable load |
| **Pack / DA** | `batches_created` delta, `own_open_n` | batches grow with load; open drains |
| **Cert / UNL** | `certified` delta | equal across vals when in sync |
| **Order (Bullshark)** | `ordered`, `ordered_lag`, `max_cert_round` | ordered_lag=0 |
| **Execute** | `executed_txs` delta / spread | min exec TPS rises with load |
| **Archival** | `runs_da=false`, `batches_created=0`, exec track | no pack; exec near vals |
| **P2P** | `p2p_peers` | 14 (full mesh of 8) |

---

## 2. Function probes (idle / micro)

| Probe | Result |
|-------|--------|
| val1 server_info RTT | **~4 ms** p50 (local) |
| val4 server_info RTT | **~17 ms** p50 (cross-LAN) |
| arch1 RTT | **~6 ms** |
| Submit once (local vals) | **~2–3 ms**, tesSUCCESS |
| Submit once (f2 vals) | **~10–18 ms**, tesSUCCESS |
| Post-probe agreement | all 8 nodes **exec=483**, lag=0, roots agree |

**Working well:** RPC, auto-sign, owner-route admit, local vs remote latency as expected.

---

## 3. Load ramp (where it goes / stops)

| Phase | target/s | workers | ok | err | submit_tps | **exec_tps_min** | spread | lag | pass |
|-------|----------|---------|----|-----|------------|------------------|--------|-----|------|
| r01_warm | 10 | 4 | 150 | 0 | 8.8 | **8.8** | 0 | 0 | **YES** |
| r02_light | 25 | 8 | 500 | 0 | 22.6 | **22.4** | 0 | 0 | **YES** |
| r03_med | 50 | 12 | 900 | 0 | 29.8 | **9.9** | 127 | 0 | no (arch lag / spread) |
| r04_mid | 80 | 16 | 560 | 0 | 16.8 | **0** | 159 | 0 | no |

**Stop reason:** `sustained_exec_tps_plateau` (raising target no longer raises global exec; not a freeze).

### Sweet spot

- **Best full-mesh pass:** **~22 exec TPS** at target 25/s  
- **Zero errors** entire ramp  
- **`admit_exec_lag` always 0** — packer front-door never “stuck” in freeze sense  

### Ceiling behavior (not freeze)

Past ~25/s target:

1. **Submit path** tops out around **~30 ok/s** (workers help until RPC/sign/path saturates).  
2. **Exec diverges by host:** host1 vals pull ahead of host2 (e.g. 1727 vs 1568 mid-suite).  
3. **Cert production collapses** under backlog (r04 cert Δ ≈ 0–1 while admits still +70–100/val).  
4. **Mempool builds** (0 → 4 → 77 → **163**).  
5. **Uncertified opens stick** (~3–4), `own_open_n≈1`.  
6. **No hard stall** of admits; chain **progress slows / skews**, then recovers slowly when load drops.

So: **load can increase (admits), but global executed progress does not** — classic pipeline back-pressure, not deadlock.

---

## 4. Component scorecard

### Admit (working well)

- 0 `terBUSY` / 0 hard engine errors through ramp.  
- Owner distribution rough but all 6 lanes used.  
- Can **outpace** pack/cert (by design of aggressive admit-ahead).

**Improve:** lower `admit-ahead-cap` under pressure; health-aware backpressure when `uncertified` or mempool high.

### Pack / DA (OK then weak)

| Phase | batches_created Δ (vals) |
|-------|---------------------------|
| r01 | 8–14 each |
| r02 | 11–16 |
| r03 | **3–6** (admits hundreds) |
| r04 | **0–1** |

Pack rate does **not** scale with admit rate past the sweet spot → primary throughput limit for multipack + Falcon-512 signing.

**Improve:** profile pack loop / Falcon sign cost; tune `max-batch-txs`, batch interval, `max-uncertified-per-creator`, multipack open cap.

### Cert / UNL (good then cliffs)

- r01–r02: **equal cert Δ** across all 6 vals (66, 82).  
- r03: slight f2 lag (21–24).  
- r04: **almost no new certs**.

**Improve:** body/sig propagation priority when open work ages; reduce open set so cert threshold reachable faster.

### Order + execute (good at low load)

- `ordered_lag=0` throughout.  
- At sustainable load: perfect exec equality.  
- Under pressure: **host skew** (f1 faster than f2), state roots diverge until catch-up.

**Improve:** ensure cross-host pull path keeps f2 fed; avoid 3 heavy vals + arch all contending on one box without QoS.

### Archival (profile correct; tracking under stress)

- `batches_created=0`, `runs_da=false`, `runs_ledger=true` — **full-history role correct**.  
- Perfect track through r02; under r03+ **trails host leaders** (arch2 with f2).  
- Does not pack (good).

**Improve:** same as order/exec catch-up; optional prioritise archival pull of certified tips.

### P2P (solid)

- **14 peers** every node for entire suite — mesh stayed up.

### Memory / CPU (room for improvement — important)

After ramp (approx RSS):

| Host | Processes | Notes |
|------|-----------|--------|
| host1 | vals **~1.3–1.6 GiB each**, arch **~1.6 GiB**, CPU ~40–60% | **~6 GiB+** process total under backlog |
| host2 | vals **~0.6–0.8 GiB**, arch **~0.5 GiB**, CPU **~78%** vals | CPU hotter, still behind on exec |

**6 vals + 2 arch under overload is heavy.** 16GB hosts survive but RSS growth under uncertified/mempool backlog is the same class of bug as droplet OOM — just more runway.

---

## 5. What’s working well (summary)

1. Multi-host **LAN** 6-val UNL + 2 archivals **boots clean** and meshes fully.  
2. **Sustainable band ~10–22 exec TPS** with perfect agreement, lag=0, arch match.  
3. **Admit path + Falcon wallet auto-sign** rock solid (0 submit errors).  
4. **No packer freezes** under this ramp.  
5. Archival **does not pack** and follows when the network is healthy.  
6. Cross-host RPC ~4× local — fine for LAN lab.

## 6. Room for improvement (priority)

| P | Item | Why |
|---|------|-----|
| **P0** | Bound memory under open/uncertified backlog | RSS multi-GiB per process under pressure |
| **P0** | Pack/cert path must scale past ~20 TPS or intentionally rate-limit admits | Admits outrun certs → skew |
| **P1** | Cross-host DA fairness (f2 lags f1 under load) | Split brain-ish exec until catch-up |
| **P1** | Admit backpressure from mempool/uncert depth | Prevents useless mempool fill |
| **P2** | Faster catch-up / settle metrics in status | ops |
| **P2** | Parallel load client is good; keep for benches | sequential RPC understates capacity |

### Tuning knobs to try next (lab)

- `--admit-ahead-cap 256–512` (from 2000)  
- `--max-uncertified-per-creator 4`  
- `--max-batch-txs 32–128` A/B (sign cost vs amortisation)  
- Slightly slower admit under `mempool > N`  
- Optional: only 2–3 packers active for isolation digs  

### Architecture digs

1. **Sign CPU** on Falcon-512 pack path (where r03 batch rate dies).  
2. **Open-batch / conf-pool retention** (memory).  
3. **Pull scheduler** under multipack (why f2 trails).  

---

## 7. How far we can go (plain answer)

| Regime | Behavior |
|--------|----------|
| **0–~22 exec TPS** | Full mesh healthy, archivals in lockstep, lag=0 |
| **~25–30 submit TPS** | Client/admit near max on this client design |
| **Above that** | Admits still succeed, **executed global progress plateaus / skews**, mempool & uncert grow, **no freeze** |
| **Hard crash** | Not hit on 16GB in this suite; memory trend is the long-run risk |

**Break point for “good multi-host agreement” ≈ low-20s exec TPS.**  
**Break point for “can keep dumping txs”** is higher (admits keep working) but that is **not** the same as the network staying tight.

---

## Artifacts

- `/tmp/real-mesh-measure-20260807-140126/REPORT.json`  
- `phase_r0*.json`, `baseline.json`, `probes.json`  
- Script: `deploy/real-mesh/06-measure-ramp.py`  
