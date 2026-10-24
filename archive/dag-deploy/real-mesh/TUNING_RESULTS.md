# Tuning experiments (6v+2a LAN)

Same ramp harness: `06-measure-ramp.py`

| Tag | Knobs | Best full-mesh pass | Best exec_tps_min | Notes |
|-----|-------|---------------------|-------------------|--------|
| **v1** | admit=2000, uncert=8, batch=64, open=0 | **r02 @ 25/s** | **22.4** | **Winner** — spread=0 through r02 |
| v2 | admit=512, uncert=4, batch=128, open=2 | r01 @ 10/s only | 10.8 | Too tight on open/uncert; earlier host skew |
| v3 | admit=1000, uncert=8, batch=128, open=0 | r01 @ 10/s only | 11.9 | Larger batches alone not enough; full ramp to wall |

## Phase comparison (exec_tps_min)

| Phase | v1 | v2 | v3 |
|-------|----|----|-----|
| r01_warm (10) | 8.8 ✓ | 8.6 ✓ | 8.6 ✓ |
| r02_light (25) | **22.4 ✓** | 10.8 ✗ | 11.9 ✗ |
| r03_med (50) | 9.9 ✗ | 1.9 ✗ | 1.4 ✗ |
| r04_mid (80) | 0 ✗ | 0.7 ✗ | 2.2 ✗ |

## Extreme load (v3 only went full ramp)

- Submit still peels off ~20–28 ok/s until **busy** dominates (r07+)
- **terBUSY** appears under crushing target rates (admit path finally backpressures)
- Brief **lag=55** at r06 then recovered (not sticky freeze)
- **error_collapse** only at target 700/s
- RSS ballooned **~6 GiB / host** after full ramp — memory still the long-run risk

## Conclusion

1. **Do not ship v2-style tight uncert/open** for multipack 6-val — hurts more than helps.  
2. **Larger max-batch-txs (128)** did not raise the tight-mesh ceiling vs v1.  
3. **Keep v1 knobs** for best multi-host agreement (~20–22 exec TPS).  
4. Ceiling is **not** fixed by admit-ahead alone — pack/cert/cross-host path needs code-level work.  
5. Defaults in `env.sh` restored to **v1-best-measured**.

## Artifacts

- v1: `/tmp/real-mesh-measure-20260807-140126/`  
- v2: `/tmp/real-mesh-measure-v2-20260807-144305/`  
- v3: `/tmp/real-mesh-measure-v3-20260807-144616/`  
