# Falcon DAG — Network Economics & Bootstrap

**Falcon (FALC)** — A decentralized, post-quantum consensus network.

---

## Executive Summary

Falcon DAG is a production-grade consensus system with:
- **Post-quantum cryptography** (Falcon-512 signatures)
- **263+ TPS throughput** (263 TPS Phase 49 baseline, targeting 600+ with Phase 50 optimizations)
- **Sub-400ms finality** (335ms Phase 49 measured)
- **Decentralized validator participation** (adaptive UNL rotation, community-driven)
- **8-week bootstrap airdrop** (1% of supply to early participants)
- **Sustainable long-term economics** (CID emissions + fee replenishment)

**Ticker:** FALC  
**Total supply:** 200 billion FALC

---

## Part 1: Supply & Treasury

### Total Supply

| Allocation | Amount | % |
|------------|--------|---|
| **Genesis allocation** | 4 billion FALC | 2% |
| — Community airdrop | 2 billion FALC | 1% |
| — Public faucet | 1 billion FALC | 0.5% |
| — Builder/dev pot | 1 billion FALC | 0.5% |
| **Protocol treasury** | 196 billion FALC | 98% |

### Why 98% to treasury?

- Prevents premine — no founder whale allocation
- Validators earn by building the network
- Decentralizes over time via CID emission schedule
- Sustainable long-term: fees replenish treasury after emissions floor

### Treasury controls

- Protocol-controlled only; no human discretion
- Emitted via CID schedule (weekly, declining rate)
- Replenished continuously by transaction fees

---

## Part 2: Node Types

The network has two distinct node roles. Both are shipped in the same binary with different startup flags.

### Validator nodes

Participate in Bullshark DAG consensus. Required for network security.

```bash
falcon-dagd --validator \
  --keys /path/to/validator_key.json \
  --network mainnet \
  --region us-east-1
```

Responsibilities:
- Sign batches and certificates
- Participate in weekly hard-close scoring
- Vote on governance amendments (UNL members only)
- Earn rewards from CID emissions (proportional to score)

### Full nodes

Sync ledger state and serve RPC. Required for network accessibility.

```bash
falcon-dagd --full-node --network mainnet
```

Responsibilities:
- Download and verify every batch
- Store full DAG history
- Serve RPC queries (clients, wallets, dApps)
- Help new validators sync
- Earn rewards from fee split (proportional to score)

### Do operators need to run both?

No. Operators choose their role:
- **Validators** focus on consensus, earn emission rewards.
- **Full node operators** focus on availability, earn fee rebates.
- Advanced operators may run both on the same machine; they earn both reward streams.

---

## Part 3: UNL (Unique Node List) Architecture

### Bootstrap phase (epochs 0–7)

| Parameter | Value |
|-----------|-------|
| UNL size | 30 validators total |
| Trusted pool | 15 (dev team + known early operators) |
| Public pool | 15 (best-performing open participants) |
| Rotation | Disabled during bootstrap (proves system stability first) |

### Production phase (epochs 9+)

**Dual-pool model:**

```
UNL = 30 validators
  Trusted pool (15):  dev team + vetted operators
  Public pool (15):   top performers from open bench
  
Bench: all validators NOT in UNL, earning scores, waiting for rotation
```

**Amendment voting — dual approval required:**

```
To pass an amendment:
  trusted_approval  = 8 of 15 trusted validators vote YES
  public_approval   = 8 of 15 public validators vote YES
  Both must pass simultaneously.

This prevents:
  - A single compromised pool from passing malicious amendments
  - Capture by a coordinated 80%+ majority attack
```

### Weekly UNL rotation

At each weekly hard close:

```
1. Score all validators (4-component model — see Part 5)

2. Sort by composite score:
   Trusted pool:  [t1 (95%), t2 (89%), ..., t15 (40%)]
   Public pool:   [p1 (98%), p2 (92%), ..., p15 (60%)]
   Bench:         [b1 (94%), b2 (88%), ..., bN (20%)]

3. Rotation candidates:
   bottom_10_pct_trusted = floor(15 × 0.10) = 1–2 validators
   worst_trusted         = t15 (40%)

4. Swap:
   if bench_best.score > worst_trusted.score:
     remove worst_trusted from UNL  →  bench
     promote bench_best from bench  →  UNL (public pool)

5. Emit UNLRotation transaction on-ledger (transparent, auditable)
```

