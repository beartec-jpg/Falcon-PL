# Falcon PL (`fd-pl` — v2.9.81)

**Product:** Falcon PL · **Ticker:** FPL · **Consensus:** [Falcon Consensus](docs/IDENTITY.md) · **Signatures:** Falcon-512

Greenfield post-quantum **ordered ledger** — **not** an XRP Ledger fork, **not** a DAG, and **not** Falcon Ledger (qXRP).

Bonded validators join the deterministic **committee lottery**. Each height selects a 6-seat committee and ranked proposer order; blocks commit on a **4-of-6 Falcon-512 Quorum Certificate (QC)**; silent proposers time out so the next ranked standby seat seals without stalling the tip.

| Component | Behavior in `2.9.81` |
|-----------|----------------------|
| Truth | Hash-linked ledgers of Falcon-512 signed transactions |
| Cryptography | **`falcon-512`** on network 2300 (`hmac-sha256-dev` restricted to local dev harnesses) |
| **Falcon Consensus** | Deterministic 6-seat committee lottery per height; 4-of-6 Falcon-512 QC commit; ranked standby fallback; equivocation slash |
| Key Registry & Seat Payouts | On-chain wallet key binding (`PayToKey`, Fix B at `692,000+`) and isolated per-seat validator reward buckets (`SetSeatPayout` / `ClaimSeat`, [docs/SEAT_PAYOUT.md](../../../../docs/SEAT_PAYOUT.md)) |
| Storage & RAM | Disk-backed ledger body store (`--body-store-dir`) + bounded in-memory chain window (`--max-chain-ram`) + `MALLOC_ARENA_MAX=2` |
| Bridges | Sepolia ETH/USDC V3 (`FalconQcBridgeV3` + Gloas LC) and Bitcoin Testnet4 `BTC4` BitVM2 bridge (`692,000+`) |

## Documentation

| Doc | Contents |
|-----|----------|
| **[docs/README.md](docs/README.md)** | **Documentation index** — current vs. historical |
| **[../../docs/PUBLIC_NODE.md](../../../../docs/PUBLIC_NODE.md)** | Public observer, watcher, and validator node guide (`2.9.81`) |
| **[../../docs/SEAT_PAYOUT.md](../../../../docs/SEAT_PAYOUT.md)** | Per-seat validator rewards (`SetSeatPayout` / `ClaimSeat` at `692,000+`) |
| **[docs/BRIDGES_2300_STATUS.md](docs/BRIDGES_2300_STATUS.md)** | Live Sepolia ETH/USDC V3 and Bitcoin `BTC4` / legacy `BTC` status |
| **[docs/ETH_V3_DEPOSITS.md](docs/ETH_V3_DEPOSITS.md)** | Sepolia `FalconQcBridgeV3` deposit mint-or-refund mechanics |
| **[docs/SNARK_LC_2300.md](docs/SNARK_LC_2300.md)** | Groth16 4-of-6 Falcon-512 QC light client on Sepolia |
| **[docs/BITVM2_ENGINE.md](docs/BITVM2_ENGINE.md)** | BitVM2 withdrawal circuit (`FplWithdrawValid`) and Bitcoin Testnet4 `BTC4` engine |
| **[docs/PUBLIC_PARAMS_2300.md](docs/PUBLIC_PARAMS_2300.md)** | Public Testnet 2300 parameter sheet |
| **[docs/EMISSION_MATRIX.md](docs/EMISSION_MATRIX.md)** | Epoch emission & Rewards v2 distribution matrix |
| **[docs/IDENTITY.md](docs/IDENTITY.md)** | Naming + Falcon Consensus definition |

## Build & Test

```bash
cd falcon-pl-rs
cargo test -p fd-pl --lib
cargo build -p fd-pl --release
```

Binaries: `falcon-pl-node`, `falcon-pl-ctl`, `falcon-pl-load-net`, `falcon-pl-demo`.
