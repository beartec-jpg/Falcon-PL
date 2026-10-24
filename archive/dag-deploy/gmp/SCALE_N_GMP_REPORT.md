# GMP scale-N dig (Phase A with global mempool)

**Verdict:** SCALE_OUT_FAIL: N=6 best 14.66 < N=3 best 21.8 (regression) | med: N3=21.8 N6=14.66
**scale_out_ok:** False

Path: clients → GMP only → in-engine leases → multipack UNL cert/exec.
Equal capacity budgets (not a skew dig).

| N | best exec_tps_min | best phase | med tps | light tps | lag@med | spread@med |
|---|------------------|------------|---------|-----------|---------|------------|
| **3** | **21.8** | med | 21.8 | 15.6 | 0 | 0 |
| **6** | **14.66** | med | 14.66 | 10.16 | 0 | 0 |

## Notes

- Metric is **exec_tps_min** (slowest UNL validator), not max.
- Prior non-GMP digs: N≈3 multipack works; N=6 often collapses (cert/UNL/pack).
- This dig asks whether **GMP front door alone** unlocks N=6 scale-out.

Raw: `/tmp/gmp-scale-20260810-124953/SCALE_N_GMP_REPORT.json`
