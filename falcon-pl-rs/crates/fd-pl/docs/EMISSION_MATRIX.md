# Falcon PL — Emission & Rewards v2 Matrix (`v2.9.81`, Height `692,000+`)

Verified by unit tests (`emission_matrix_sectors_and_caps`, `rewards_v2`, and `operator_registry` tests in `crates/fd-pl`).

## 1. Live Testnet 2300 Reward Rules (`692,000+`)

### Block Transaction Fees (Every Block, `420,000+`)
- **30% Proposer Share:** Credited immediately at block commit to the proposer seat's reward bucket (`seat_claimable` or configured `SetSeatPayout` account).
- **70% Epoch Fee Pool:** Added to the epoch reward pool distributed at the end of the 7-day (`10,000`-block) epoch.

### Epoch Emission Split (`0.30%` of Remaining Treasury per Epoch)
Since height **`692,000`** (`TESTNET_2300_BRIDGE_FROM_HEIGHT = 692_000`), **`5%` (`OPERATOR_SHARE_BPS = 500`)** is carved off the top of `epochEmit` for the **`BTC4` BitVM2 operator bucket**, and the remaining **`95%`** (`post = epochEmit - operator_bucket`) is split **55% Validators / 5% Watchers / 20% AMM / 20% Lending**:

| Bucket | Share of `epochEmit` | First-Epoch Reference (`epochEmit = 588,000,000 FPL`) | Inner Split & Caps |
|---|---|---|---|
| **`BTC4` BitVM2 Operators** | **5.00%** (top carve) | `29,400,000 FPL` | **25%** liveness (≥151/168 hourly slots), **25%** watch/disprove units, **50%** proven `BTC4` payout volume (capped at 25% of bucket per operator). Requires `5,000 FPL` bond. Unpaid remainder refunds treasury. |
| **Validators** | **52.25%** (`55%` of `95%`) | `307,230,000 FPL` | **50% proposals** (`64 + min(fee_txs, 64)` per round-0 block), **40% timely round-0 QC votes**, **10% floor** (archive seats `×1.05`), scaled by presence (`430,000+`) and miss penalty (`441,000+`). Credited to per-seat bucket (`692,000+`). |
| **Watchers** | **4.75%** (`5%` of `95%`) | `27,930,000 FPL` | Work credits × hourly presence slots (`1,000 FPL` min bond). Per-account cap: `0.05%` of `epochEmit` (`294,000 FPL`). |
| **AMM LPs** | **19.00%** (`20%` of `95%`) | `111,720,000 FPL` | Time-weighted FPL liquidity held across the epoch. Per-account cap: `0.50%` of `epochEmit` (`2,940,000 FPL`). |
| **Lending LPs** | **19.00%** (`20%` of `95%`) | `111,720,000 FPL` | Time-weighted supplied value across the epoch. Per-account cap: `0.50%` of `epochEmit` (`2,940,000 FPL`). |

---

## 2. Pre-692,000 / Baseline v1-v2 Matrix (`operator_bucket = 0`)

Before height `692,000` (or on networks where `operator_bucket = 0`), the 55/5/20/20 split applies directly to 100% of `epochEmit`:

| Item | Value |
|------|-------|
| Initial Treasury | `196,000,000,000 FPL` |
| `emission_bps` | `30` (`0.30%` per epoch) |
| **`epochEmit` (Epoch 1)** | **`588,000,000 FPL`** |
| Validators (`55%`) | `323,400,000 FPL` |
| Watchers (`5%`) | `29,400,000 FPL` |
| AMM (`20%`) | `117,600,000 FPL` |
| Lending (`20%`) | `117,600,000 FPL` |
| Per-account LP cap (`0.50%`) | `2,940,000 FPL` |
| Per-account Watcher cap (`0.05%`) | `294,000 FPL` |

## Running the Unit Tests

```bash
cd falcon-pl-rs
cargo test -p fd-pl --lib emission_matrix -- --nocapture
cargo test -p fd-pl --lib operator_registry -- --nocapture
```