**Result:** Poor performers are demoted; top performers are promoted automatically. No human gatekeeping after bootstrap.

### On-ledger rotation record

```rust
pub struct UNLRotation {
    epoch:          u64,
    removed:        ValidatorId,
    added:          ValidatorId,
    removed_score:  f64,
    added_score:    f64,
    reason:         RotationReason,  // "bottom_10_percent_rotation"
}
```

---

## Part 4: Bootstrap Phase (epochs 0–7, 8 weeks)

### Timeline

| Phase | Duration | What happens |
|-------|----------|--------------|
| **Genesis** | Week 0 | Network launches; UNL initialized (30 validators); faucet & arcade open |
| **Onboarding** | Weeks 1–7 | Community joins as validators, full nodes, users; all earn scores |
| **Snapshot** | Week 8 | Bootstrap ends; airdrop calculated; 2B FALC distributed |
| **Production** | Week 9+ | CID emissions begin; validator scoring continues; user activity scoring ends |

### Who earns bootstrap scores

**Validators** (40% uptime, 30% vote accuracy, 20% latency, 10% consistency):

| Component | Measurement |
|-----------|-------------|
| Uptime | % of batches signed |
| Vote accuracy | Correct cert votes / total cert votes |
| Latency | Time from first signer to this validator's signature |
| Consistency | No equivocation or Byzantine behavior detected |

**Full node operators** (40% uptime, 35% latency, 25% sync quality):

| Component | Measurement |
|-----------|-------------|
| Uptime | RPC endpoint responding |
| Latency | Response time in ms |
| Sync quality | Blocks behind chain tip |

**Users:**
- Arcade activity (games, challenges)
- Faucet claims (rate-limited: 5 per day)
- Transaction activity (network usage)

### Composite scoring (week 8 snapshot)

```
composite = validator_score   × 0.25
          + full_node_score   × 0.25
          + arcade_score      × 0.15
          + faucet_score      × 0.15
          + transaction_score × 0.20

airdrop_share = 2B FALC × (participant_composite / sum_all_composites)
```

Zero scores are used for roles the participant did not run (e.g., `validator_score = 0` if not running a validator).

---

## Part 5: Full Node Rewards — On-Ledger Registry + UNL Probing

Full nodes are paid from the transaction fee pool. Measurement uses **on-ledger registry + UNL probing** (Option 1).

### How it works

**Step 1 — Registration**

Full node operator registers on-ledger:

```
FullNodeRegistry entry:
  operator_pubkey:  <Falcon-512 public key>
  rpc_endpoint:     https://<ip>:<port>
  stake:            <minimum FALC stake to prevent spam>
  registered_epoch: <epoch number>
```

**Step 2 — Weekly probing by UNL validators**

Each week, every UNL validator independently probes all registered full nodes:

```
For each registered full node:
  1. Connect to rpc_endpoint
  2. Record: connected (yes/no), latency_ms, latest_ledger_index
  3. Compare latest_ledger_index to chain tip → sync_lag
  4. Publish signed measurement on-ledger
```

**Step 3 — Aggregate measurements**

At hard close, use the **median** across all UNL probes for each full node (Byzantine-robust — a minority of dishonest validators cannot skew the result):

```
uptime_pct  = median(connected_pct across all UNL probes)
latency_ms  = median(latency_ms across all UNL probes)
sync_lag    = median(sync_lag across all UNL probes)
```

**Step 4 — Score and pay**

```
full_node_score = (uptime_pct × 0.40)
                + (latency_score × 0.35)    # inverted: lower ms = higher score
                + (sync_score × 0.25)       # inverted: lower lag = higher score

full_node_reward = full_node_pool × (full_node_score / sum_all_full_node_scores)
```

---

## Part 6: Fee Model

### Base fee

- **15 drops per transaction** (0.000015 FALC)
- Reflects post-quantum cryptography (PQC) overhead

### Weekly fee allocation (at hard close)

```
Total fees collected this epoch = F FALC

  Burned (deflationary):       F × 0.50
  Treasury replenishment:      F × 0.30
  Full node operator rewards:  F × 0.20
```

