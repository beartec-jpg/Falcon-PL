# Pack workers + UNL-only cert dig

**Verdict:** C_REGRESS: 20.21 < A 26.33 | B_REGRESS: 20.05 < A 26.33

| Mode | UNL role | Packers | exec_tps_min | spread | lag | gmp_ok | stable |
|------|----------|---------|--------------|--------|-----|--------|--------|
| A classic | pack+cert | 3 (UNL) | **26.33** | 0 | 0 | 1500 | True |
| B mixed | pack+cert | 6 (UNL+pack) | **20.05** | 0 | 0 | 1500 | True |
| **C pure** | **cert only** | **3 (pack*)** | **20.21** | 0 | 0 | 1500 | True |

A batches: `{'val1': 37, 'val2': 38, 'val3': 34}`
B batches: `{'val1': 23, 'val2': 23, 'val3': 5, 'pack1': 33, 'pack2': 6, 'pack3': 8}`
C batches: `{'pack1': 40, 'pack2': 19, 'pack3': 12}`

Mode C: UNL never packs and does not lease from GMP; only pack1–3 pack; UNL only signs/executes.
