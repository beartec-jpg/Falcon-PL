# pack_open_cap dig (N=6 multipack, 50/s × 25s)

OUT: `/tmp/pack-open-dig-20260810-100126`

| cap | exec_tps_min | submit_tps | spread | batches(val1) | tight |
|-----|--------------|------------|--------|---------------|-------|
| 0 | **6.52** | 39.73 | 184 | 5 | False |
| 1 | **7.2** | 35.3 | 122 | 5 | False |
| 2 | **6.51** | 36.16 | 103 | 5 | False |
| 4 | **7.59** | 36.95 | 324 | 7 | False |

**Best:** cap=4 exec_tps_min=7.59

Note: cap=0 uses binary auto multipack open depth (code default now 2).

