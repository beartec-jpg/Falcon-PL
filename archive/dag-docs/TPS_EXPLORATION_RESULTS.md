# Falcon-512 TPS exploration results (Phase 50a)

**Date:** 2026-08-05  
**Host:** DigitalOcean droplet (5-node localhost mesh)  
**Crypto:** full Falcon-512 (UNL + shared faucet wallets) — not HMAC hybrid  

## Summary

| Goal | Result |
|------|--------|
| Min 400 TPS stable | **Not met** |
| Target 600 TPS | **Not met** |
| Stretch 800 TPS | **Not met** |
| Reliable full-apply band | **~250–300 TPS** (single-packer) |
| Unreliable multi-packer peaks | **~320–340 TPS** when it works; frequent stalls |

**Recommended production-style defaults:** `single_packer=true`, `max_batch_txs=256`, `unl_threshold=0.6`, optional `pack_skip_hole_after=0.5`. Do **not** enable free multi-packer or aggressive intervals for reliability.

## Robustness suite (75 runs)

Source: `docs/TPS_ROBUST_SUITE_SUMMARY.json`  
Method: 15 configs × 5 reps; **full apply** = ≥95% executed **and** agreement; **score** = mean full TPS × success rate.

| Rank | Config | Rate | Mean full TPS | Notes |
|------|--------|------|---------------|--------|
| 1 | SP_A20 single-packer 20 accounts | **100%** | **248** | **Most robust** |
| 2 | SP_A1 single-packer 1 account | **80%** | **282** | Fastest among ≥80% |
| 3 | SP_A10_B512 batch 512 | **80%** | **261** | Low variance |

Multi-packer multi-account variants: **0%** full-apply in this suite. Aggressive interval: **0%** full-apply (partial apply inflates raw TPS).

## Knob sweep (13 experiments)

Source: `docs/TPS_KNOB_SWEEP_SUMMARY.json`  
One-knob-at-a-time Falcon floods (NTX=200). Prefer **full_tps** (complete apply) over raw settle_tps.

Notable: batch size alone (512/1024) little gain; UNL 0.51 and free multi-packer often broke mesh; aggressive interval inflated TPS with incomplete apply.

## Code / harness shipped with this commit

- Account-sharded multi-packer (`owns_tx` when `single_packer=false`)
- CLI knobs: `--max-uncertified-per-creator`, `--single-packer`, interval min/max, adaptive mempool
- `CRYPTO=falcon-512` + shared keys/wallets in `multinode_metrics.sh`
- `ACCOUNTS=N` multi-account flood
- `scripts/tps_knob_sweep.py`, `scripts/robust_tps_suite.py`, `scripts/explore_tps.py`
- RESULTS_JSON machine-readable harness output

## How to re-run

```bash
# Falcon baseline flood
FLOOD=1 NTX=200 CRYPTO=falcon-512 PACK_SKIP_HOLE_AFTER=0.5 \
  ./scripts/multinode_metrics.sh

# Knob sweep
CRYPTO=falcon-512 python3 scripts/tps_knob_sweep.py

# Robustness matrix
REPS=5 python3 scripts/robust_tps_suite.py
```