### Long-term sustainability

| Period | Dominant source | Annual emission rate |
|--------|-----------------|----------------------|
| Year 1–5 | CID emissions | ~12% declining |
| Year 5–10 | Fees increasingly matter | ~4% |
| Year 10+ | Fees sustain network | ~1.5% floor |

Treasury is replenished weekly from fees, so it never depletes to zero even after CID emissions decline.

---

## Part 7: CID Emission Schedule

### Formula

```
emission_this_epoch = treasury_balance × (base_rate - decay_per_epoch)

  base_rate       = 0.12 / 52  ≈ 0.0023   (≈12% annually, year 1)
  decay_per_epoch = 0.0001 / epoch_number

Effect: Declining schedule, reaches ~1.5% annual floor after 10+ years
```

### Indicative schedule

| Year | Weekly emission | Annual equiv | Treasury balance |
|------|-----------------|--------------|------------------|
| 1 | 0.420M FALC | ~12.0% | 194.6B FALC |
| 2 | 0.410M FALC | ~11.8% | 193.3B FALC |
| 5 | 0.325M FALC | ~4.1% | 188.5B FALC |
| 10 | 0.215M FALC | ~2.1% | 178.6B FALC |
| 20+ | ~0.1M FALC | ~1.5% | Stabilizes ~50–100B |

### Validator reward distribution (production phase)

```
Weekly hard close:

1. Validators scored (4-component model)
2. CID emitted: per schedule above
3. 70% CID → validator pool
4. Distributed proportional to score:
     validator_reward = validator_pool × (validator_score / sum_all_scores)

5. Full node rewards from fee split:
     full_node_pool   = transaction_fees × 0.20
     full_node_reward = full_node_pool × (full_node_score / sum_all_full_node_scores)

6. Treasury replenishment:
     treasury        += transaction_fees × 0.30

7. Burned (deflationary):
     burned           = transaction_fees × 0.50
```

---

## Part 8: Governance

### Amendment process (dual-pool)

```
Proposal submission:
  Any UNL validator submits a GovernanceProposal.
  Examples:
    - Increase max_batch_txs (256 → 512)
    - Adjust fee split (50/30/20 → 40/40/20)
    - Change UNL rotation percentage (10% → 5%)
    - Modify faucet limits (5 claims/day → 3)
    - Expand UNL size (30 → 50)

Voting window: 2 weeks (14 days)
  - All UNL validators vote YES/NO
  - Requires majority in BOTH trusted pool AND public pool
  - Simple majority per pool (50%+1)

Apply: At next hard close (if passed)
  - Parameter change recorded on-ledger
  - Applies immediately to new epoch
  - Rate limit: no more than 1 amendment per week
```

### Anti-capture properties

- **Dual-pool requirement:** Attacker must control both pools simultaneously
- **Transparent voting:** All votes published on-ledger, auditable by anyone
- **Amendment rate-limiting:** Max 1 amendment per week (prevents governance spam)
- **UNL rotation:** Public pool refreshes continuously; sustained capture is difficult

---

## Part 9: Validator Key Generation & Startup

### Step 1 — Generate Falcon-512 key

```bash
falcon-key-gen --output validator_key.json
```

This produces a local key file containing the Falcon-512 private key and public key.

### Step 2 — Register (bootstrap phase)

During bootstrap, submit your public key to the bootstrap registry (website or API). Your node begins as a **bench validator** immediately upon connecting; you earn scores from day one.

After bootstrap, registration is permissionless and fully on-chain.

### Step 3 — Start validator node

```bash
falcon-dagd --validator \
  --keys /path/to/validator_key.json \
  --network mainnet \
  --region us-east-1
```

The node automatically:
- Syncs the DAG state
- Begins scoring (uptime, accuracy, latency, consistency)
- Participates in bootstrap airdrop eligibility
- Gets promoted to UNL via rotation when score qualifies

### Step 4 — Start full node (optional, additional rewards)

```bash
falcon-dagd --full-node --network mainnet
```

The full node automatically:
- Registers in on-ledger full node registry
- Gets probed weekly by UNL validators
- Earns fee rebates proportional to measured score

---

## Part 10: Monitoring & Health Metrics

### Weekly hard-close metrics

