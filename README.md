# Falcon PL (FPL)

**Participation Ledger** — post-quantum ordered ledger and cross-chain bridge stack for Falcon Consensus.

| | |
|--|--|
| **Product** | Falcon PL |
| **Ticker** | FPL |
| **Consensus** | Falcon Consensus (deterministic committee lottery + 4-of-6 Falcon-512 QC) |
| **Signatures** | Falcon-512 (NIST PQC lattice signature; `hmac-dev` restricted to local test harnesses) |
| **Version** | **2.10.0** live on public testnet **2300** (height **692,000+**) |
| **Repository** | https://github.com/beartec-jpg/Falcon-PL |

> This repository is the **Falcon PL product**.  
> Historical **Falcon DAG** (Narwhal-style DA research) is frozen under [`archive/`](archive/) and [`falcon-pl-rs/archive/`](falcon-pl-rs/archive/) and is not part of the active workspace.

## Workspace Layout

```
falcon-pl-rs/
  crates/
    fd-pl/               ← Falcon PL core node, consensus, execution, light clients & CLI
    fd-pl-snark/         ← Groth16 over BN254 circuits for 4-of-6 Falcon-512 QC & light client
    fd-pl-bitvm2/        ← BitVM2 withdraw circuit (FplWithdrawValid), trace split & taptree engine
    fd-pl-graph/         ← BitVM2 operator presigned transaction graph primitives
  tools/
    fpl-operator/        ← Standalone BTC4 BitVM2 operator daemon, gossip & challenger CLI (fpl-operatord)
    fpl-bridge/          ← BitVM2 ceremony coordinator, participant, challenger & regtest/testnet4 tools
    bitvm-c1b-export/    ← BitVM BN254 script chunk digest exporter
docs/
  PUBLIC_NODE.md         ← Public observer, watcher & validator node guide (Testnet 2300)
  SEAT_PAYOUT.md         ← Per-seat validator reward isolation & SetSeatPayout / ClaimSeat guide
  bridge/                ← BTC4 BitVM2 bridge architecture, operator guide & rewards specification
```

## Key Features in 2.10.0 (Testnet 2300 @ Height 692,000+)

- **Falcon-512 Post-Quantum Consensus:** Deterministic 6-seat committee lottery per height with 4-of-6 Falcon-512 Quorum Certificates (QC), ranked standby fallback on timeout, and equivocation slashing.
- **On-Chain Key Registry & Seat Payout Isolation (`692,000+`):** Wallet keys are committed and enforced on-chain (`PayToKey` / Fix B), while validator rewards accrue to isolated per-seat buckets (`ValidatorInfo.seat_claimable` or configured `--payout-address` via `SetSeatPayout` / `ClaimSeat`).
- **Lean Node Runtime:** Disk-backed ledger body store (`--body-store-dir`), bounded in-memory chain window (`--max-chain-ram`), and `MALLOC_ARENA_MAX=2` keep steady-state validator/observer RAM under ~300–600 MB.
- **Rewards v2 & Operator Emissions:**
  - **30%** of transaction fees go directly to the block proposer at commit; **70%** enters the epoch fee pool.
  - At each 7-day epoch boundary, **5%** of emission is carved off the top for active `BTC4` BitVM2 operators (`OPERATOR_SHARE_BPS = 500`), and the remaining 95% splits **55% validators / 5% watchers / 20% AMM / 20% lending** (effective total: **5% BTC4 operators / 52.25% validators / 4.75% watchers / 19% AMM / 19% lending**).
  - The validator pot splits **50% proposals / 40% timely round-0 QC votes / 10% floor**, scaled by active presence (`430,000+`) and catch-up miss penalties (`441,000+`).
- **Live Cross-Chain Bridges on Testnet 2300:**
  - **Sepolia ETH / USDC V3 (`FalconQcBridgeV3`):** Live at `0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4` (Groth16 verifier `0xA03005921E1807587B942ab00cC4212650670aA6`), backed by the on-chain Ethereum Gloas beacon light client (`409,134+`) and V3 mint-or-refund deposits (`414,000+`).
  - **Bitcoin Testnet4 (`BTC4`) BitVM2 Bridge:** Live from height `692,000` (`TESTNET_2300_BRIDGE_FROM_HEIGHT = 692_000`, `5,000 FPL` operator bond, `Bitvm2AttestV2` SPV + Groth16 verification, presigned Challenge/Assert/Disprove graphs). Legacy Bitcoin Testnet3 (`BTC`) remains frozen.

## Build & Test

```bash
cd falcon-pl-rs
cargo check --workspace
cargo build -p fd-pl --release
cargo test -p fd-pl --lib
```

Key binaries: `falcon-pl-node`, `falcon-pl-ctl`, `falcon-pl-snark`, `falcon-pl-bitvm2-emu`, `fpl-operatord`.

## Documentation

- **[docs/PUBLIC_NODE.md](docs/PUBLIC_NODE.md)** — Install and operate an observer, watcher, or validator node on Testnet 2300 (`2.10.0`, `fdash`, `--payout-account`, open validator onboarding)
- **[docs/SEAT_PAYOUT.md](docs/SEAT_PAYOUT.md)** — Validator seat reward buckets, `SetSeatPayout`, and `ClaimSeat` (`692,000+`)
- **[docs/bridge/BRIDGE_INTEGRATION.md](docs/bridge/BRIDGE_INTEGRATION.md)** — `BTC4` BitVM2 bridge + operator registry ledger integration (`692,000+`)
- **[docs/bridge/OPERATOR_GUIDE.md](docs/bridge/OPERATOR_GUIDE.md)** — Running a `BTC4` BitVM2 operator (`fpl-operatord`)
- **[docs/bridge/OPERATOR_REWARDS.md](docs/bridge/OPERATOR_REWARDS.md)** — `BTC4` BitVM2 operator bonding, slashing, and 5% emission share
- **[falcon-pl-rs/crates/fd-pl/docs/README.md](falcon-pl-rs/crates/fd-pl/docs/README.md)** — Core `fd-pl` documentation index
- **[falcon-pl-rs/crates/fd-pl/docs/BRIDGES_2300_STATUS.md](falcon-pl-rs/crates/fd-pl/docs/BRIDGES_2300_STATUS.md)** — Live Sepolia ETH/USDC V3 and Bitcoin `BTC4` / legacy `BTC` bridge status
- **[falcon-pl-rs/crates/fd-pl/docs/PUBLIC_PARAMS_2300.md](falcon-pl-rs/crates/fd-pl/docs/PUBLIC_PARAMS_2300.md)** — Public Testnet 2300 parameter sheet
- **[SECURITY.md](SECURITY.md)** & **[CONTRIBUTING.md](CONTRIBUTING.md)** — Security disclosure policy and public review guidelines

## Archive (Historical Research Only)

| Path | Contents |
|------|----------|
| [`archive/dag-python/`](archive/dag-python/) | Early Python DAG prototype |
| [`archive/dag-docs/`](archive/dag-docs/) | Historical DAG design / TPS / Narwhal notes |
| [`archive/dag-deploy/`](archive/dag-deploy/) | Historical DAG deploy scripts |
| [`falcon-pl-rs/archive/dag/`](falcon-pl-rs/archive/dag/) | Frozen Rust DAG crates (`fd-node`, `fd-dag`, …) |

## License

Copyright (c) BEARTEC. All Rights Reserved.  
Provided under the BEARTEC Public Review & Network Participation License — see [`LICENSE`](LICENSE).
