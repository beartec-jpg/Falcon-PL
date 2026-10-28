# Falcon PL Documentation Index

**For reviewers, node operators, and bridge operators.** Last updated **2026-10-24** (`v2.10.0`, Testnet 2300 height `692,000+`).

Read **current** files first. Files under "Historical" describe earlier milestones (private soak 2200, v2.2 expansion plan, or pre-BitVM2 vault designs) and are kept in git so engineering history remains auditable.

---

## Read First (Current — `v2.10.0` / Testnet 2300)

| Doc | What it covers |
|-----|----------------|
| **[../../../../docs/PUBLIC_NODE.md](../../../../docs/PUBLIC_NODE.md)** | **Public Node & Validator Guide (`2.10.0`)** — observer/watcher/validator install, open validator onboarding (`50,000 FPL` bond), `fdash`, `--payout-account`, Rewards v2 |
| **[../../../../docs/SEAT_PAYOUT.md](../../../../docs/SEAT_PAYOUT.md)** | **Seat Payout Isolation (`692,000+`)** — per-seat validator reward buckets, `SetSeatPayout`, `ClaimSeat` |
| **[BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md)** | **Live Bridges Source of Truth** — Sepolia ETH/USDC V3 (`FalconQcBridgeV3`), Bitcoin Testnet4 `BTC4` BitVM2 bridge (`692,000+`), and frozen legacy Testnet3 `BTC` pool |
| **[../../../../docs/bridge/BRIDGE_INTEGRATION.md](../../../../docs/bridge/BRIDGE_INTEGRATION.md)** | **BTC4 BitVM2 Ledger Integration (`692,000+`)** — `Bitvm2AttestV2`, operator registry, graph bindings, SPV disprove slashing |
| **[../../../../docs/bridge/OPERATOR_GUIDE.md](../../../../docs/bridge/OPERATOR_GUIDE.md)** | **BTC4 Operator Guide** — running `fpl-operatord`, pregen pools, peer gossip, and challenge/disprove watching |
| **[../../../../docs/bridge/OPERATOR_REWARDS.md](../../../../docs/bridge/OPERATOR_REWARDS.md)** | **BTC4 Operator Rewards** — `5,000 FPL` bond, 5% top-carve epoch emission share, liveness/watching/payout split, 100% slash |
| **[ETH_V3_DEPOSITS.md](ETH_V3_DEPOSITS.md)** | Sepolia `FalconQcBridgeV3` mint-or-refund deposit rules (`414,000+`) |
| **[ETH_USDC_2300.md](ETH_USDC_2300.md)** | Sepolia ETH/USDC bridge architecture (`FalconQcBridgeV3` + Gloas sync-committee light client) |
| **[SNARK_LC_2300.md](SNARK_LC_2300.md)** | Groth16 4-of-6 Falcon-512 QC circuit (`fd-pl-snark`) and Sepolia verifiers |
| **[BITVM2_ENGINE.md](BITVM2_ENGINE.md)** | BitVM2 `FplWithdrawValid` circuit (`fd-pl-bitvm2`), M1 checkpoints, and Bitcoin Testnet4 `BTC4` execution |
| **[PUBLIC_PARAMS_2300.md](PUBLIC_PARAMS_2300.md)** | Public Testnet 2300 consensus, storage, bridge, and tokenomics parameters (`2.10.0`) |
| **[EMISSION_MATRIX.md](EMISSION_MATRIX.md)** | Epoch emission & Rewards v2 split (proposer 30% fees, 5% `BTC4` operators + 55/5/20/20 split, 50/40/10 validator pot) |
| [BTC_POOL_FREEZE_2026-10-16.md](BTC_POOL_FREEZE_2026-10-16.md) | Legacy Bitcoin Testnet3 (`BTC`) custodial pool freeze record |
| [IDENTITY.md](IDENTITY.md) | Naming: Falcon-512 vs. Falcon Consensus vs. Falcon PL |
| [CONSENSUS_2.9.0.md](CONSENSUS_2.9.0.md) | 4-of-6 committee QC commit rules |

---

## Historical (Retained for Engineering Context)

| Doc | Why it is historical |
|-----|----------------------|
| [BITVM_VAULT_2300.md](BITVM_VAULT_2300.md) | Legacy Testnet3 watch+dest Kickoff vault (frozen 2026-10-16; replaced by `BTC4` BitVM2 at `692,000`) |
| [USER_VAULT_AUDIT.md](USER_VAULT_AUDIT.md) | Early 1:1 user vault notes |
| [CEREMONY_2300.md](CEREMONY_2300.md) | Initial 2300 genesis ceremony notes |
| [BTC_RAIL_2300_PLAN.md](BTC_RAIL_2300_PLAN.md) | Pre-e2e BTC rail plan (superseded by `BRIDGES_2300_STATUS.md`) |
| [FEATURES_AND_TOKENOMICS.md](FEATURES_AND_TOKENOMICS.md) | v2.2 target surface (superseded by Rewards v2 and `PUBLIC_PARAMS_2300.md`) |
| [EXPANSION_PLAN_v2.2.0.md](EXPANSION_PLAN_v2.2.0.md) | Locked private-fleet 2200 plan |
| [PRIVATE_GATE_v2.2.md](PRIVATE_GATE_v2.2.md) | 2200-era gate |
| [POC.md](POC.md) | Early PoC report |
| [REVIEW_v2.6.0.md](REVIEW_v2.6.0.md) | Snapshot review of v2.6.0 (2026-08-13) |
| [PUBLIC_TESTNET_IMPL_PLAN.md](PUBLIC_TESTNET_IMPL_PLAN.md) | Initial public testnet planning notes |
| [DIG_20260813_tip49.md](DIG_20260813_tip49.md) | Early incident dig |
| Repo root `archive/` | Frozen Falcon DAG research — not part of Falcon PL |

---

## What This Is Not

- Not an external third-party audit report.
- Not mainnet contract addresses (Testnet 2300 + Sepolia + Bitcoin Testnet4).
- Not Falcon Ledger (qXRP / network 1001).
