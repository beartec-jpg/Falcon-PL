# Fat-batch amortization dig (N=3 + GMP)

**Verdict:** FAT_BATCH_FLAT: best mb16_iv0.05 tps=40.2 ≈ baseline64 37.89

Load: 80/s × 35.0s + 18.0s drain · multipack N=3 · clients→GMP only

| max_batch | interval | exec_tps_min | batch/s | cert/s | txs/batch≈ | exec/cert≈ | lag | stable |
|-----------|----------|--------------|---------|--------|------------|------------|-----|--------|
| 16 | 0.05 | **40.2** | 0.974 | 2.921 | 41.28 | 13.76 | 0 | True |
| 64 | 0.05 | **37.89** | 0.836 | 2.507 | 45.34 | 15.11 | 0 | True |
| 128 | 0.05 | **36.46** | 0.729 | 2.186 | 50.04 | 16.68 | 0 | True |
| 256 | 0.05 | **38.36** | 0.641 | 1.923 | 59.85 | 19.95 | 0 | True |
| 256 | 0.12 | **38.29** | 0.857 | 2.57 | 44.69 | 14.9 | 0 | True |
| 512 | 0.12 | **36.84** | 0.886 | 2.657 | 41.6 | 13.87 | 0 | True |
| 512 | 0.2 | **39.55** | 0.76 | 2.28 | 52.04 | 17.35 | 0 | True |

## How to read

- **exec_tps_min** — north-star (slowest UNL val)
- **cert/s** — agreement load; should drop if amortization works
- **txs/batch≈** — how much work each pack unit carries
- **exec/cert≈** — work covered per certificate (amortization ratio)

If fat cells raise exec/cert and tps while cert/s falls → theory confirmed.

Amortization span: {"lowest_txs_per_batch": 41.28, "highest_txs_per_batch": 59.85, "tps_at_lowest": 40.2, "tps_at_highest": 38.36, "certs_s_at_lowest": 2.921, "certs_s_at_highest": 1.923}
