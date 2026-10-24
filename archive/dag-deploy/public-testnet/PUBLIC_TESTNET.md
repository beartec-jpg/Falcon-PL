# Falcon DAG — Public play testnet (`falcon-dag-testnet-1`)

**Tier:** R2 public play testnet (not mainnet, **no real money**).  
**Crypto:** Falcon-512 (post-quantum).  
**Architecture:** 2 shards × N=3 multipack + GMP leases + facade router + cross-shard lab bridge.

This is a **legit public testnet environment**: frozen genesis, public entrypoints, faucet, smoke/health gates, operator runbooks. It is **not** a production financial product.

---

## Quick start (users)

### Endpoints (seed / foundation host)

Replace `HOST` with the published seed IP/DNS (see `genesis/bootstrap.json` on seed, or ask operators).

| Service | URL |
|---------|-----|
| **Facade** (preferred client entry) | `http://HOST:18499` |
| **Faucet** | `http://HOST:18550` |
| Public RPC shard 0 | `http://HOST:18001` |
| Public RPC shard 1 | `http://HOST:18004` |

### 1. Get test FDAG

```bash
curl -sS -X POST http://HOST:18550/v1/drip \
  -H 'Content-Type: application/json' \
  -d '{"destination":"acct7","amount":1000}'
```

Limits (default): 20 drips/IP/hour, 5/destination/hour, max 1e6 units/drip.

### 2. Route an account

```bash
curl -sS -X POST http://HOST:18499/v1/route \
  -H 'Content-Type: application/json' \
  -d '{"account":"acct3"}'
# → shard 0 for acct1–20, shard 1 for acct21–40
```

### 3. Submit a payment (via facade)

```bash
curl -sS -X POST http://HOST:18499/v1/submit \
  -H 'Content-Type: application/json' \
  -d '{
    "account": "acct3",
    "sequence": 1,
    "destination": "acct4",
    "amount": 1,
    "fee": 10,
    "client_id": "my-unique-id-1"
  }'
```

- Same-shard: immediate GMP accept → packers execute.  
- Cross-shard (e.g. acct3→acct23): poll `GET /v1/xshard/{xfer_id}`.  
- Use stable `client_id` for idempotency.

### 4. Check balances

```bash
curl -sS -X POST http://HOST:18001/ \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"account_info","params":[{"account":"acct3"}]}'
```

### 5. Optional: client wallet (pre-sign)

On seed validators, unsigned faucet submits are **auto-signed** for convenience.  
For client-side Falcon-512 signing:

```bash
falcon-dag-wallet gen-keys --dir ./wallets --accounts 40
falcon-dag-wallet sign --scheme falcon-512 --wallet-dir ./wallets \
  --network-id 9003 --account acct3 --sequence 1 \
  --destination acct4 --amount 1 --fee 10 --out tx.json
# submit signed body via JSON-RPC `submit` on the account's shard RPC
```

---

## Network identity

| Field | Value |
|-------|--------|
| Name | `falcon-dag-testnet-1` |
| Shard 0 `network_id` | **9003** |
| Shard 1 `network_id` | **9004** |
| Accounts | s0: acct1–20 · s1: acct21–40 |
| Bridges | s0: acct20 · s1: acct40 |
| Genesis balance | 1e9 FDAG per acct (lab faucet genesis) |
| UNL s0 | val1, val2, val3 |
| UNL s1 | val4, val5, val6 |
| Public key pin | `genesis/unl_public.json` + sha256 in `network.json` |

**Do not** reuse IDs 1001/1026 (Falcon Ledger) or 9002 (local regtest).

---

## Operator: run the seed network

```bash
export SSH_F2=<f2-host>   # second machine for remote UNL thirds
export F2_IP=<f2-lan-ip>

bash deploy/public-testnet/scripts/start.sh
bash deploy/public-testnet/scripts/readiness.sh   # must print READY
bash deploy/gmp/status.sh

# stop (leave watchdog unless --all)
bash deploy/public-testnet/scripts/stop.sh
bash deploy/public-testnet/scripts/stop.sh --all
```

What `start.sh` brings up:

1. Dual-shard multipack mesh (GMP + validators + archival)  
2. Facade `:18499`  
3. Faucet `:18550`  
4. Watchdog (health + smoke + restart on DOWN)  
5. Frozen genesis copy under `~/falcon-dag-mesh/testnet/`  

---

## What “public testnet ready” means (this product)

| Capability | Status |
|------------|--------|
| Multi-host multi-validator consensus | Yes |
| Falcon-512 + signed txs | Yes |
| Stable client facade API | Yes |
| Same-shard payments | Yes |
| Cross-shard (lab bridge + refund/idempotency) | Yes |
| Rate-limited faucet | Yes |
| Health / smoke / watchdog | Yes |
| Frozen genesis + bootstrap template | Yes |
| User-facing docs | Yes |
| Open validator self-join with stake | **No** (seed UNL; join request is social) |
| Permissionless peer discovery (DHT) | **No** (bootstrap list) |
| Block explorer UI | **No** (JSON RPC/status only) |
| Real BTC/ETH bridges | **No** (synthetic rails only) |
| Mainnet economics / slash | **No** |

Seed UNL is **foundation-operated** for testnet stability. Third parties can build **clients** and (later) apply to join UNL; committees/open registry is the next track.

---

## Expected metrics (testnet)

| Condition | exec_tps (approx) | Notes |
|-----------|-------------------|--------|
| Idle | ~0 | smoke/faucet only |
| Light | 15–25 / shard | |
| Busy single shard | ~35–45 | N=3 multipack |
| Dual shard local load | sum mid‑40s+ | additive when saturated |
| Cross-shard settle | 1–10 s | lab bridge path |

Do **not** grow one UNL to raise TPS; add shards or keep N small.

---

## Failure / support

| Symptom | Check |
|---------|--------|
| Faucet 429 | Rate limit — wait or use another dest |
| Faucet 400 seq | Operators restart clears faucet_seq; re-drip |
| xshard stuck | `GET /v1/xshard/{id}`; bridge balances `/v1/bridges` |
| Network DOWN | Operators: `status.sh`, `watchdog.log` |

---

## Version

- Node: `falcon-dagd` (see `server_info` → `build_version`)  
- Genesis package version: see `genesis/network.json` → `version`  
- Tier label: **public-play-testnet-R2**
