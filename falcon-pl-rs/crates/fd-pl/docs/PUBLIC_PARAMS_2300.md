# Falcon PL — Public Testnet 2300 Parameter Sheet (`v2.9.81`, Height `692,000+`)

| | |
|--|--|
| **Product** | Falcon PL (FPL) |
| **Consensus** | Falcon Consensus (6-seat deterministic lottery + 4-of-6 Falcon-512 QC) |
| **Cryptography** | Falcon-512 (`hmac-dev` refused without `--allow-dev-crypto`) |
| **Version (`PRODUCT_VERSION`)** | **`2.9.81`** (workspace & live Testnet 2300 mesh) |
| **Public Testnet** | `network_id` **`2300`** (`VALIDATOR_ONBOARDING=open`) |
| **Public Seed Relay** | `5.78.142.246:19301` (plus per-seat ports `19302..19312` in `scripts/network-2300.env`) |

## Consensus & Runtime Parameters

| Parameter | Public Testnet 2300 (`2.9.81`) | Notes |
|-----------|-------------|----------------------------|
| Soft QC | **4-of-6** committee | Deterministic 6-seat committee per height |
| Hard QC | `ceil(n × 0.6)` bonded | Active bonded validator set |
| Pack budget | `1000 ms` | |
| Progress timeout | `3000 ms` | Standby seat fallback on timeout |
| Seal interval | `200 ms` | Empty seals rejected |
| Max txs / ledger | `128` | |
| Max mempool | `50,000` | |
| Max chain RAM | `20,000` ledgers (`--max-chain-ram`) | Older bodies pruned from RAM and served from disk (`--body-store-dir`) |
| Allocator tuning | `MALLOC_ARENA_MAX=2` | Set in systemd unit to prevent glibc arena bloat |
| Vote & tx signatures | Falcon-512 required | On-chain wallet key binding enforced at `692,000+` (`PayToKey` / Fix B) |
| Admin on TCP | Disabled (`--lock-public-wire`) | Local Unix socket `--admin-sock` (mode `0600`) only |

## Staking, Fees & Emissions (Rewards v2 + Operator Bucket)

| Parameter | Public Testnet 2300 (`2.9.81`) | Notes |
|-----------|-------------|-----------|
| Epoch length | **7 days** (`604,800,000 ms` / `10,000` blocks) | |
| First claimable epoch | **1** on Testnet 2300 | Mainnet bootstrap: epoch 8 |
| Unbond cooldown | **14 days** (`1,209,600,000 ms`) | 2 epochs for `BTC4` operators |
| **Validator minimum bond** | **`50,000 FPL`** (`bond-v2`) | Proof-of-possession + admission key approval (`2.9.62+`) |
| **Watcher minimum bond** | **`1,000 FPL`** | Required for watcher reward eligibility |
| **`BTC4` Operator minimum bond** | **`5,000 FPL`** (`operator_register`) | Active since height `692,000`; 100% slash on proven Disprove |
| Fee split (Rewards v2, `420,000+`) | **30% proposer** at commit / **70% epoch fee pool** | Base min fee `1`, fee escalation `1× / 2× / 4× / 8× / 16×` |
| Epoch emission | `treasury × 30 bps / 10,000` (`0.30%`) | Hard cap `200,000,000,000 FPL` (196B initial treasury) |
| **Top-carve (`692,000+`)** | **5% `BTC4` BitVM2 Operators** (`OPERATOR_SHARE_BPS = 500`) | Carved before the 55/5/20/20 split |
| **Post-operator split (95%)** | **55% Validators / 5% Watchers / 20% AMM / 20% Lending** | Effective total: **5% operators / 52.25% vals / 4.75% watchers / 19% AMM / 19% lend** |
| **Validator pot split** | **50% proposals / 40% round-0 QC votes / 10% floor** | Presence scaling (`430,000+`), catch-up miss penalty (`441,000+`), archive floor `×1.05` |
| **Seat payout isolation** | Active since **`692,000`** | Validator rewards credited to `seat_claimable` or `SetSeatPayout` account ([docs/SEAT_PAYOUT.md](../../../../docs/SEAT_PAYOUT.md)) |

## Bridge & Consensus Activation Heights on 2300

| Gate / Constant (`params_v2.rs`) | Activation Height | Description |
|---|---|---|
| `TESTNET_2300_ETH_GLOAS_FROM_HEIGHT` | **`409,134`** | Sepolia Gloas beacon sync-committee LC + 50,400-header execution window |
| `TESTNET_2300_ETH_V3_FROM_HEIGHT` | **`414,000`** | Sepolia `FalconQcBridgeV3` (`0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4`) mint-or-refund deposits |
| `TESTNET_2300_REWARDS_V2_FROM_HEIGHT` | **`420,000`** | 30% proposer fee share, 50/40/10 validator pot |
| `TESTNET_2300_PRESENCE_FROM_HEIGHT` | **`430,000`** | Validator presence factor scaling |
| `TESTNET_2300_MISS_PENALTY_FROM_HEIGHT` | **`441,000`** | Validator catch-up / missed-turn penalty |
| `TESTNET_2300_SEAT_PAYOUT_FROM_HEIGHT` | **`692,000`** | Per-seat validator reward buckets (`SetSeatPayout` / `ClaimSeat`) & Fix B wallet key binding |
| `TESTNET_2300_BRIDGE_FROM_HEIGHT` | **`692,000`** | Bitcoin Testnet4 (`BTC4`) BitVM2 M1 attestations (`Bitvm2AttestV2`), operator registry, graph bindings & 5% operator emission bucket |
