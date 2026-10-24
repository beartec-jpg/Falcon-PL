# Run a Falcon PL Node (Public Testnet 2300 — `v2.9.81`)

Status: **Observer, Watcher & Validator onboarding are OPEN** on public testnet **2300** (`v2.9.81`, height `692,000+`, `VALIDATOR_ONBOARDING=open`).
- **Validator bond:** `50,000 FPL` minimum (`bond-v2` with proof-of-possession + admission key approval)
- **Watcher bond / stake:** `1,000 FPL` minimum
- **`BTC4` BitVM2 Operator bond:** `5,000 FPL` minimum (see [`docs/bridge/OPERATOR_GUIDE.md`](bridge/OPERATOR_GUIDE.md))

This is a public testnet. Testnet FPL has no monetary value.

## One-Line Install (Observer / Lite Node)

```bash
curl -fsSL https://falcon-fpl.com/install.sh | bash
```

To configure an automatic validator reward payout account at install time (`692,000+`):

```bash
curl -fsSL https://falcon-fpl.com/install.sh | bash -s -- --payout-account <your_wallet_account>
```

What the installer does:

1. Checks Linux x86_64, `glibc >= 2.34` (Ubuntu 22.04+, Debian 12+, RHEL 9+), 4 GB+ free disk, 2 GB+ RAM.
2. Downloads `falcon-pl-node` + `falcon-pl-ctl` (`v2.9.81`) from the public releases repository (`github.com/beartec-jpg/falcon-pl-releases`), verifies `SHA256SUMS`, and checks `SHA256SUMS.minisig` against the pinned release public key (`minisign`).
3. Generates the node key **on your machine** (`keys/`, mode `0700`). Secret keys are never printed or transmitted.
4. Installs and starts the `falcon-pl-node` systemd service (with `MALLOC_ARENA_MAX=2`, disk-backed `--body-store-dir`, and automatic `logrotate` configuration) plus the `fdash` terminal dashboard alias.
5. Peers through the public seed endpoints in `config/network-2300.env` (`SEEDS=5.78.142.246:19301,...`), pulls a certified state snapshot, and follows the live chain until within 10 blocks of the network tip.

Re-running the installer is idempotent: unchanged binaries and units are left alone; a newer release is swapped in and restarted cleanly.

### Manage Your Node

| Command | Description |
|---|---|
| `falcon-node status` | Service state, local tip vs. public tip, validator status, seat rewards & payout account |
| `falcon-node balance` | One-line summary: liquid FPL, bonded FPL, seat rewards, account claimable rewards, unbonding (`--account NAME` for another account) |
| `fdash` / `falcon-node dash` | Live terminal dashboard (node, chain, consensus, peers, system, warnings, **Balance** & **Seat rwd**); `q` quits |
| `falcon-node web` | Local browser dashboard on `127.0.0.1:19480` (prints SSH tunnel command for remote servers) |
| `falcon-node logs` | Follow `logs/node.log` (managed via `logrotate`) |
| `falcon-node start` / `stop` / `restart` | Control the systemd service |
| `falcon-node upgrade` | Fetch the latest signed release (`2.9.81+`), verify, swap, and restart |
| `falcon-node uninstall` | Remove service, binaries, and chain data; **keys are kept** |
| `falcon-node uninstall --purge` | Remove everything including keys (irreversible) |

### Seat Payout Account (`692,000+`)

Since height `692,000` (`v2.9.80+`), validator rewards (proposer fee share + epoch pack/vote/floor rewards) are isolated in a per-seat bucket (`ValidatorInfo.seat_claimable`) or credited directly to your designated payout wallet account if configured. See **[SEAT_PAYOUT.md](SEAT_PAYOUT.md)** for full details:

- Pass `--payout-account <account>` (or `--payout-address <account>`) to the installer, or set `PAYOUT_ACCOUNT=<account>` in `config/node.env`.
- Or set/claim manually with `falcon-pl-ctl`:
  ```bash
  falcon-pl-ctl set-seat-payout --validator <id> --payout-account <wallet_account> --keys-dir keys/ --network-id 2300
  falcon-pl-ctl claim-seat --validator <id> --to <wallet_account> --keys-dir keys/ --network-id 2300
  ```

### Directory Layout & Hardware Requirements

Files live under `/var/lib/falcon-pl-2300` (root) or `~/falcon-pl-2300` (user): `bin/`, `keys/`, `data/` (including disk-backed `bodies/`), `logs/`, `config/node.env`, `config/network-2300.env`, and `validator/`.

| Role | CPU | RAM | Disk | Network |
|---|---|---|---|---|
| Observer / Watcher | 1–2 vCPU x86_64 (no AVX-512 needed) | 2 GB (`MALLOC_ARENA_MAX=2`) | 10 GB | Outbound TCP to public seeds (`5.78.142.246:19301..19312`) |
| Validator | 2+ vCPU (SHA-NI recommended) | 4–8 GB (`MALLOC_ARENA_MAX=2`) | 20 GB | Outbound TCP to **every** seat endpoint in `network-2300.env` |

## Validator Onboarding (`VALIDATOR_ONBOARDING=open`)

```bash
curl -fsSL https://falcon-fpl.com/install.sh | bash -s -- --validator --id myname --contact you@example.com --payout-account mywallet
```

