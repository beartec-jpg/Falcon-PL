# Rule-bound rail agent + indexers

**Goal:** anything a human would do as a relayer is an **agent that only posts protocol-valid txs**.  
There is **no committee** and **no discretionary mint**.

```text
External chain          Indexer (observe)         Falcon node
──────────────          -----------------         -----------
blocks / locks    →     headers + proofs    →   submit_rail_header
                                                rail_deposit
                              │                      │
                              └──── tecPROTOCOL? ────┘
                                   agent stops
```

## Indexers (`scripts/rail_indexers/`)

| Module | Rail | Source |
|--------|------|--------|
| `synthetic.py` | all | Local deterministic proofs (smoke) |
| `btc.py` | BTC | Esplora/Blockstream HTTP (headers + `--btc-watch-address` SPV deposits) |
| `evm.py` | ETH/BNB | JSON-RPC `eth_getBlockByNumber` |
| `xrp.py` | XRP | XRPL JSON-RPC ledgers + Payment deposit trees (`--xrp-watch-account`) |

Indexers **never** hold Falcon mint authority. Invalid proofs → `tecPROTOCOL`.

## Merkle families (Falcon)

| Family | Verify |
|--------|--------|
| Bitcoin (`BTC`) | 32-byte hex leaves, **dsha256** parent (Bitcoin-style) |
| Evm / XrpLedger | Protocol pairing substrate until full MPT / XRPL codec |

## Agent CLI

```bash
# Synthetic e2e (local falcon-dagd)
python3 scripts/rail_agent.py --rpc http://127.0.0.1:18811 --rail BTC --indexer synthetic

# Live headers near chain tip (no mint without watch/lock proofs)
python3 scripts/rail_agent.py --rpc http://127.0.0.1:18811 --rail BTC --indexer live --max-headers 2
```

### Live dry-run smoke (public APIs)

```bash
# Headers only (default)
./scripts/live_indexer_smoke.sh

# Optional live deposits (env-gated; still no free Issue)
BTC_WATCH=bc1q... ./scripts/live_indexer_smoke.sh
ETH_LOCK=0x... BNB_LOCK=0x... ./scripts/live_indexer_smoke.sh
XRP_WATCH=r... ./scripts/live_indexer_smoke.sh

REQUIRE_ETH=1 ./scripts/live_indexer_smoke.sh
REQUIRE_DEPOSIT=1 BTC_WATCH=bc1q... ./scripts/live_indexer_smoke.sh

ETH_RPC=https://eth.llamarpc.com,https://1rpc.io/eth ./scripts/live_indexer_smoke.sh
```

| Env | Effect |
|-----|--------|
| `BTC_WATCH` | Esplora SPV deposits to address |
| `ETH_LOCK` / `BNB_LOCK` | FalconRailLock `Deposit` log scans |
| `XRP_WATCH` | XRPL Payment destination |
| `REQUIRE_DEPOSIT=1` | Fail if watch set but zero candidates |
| `MAX_DEPOSITS` | Cap deposit attempts (default 5) |
| `CI_SOFT_NETWORK=1` | Exit 0 if no headers (outage) after protocol Issue check |

Without watch envs, `total_minted` stays 0. With watches, mint only via valid proofs.

### CI gates

**PR / push (`falcon-dag-rs CI`)** — offline synthetic:

- `economy_smoke`, `vault_smoke`, `defi_smoke`, **`bridge_smoke`** (BTC/ETH/BNB/XRP mint + multi-rail withdraw + sealed burn + claim export)
- **`rail_restart_smoke`** — deposit/withdraw/`claim_root`/spent-set survive `--data-dir` snapshot restart; free Issue/Burn stay `tecPROTOCOL`; double deposit blocked
- **`claim_codec_smoke`** — golden claim leaf/root vectors + live multi-note `claim_export` parity with `FalconRailLock.sol`
- **Foundry (`evm-ci`)** — `forge test` on `contracts/evm` (codec + recipient bind + liquidity + stale height)

**Schedule (`live-indexer-smoke`)** — public APIs:

- Builds release `falcon-dagd`, runs `scripts/live_indexer_smoke.sh`
- **`CI_SOFT_NETWORK=1`**: total public-API outage does not fail the job after protocol checks
- **Hard-fail**: free `Issue` not `tecPROTOCOL`, or unexpected mint without watch
- Optional repo secrets: `BTC_WATCH`, `ETH_LOCK`, `BNB_LOCK`, `XRP_WATCH`, `ETH_RPC`, `BNB_RPC`

