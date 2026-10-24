# Shard v0 dig (parallel independent chains)

**Verdict:** SHARD_V0_HELPS: dual sum 46.38 > single 38.64 (>+15%)

No cross-shard bridge — only proves **parallel shards add capacity**.

| Mode | exec_tps_min | notes |
|------|--------------|-------|
| A single N=3 | **38.64** | all accounts, rate 80/s |
| B shard0 | **25.39** | acct1-20, rate 40/s |
| B shard1 | **20.99** | acct21-40, rate 40/s |
| **B sum** | **46.38** | capacity if load is shard-local |

A health={'vals_up': 3, 'exec_min': 2049, 'exec_max': 2049, 'exec_spread': 0, 'lag_max': 0, 'roots_agree': True} gmp_ok=2800
B0 health={'vals_up': 3, 'exec_min': 1346, 'exec_max': 1346, 'exec_spread': 0, 'lag_max': 0, 'roots_agree': True} B1 health={'vals_up': 3, 'exec_min': 1113, 'exec_max': 1113, 'exec_spread': 0, 'lag_max': 0, 'roots_agree': True}

Open validators: see docs/OPEN_VALIDATORS_AND_SHARDS.md (risks + mitigations).
