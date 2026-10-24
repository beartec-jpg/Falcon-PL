# Public testnet profile — `network_id = 9003`

Isolated from local regtest (**9002**) and Falcon Ledger mainnet/testnet IDs.

| Item | Value |
|------|--------|
| `network_id` | **9003** |
| RPC ports | `28001`–`28005` |
| P2P ports | `29001`–`29005` |
| Crypto (default compose) | `hmac-sha256-dev` (swap to Falcon-512 + keys for PQ) |
| User txs | `--require-tx-sigs` + faucet auto-sign |
| Snapshots | `/data/snapshot.json` per validator volume |

## R1 real-build smoke (recommended first run)

```bash
cd falcon-dag-rs
./scripts/real_build_smoke.sh
# → /tmp/fd-real-build-report.json
# see ../../docs/REAL_BUILD.md for R1/R2/R3 readiness
```

## Quick start (Docker)

```bash
cd falcon-dag-rs
./scripts/start-testnet-9003.sh
# wait for healthy
curl -s -X POST http://127.0.0.1:28001 -H 'Content-Type: application/json' \
  -d '{"method":"ping","params":[{}]}'
curl -s http://127.0.0.1:28001/metrics | head
```

Stop:

```bash
./scripts/stop-testnet-9003.sh
```

## Bare metal (5 processes)

```bash
./scripts/start-testnet-9003.sh --bare
```

## Falcon-512 on 9003

```bash
# validator UNL keys
./target/release/falcon-dagd --gen-keys --keys-dir ./deploy/testnet-9003/keys \
  --unl val1,val2,val3,val4,val5
# faucet wallets
./target/release/falcon-dag-wallet gen-keys --dir ./deploy/testnet-9003/wallets --accounts 20
# then run nodes with --crypto falcon-512 --keys-dir ... --wallet-dir ...
```

## Client sign for network 9003

Tx signatures bind `network_id` — always pass `--network-id 9003`:

```bash
falcon-dag-wallet sign --scheme hmac-sha256-dev --network-id 9003 \
  --account acct1 --sequence 1 --destination acct2 --amount 1 --fee 10 --out tx.json
```

## Never reuse

| ID | Product |
|----|---------|
| 1001 / 1026 family | Falcon Ledger (production) |
| 9002 | Falcon DAG local regtest |
| **9003** | Falcon DAG public testnet profile |