```
Consensus health:
  finality_lag_ms         target < 500ms
  epoch_close_on_schedule target = weekly (604,800 seconds)
  validator_liveness_pct  % of UNL signing batches
  fork_detected           any diverging tips

Economic health:
  cid_emission_falc       vs schedule
  treasury_balance_falc   tracked on-ledger
  fee_volume_falc         total fees collected this epoch
  bootstrap_weeks_left    (epochs 0–7 only)

Operational health:
  avg_batch_size_txs      transactions per batch
  settle_tps              target 263+ baseline, 600+ post-Phase 50
  full_node_rpc_uptime    % of registered nodes responding
  gossip_latency_ms       peer-to-peer propagation lag
```

### Per-validator metrics (weekly)

```
uptime_pct          % of batches signed
vote_accuracy_pct   correct cert votes / total
latency_ms          avg time to signature (lower = better)
byzantine_detected  any equivocations (0 or 1)
composite_score     weighted blend (40/30/20/10)
pool_membership     "trusted" | "public" | "bench"
reward_falc         FALC earned this epoch
rotation_candidate  is this validator bottom 10% of their pool
```

### Recommended dashboard panels

- Treasury balance (live: declining on emissions, rising on fee replenishment)
- Validator leaderboard (sorted by composite score, showing pool membership)
- Full node availability map (RPC status per registered endpoint)
- Bootstrap countdown timer (weeks remaining, epochs 0–7)
- Amendment tracker (proposals in voting, pass/fail history)
- Weekly fee statistics (burned, treasury topped up, full node payouts)

---

## Part 11: Security & Trust Model

### Validator set

| Phase | Control | Risk level |
|-------|---------|------------|
| Bootstrap (epochs 0–7) | Trusted pool (dev + vetted) | Low — known operators |
| Growth (epochs 9–52) | Dual pool, active rotation | Medium — public validators enter |
| Mature (epoch 52+) | Governance-controlled | Low — established track record |

### BFT safety requirement

- Safety threshold: 2/3 honest validators in UNL
- UNL size (30) requires ≥ 20 honest nodes
- Dual-pool amendment guard adds a second Byzantine threshold before governance changes apply

### Fee & treasury security

- **Spam prevention:** Base fee cost per transaction
- **Deflationary burn:** 50% of fees permanently removed (counters emission inflation)
- **Protocol-only treasury releases:** No human key or multisig can override emission schedule
- **On-ledger auditing:** Every emission, replenishment, rotation, and amendment is a verifiable transaction

---

## Part 12: Launch Checklist

### Before mainnet (Phase 51a implementation)

- [ ] Hard close transaction (weekly hard close logic)
- [ ] Validator scoring (4-component model, EMA smoothing)
- [ ] Full node scoring (UNL-probed measurement, on-ledger registry)
- [ ] User activity tracking (arcade, faucet, transactions)
- [ ] Airdrop mechanism (snapshot at epoch 8, allocate 2B FALC)
- [ ] Epoch 9 transition (CID emissions begin, user scoring ends)
- [ ] CID emission logic (treasury release per schedule)
- [ ] Fee allocation (50/30/20 burn/treasury/full-nodes)
- [ ] Dual-pool governance voting
- [ ] UNL rotation logic (bottom 10% swap, bench promotion)
- [ ] Monitoring & alerting (key metrics tracked)

### At mainnet launch (epoch 0)

- [ ] Genesis block with 196B FALC treasury
- [ ] Initial UNL deployed (30 validators: 15 trusted + 15 public)
- [ ] Full node registry initialized
- [ ] Faucet active (5 claims/day rate limit)
- [ ] Arcade active (community games, point-based scoring)
- [ ] Bootstrap countdown timer (8 weeks to airdrop)
- [ ] Public documentation (validator setup, scoring rules, airdrop mechanics)

### Week 8 (epoch 8)

- [ ] Airdrop snapshot finalized
- [ ] 2B FALC distributed to all eligible participants
- [ ] Airdrop results broadcast (transparent, on-ledger)

### Week 9+ (epoch 9+)

- [ ] CID emissions active
- [ ] User activity scoring ends
- [ ] Validator and full node scoring continues (production model)
- [ ] Dual-pool governance amendments enabled
- [ ] UNL rotation begins (weekly)
