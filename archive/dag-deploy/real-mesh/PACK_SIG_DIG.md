# Pack / foreign-sig dig (after certify thrash fix)

## Root causes of PACK_BOUND (N=6)

### 1. Multipack open depth is **not** the main cliff

`pack_open_cap` dig (N=6, 50/s × 25s, multipack):

| cap | exec_tps_min | batches (val1) | spread |
|-----|--------------|----------------|--------|
| 0 (auto→1) | 6.5 | 5 | 184 |
| **1** | **7.2** | 5 | 122 |
| 2 | 6.5 | 5 | 103 |
| 4 | 7.6 | 7 | **324** (worse agreement) |

Raising open cap barely moves min TPS; cap=4 worsens spread.  
**Default multipack open_cap stays 1.**

### 2. What actually limits pack rate

From code + counters:

| Factor | Effect |
|--------|--------|
| **Conf-pool Pack/Check duty** (`pack_check_slot` 0.25s) | Multipack only packs half the time; other half is sign/check |
| **Need 4-of-6 Falcon sigs** | Each open waits long for foreign weight |
| **Parent-certified barrier** | Opens cannot complete cert until parent frontier catches up |
| **Falcon-512 sign** on pack | CPU-heavy; co-located 3 vals/host |
| **foreign_sig merge** | High ingest_merge / foreign_sig vs few certs |

So PACK_BOUND at N=6 is **pipeline + crypto + quorum width**, not “forgot to raise open_cap.”

### 3. Foreign-sig path

- Verify already off-lock via `spawn_blocking` (good).
- **Fix landed:** drop invalid sigs before `on_remote_sig` (was ignoring verify result and re-entering engine anyway).
- Valid sigs still cache-hit re-check under lock (cheap).

---

## Code changes this dig

| Change | File | Intent |
|--------|------|--------|
| Drop failed off-lock Falcon verifies | `main.rs` handle_inbound | Cut SIG thrash on bad/garbage sigs |
| Keep multipack `pack_open_cap` auto = **1** | `engine.rs` | Dig showed 2/4 don’t fix N=6; 4 hurts spread |
| `PACK_OPEN_CAP` env on scale harness | `07-scale-n-curve.py` | Experiment knob |
| Dig script | `10-pack-open-cap-dig.py` | Re-run open-cap matrix |

---

## What will *not* fix scale-out alone

- Higher `pack_open_cap`
- More admit-ahead
- More try_certify spinning (already fixed)

## What might (next engineering)

1. **Pack pulse / narrower multipack** so parent frontier stays short (engine already has pulse sketches).  
2. **Faster cert return path** (creator-directed cert already partial; measure foreign-sig→cert latency).  
3. **Cheaper quorum** or hierarchical UNL for large N.  
4. **1 val per machine** for N=6 geo dig (separate co-location tax).  
5. Stay on **N=3 multipack** for product until Phase A turns up.

---

## Artifacts

- Open-cap dig: `/tmp/pack-open-dig-20260810-100126/`
- Prior N=6 profile: `/tmp/continue-post-certfix-20260810-095228/`
