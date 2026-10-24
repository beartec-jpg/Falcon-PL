# Continue report — profile N=6 + N=3 steady

OUT: `/tmp/continue-post-certfix-20260810-095228`
LAN F1=192.0.2.1 F2=192.0.2.2

## A) N=6 multipack bottleneck profile

- load: {'rate': 50, 'hold': 30.0, 'workers': 16}
- submits ok/busy/err: {'ok': 1400, 'busy': 0, 'err': 0}
- **exec_tps_min: 6.08** (delta min 232)
- CPU mean by id: `{'val1': 52.4, 'val2': 52.3, 'val3': 53.1, 'arch1': 44.0, 'val4': 53.8, 'val5': 54.1, 'val6': 56.5, 'arch2': 44.9}`
- RSS total MiB f1/f2: 1811.4 / 277.6
- health after: `{'exec_min': 232, 'exec_max': 368, 'exec_spread': 136, 'lag_max': 48, 'vals_up': 6, 'roots_agree': False, 'arch1_exec': 285, 'arch2_exec': 232}`

### Pipeline deltas (selected)

- **val1**: admits=245 batches=4 cert=21 try_cert=0/0 foreign_sig=99 exec=368
- **val2**: admits=245 batches=4 cert=21 try_cert=0/0 foreign_sig=99 exec=368
- **val3**: admits=245 batches=5 cert=21 try_cert=0/0 foreign_sig=100 exec=368
- **val4**: admits=175 batches=3 cert=18 try_cert=0/0 foreign_sig=81 exec=232
- **val5**: admits=210 batches=4 cert=18 try_cert=0/0 foreign_sig=81 exec=232
- **val6**: admits=280 batches=4 cert=18 try_cert=0/0 foreign_sig=83 exec=232

### Hints

- PACK_BOUND: few batches_created vs admits — packing/signing not keeping up with admit.
- SIG_GOSSIP_HEAVY: foreign_sig_merged much larger than certs — sig merge work high.
- EXEC_LAGGING_ADMITS: executed min delta << successful submits (pipeline backlog).
- HOTTEST_PROC: val6 avg 56.5% CPU during load.

## B) N=3 multipack steady / multi-client

| mode | submit_tps | exec_tps_min | spread | lag | tight/pass |
|------|------------|--------------|--------|-----|------------|
| single owner-router client @40 | 37.01 | 37.01 | 0 | 0 | True |
| multi-client local (15/val) | 41.43 | 41.43 | 0 | 0 | True |
| soak 60s @20 | 19.31 | 18.12 | 44 | 0 | True |

Final health: `{'exec_min': 3325, 'exec_max': 3325, 'exec_spread': 0, 'lag_max': 0, 'vals_up': 3, 'roots_agree': True, 'arch1_exec': 3325, 'arch2_exec': 3325}`

## What this means next

- **Code focus: pack/sign rate** vs admit (Falcon-512 pack loop).
- **Code focus: foreign sig merge / P2P pull efficiency.**
- Keep **ops default N=3 multipack** until Phase A curve turns upward.
- Multi-client should be the default load model for user-growth tests.

## Mesh left running
- **N=3 multipack** intentionally left up after section B for continued use.