## Withdrawal claims (no committee)

After every `RailWithdraw`, Falcon **auto-commits** `rail.claim_root` (merkle over open notes).  
That root is a pure function of public state — not a federation signature. Agents recompute and must match `claim_root_matches: true` on `rail_info`.

After `RailWithdraw`, notes are public on Falcon:

```bash
# list open notes + claim_root (sha256 protocol tree)
curl -sX POST $RPC -H 'Content-Type: application/json' \
  -d '{"method":"withdrawal_notes","params":[{"rail":"BTC"}]}'

# single note + merkle path for agents
curl -sX POST $RPC -H 'Content-Type: application/json' \
  -d '{"method":"claim_export","params":[{"note_id":"<id>"}]}'

# EVM keccak path for FalconRailLock.sol
python3 scripts/rail_indexers/claim_export.py --rpc $RPC --rail ETH --evm
```

Anyone recomputes the same root from the note list — **not** a federation signature.

## EVM lock + outbound claim loop

See [`contracts/evm/FalconRailLock.sol`](../contracts/evm/FalconRailLock.sol).

```bash
# After RailWithdraw: verify committed claim_root + pack EVM txs
python3 scripts/rail_indexers/evm_outbound_claim.py --rpc $RPC --rail ETH
```

Codec is shared **sha256** leaf/parent with Falcon (not committee signatures).

`claim_root` is included in Falcon **`state_root`**. Agents use:

```bash
curl -sX POST $RPC -d '{"method":"bridge_status","params":[{}]}'
# hard_final_batches + state_root + per-rail claim_root_matches
```

Only pack foreign claims when `ready_for_foreign_claim` and hard finality has advanced.

### BTC inbound (live SPV)

```bash
python3 scripts/rail_agent.py --rpc $RPC --rail BTC --indexer live \
  --btc-api https://blockstream.info/api \
  --btc-watch-address bc1q... \
  --max-headers 3
```

- Headers use real `merkle_root` / block id / prev  
- Deposits: payments to watch address + Esplora `/tx/{txid}/merkle-proof`  
- `external_txid` = **display** txid; Falcon reverses bytes for internal dsha256 leaf  
- `lock_id` from `rail_info` (must match genesis protocol lock id)  
- Invalid proofs → `tecPROTOCOL`

### EVM inbound (Deposit → Falcon mint)

When `--lock-contract` is set, the live indexer:

1. Reads `Deposit` logs per block for the protocol `lock_id`
2. Builds **deposit_root** = binary-sha256 merkle of deposit leaves  
   (`sha256(tx32‖logIndex_be8‖amount_be8‖lock32)`)
3. Reads real `transactionsRoot` as **tx_root**
4. Sets header `merkle_root` = **bind** `sha256(tx_root‖deposit_root)` and stores `tx_root`  
   (so the deposit tree is bound to the real EVM block, not only an agent-built tree)
5. `RailDeposit` proves into `deposit_root`; Falcon checks bind against header

### XRP inbound (Payment → Falcon mint)

Same bind shape as EVM:

1. `deposit_root` = binary-sha256 tree of protocol Payments to `--xrp-watch-account`  
   leaf = `sha256(tx32‖amount_be8‖lock32‖sha256(destination))`
2. `tx_root` = ledger `transaction_hash`
3. header `merkle_root` = `sha256(tx_root‖deposit_root)`
4. `RailDeposit` with `external_txid = "{txhash}:{destination}"`

```bash
python3 scripts/rail_agent.py --rpc $RPC --rail XRP --indexer live \
  --xrp-watch-account rXXXXXXXX --xrp-rpc https://xrplcluster.com
```

Synthetic paths (all four genesis rails):

```bash
python3 scripts/rail_agent.py --rpc $RPC --rail BTC --indexer synthetic
python3 scripts/rail_agent.py --rpc $RPC --rail ETH --indexer synthetic --amount 5000
python3 scripts/rail_agent.py --rpc $RPC --rail BNB --indexer synthetic --amount 3000
python3 scripts/rail_agent.py --rpc $RPC --rail XRP --indexer synthetic --amount 4000
```