Each step is resumable at any time by running `falcon-node validator`:

1. **Observer Sync:** The node installs and syncs to the public tip.
2. **Local Key Generation:** Generates a validator key (`keys/<id>.json`) and a dedicated bond-account key (`keys/<id>-bond.json`) locally.
3. **Application:** Writes `validator/application.json` (id, validator public key + fingerprint, bond account + `bond_account_key_hash`, node version, contact) and prints the application link (`falcon-fpl.com/validator/apply`) to open a pre-filled admission issue in `github.com/beartec-jpg/falcon-pl-validators`.
4. **On-Chain Approval:** Once approved by the admission key (`approve-validator`), the installer detects `status.approvals[<id>]` matching your public key.
5. **Funding (`PayToKey`):** Since v2.9.65 (and enforced for all wallet keys from height `692,000`), new accounts bind the public key committed by their funder via `PayToKey`. The bond account needs **`50,000 FPL` + fees**.
6. **BondV2 (`bond-v2`):**
   ```bash
   falcon-pl-ctl bond-v2 --id <id> --from <id>-bond --amount 50000 --validator-keys keys/ --keys-dir keys/ --network-id 2300
   ```
   Carries the validator public key and a Falcon-512 proof-of-possession signature over `fpl-pop|2300|<id>|<bond_account>`.
7. **Activation:** Once bonded on-chain, the node restarts under the validator ID and verifies that peers report the seat `lottery_ready`.

Leaving or reactivating:
- `falcon-pl-ctl unbond --id <id> --amount <bond>` followed by `complete-unbond` after the 14-day cooldown.
- An offline seat is marked inactive after 20 consecutive missed turns (no bond slash); `falcon-pl-ctl reactivate` brings it back online.

### Validator Connectivity

Seats do not relay other seats' consensus votes or proposals. Every validator dials the filtered public seat ports listed in `SEEDS` inside `network-2300.env` (`5.78.142.246:19301` and per-seat ports `19302..19312`) so it exchanges votes and proposals directly with every active committee member.

## How Rewards Work (Rewards v2 + Operator Bucket at `692,000+`)

- **Transaction Fees (every block):**
  - **30%** of block transaction fees go immediately to the **proposer** (credited to the proposer seat's reward bucket / payout account).
  - **70%** enters the **epoch fee pool** (distributed alongside epoch emissions).
- **Epoch Emissions (every 7-day epoch = `10,000` blocks on 2300, `0.30%` of remaining treasury):**
  - **5% off the top** is carved for active **`BTC4` BitVM2 bridge operators** (`OPERATOR_SHARE_BPS = 500`, active since height `692,000`; see [`docs/bridge/OPERATOR_REWARDS.md`](bridge/OPERATOR_REWARDS.md)).
  - The remaining **95%** splits **55% Validators / 5% Watchers / 20% AMM LPs / 20% Lending LPs** (effective total across all emission: **5% `BTC4` operators / 52.25% validators / 4.75% watchers / 19% AMM / 19% lending**).

| Role | Share | Distribution Rules |
|---|---|---|
| **Validator — Proposer** | **50%** of validator pot + **30%** of block tx fees | Per block sealed in its **own** round (round 0): `64 + min(fee-paying txs, 64)` credits |
| **Validator — Voter** | **40%** of validator pot | `+1` credit per committed block whose QC carries its timely round-0 vote |
| **Validator — Floor** | **10%** of validator pot | Equal per active seat (`×1.05` for archive seats) |
| **Validator Presence & Miss Scaling** | Applied to validator pot | Scaled by active presence (`430,000+`) and catch-up/missed-turn penalty (`441,000+`) |
| **`BTC4` BitVM2 Operators** | **5%** of total epoch emission (top carve) | `5,000 FPL` minimum bond; split across liveness (25%), watching/disprove units (25%), and proven `BTC4` payout volume (50%, capped per operator) |
| **Watchers** | **5%** of post-operator emission (`4.75%` effective) | `1,000 FPL` minimum bond; work credits × hourly presence slots, capped at `0.05%` of epoch emission per account |
| **Liquidity Providers (AMM & Lending)** | **20% AMM + 20% Lending** of post-operator emission (`19%` each effective) | Time-weighted FPL value held across the epoch, capped at `0.50%` of epoch emission per account |

Lottery election odds are equal for every eligible validator seat (`50,000 FPL` minimum bond buys eligibility, not extra lottery weight).

## Verify a Release Manually

```bash
V=2.9.81
B=https://github.com/beartec-jpg/falcon-pl-releases/releases/download/v$V
curl -fsSLO $B/SHA256SUMS -O $B/SHA256SUMS.minisig -O $B/falcon-pl-$V-linux-x86_64.tar.gz
sha256sum -c --ignore-missing SHA256SUMS
minisign -Vm SHA256SUMS -P RWTs+gnuKfzPik0Xbcf+m1hD/JrCUnWqaD/OAZFZEY2fcNc90D67mZlM
```

Release binaries are built reproducibly in `ubuntu:22.04` (Rust 1.85.1 pinned, `RUSTFLAGS` unset, no `target-cpu=native`, zero AVX-512 instructions).
