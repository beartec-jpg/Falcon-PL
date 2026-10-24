# Real build attempt — readiness

**Question:** are we close to a *real* build attempt?  
**Short answer:** **yes for a private multi-node testnet**; **no for mainnet / real BTC–ETH custody**.

Falcon DAG is an **isolated research chain** (`network_id` 9002 regtest / **9003** public-testnet profile). Production Falcon Ledger (`qXRP`) is **not** touched.

---

## What “real build” means (three rungs)

| Rung | Meaning | Ready? |
|------|---------|--------|
| **R1 — Private multi-node** | 5 validators, P2P, snapshots, signed txs, sealed protocol rules | **Yes — run now** (`real_build_smoke` / testnet-9003) |
| **R2 — Public play testnet** | dual-shard 9003/9004, faucet, facade, PQ Falcon-512, no real money | **Yes — run `deploy/public-testnet`** (explorer UI optional) |
| **R3 — Real external value** | Live SPV against mainnet BTC/ETH/BNB/XRP + production lock contracts with cert-gated claims | **Not yet** — months of proof codecs, lock deploy, security review |

---

## What is already solid (R1)

| Area | Status | Evidence |
|------|--------|----------|
| Node binary + RPC | Green | `falcon-dagd`, Docker image, CI |
| Multi-validator DA / order | Green | `multinode_smoke`, compose 5 vals |
| Snapshot restart | Green | payments + **rail claim durability** |
| Sealed Pool / Vault / treasury | Green | `defi_smoke` / `vault_smoke` |
| Economy (issue/mint/burn/fees) | Green | `economy_smoke` |
| SPV rails substrate (4 rails) | Green | synthetic mint + withdraw + claim_root |
| No federated mint | Green | free Issue/Burn → `tecPROTOCOL` |
| Claim codec = EVM lock | Green | Rust + Python goldens + **Foundry** |
| CI gates | Green | rust-ci smokes + live indexer schedule + evm-ci |

---

## What is *not* ready for R3 (honest gaps)

| Gap | Why it blocks real value |
|-----|---------------------------|
| **Placeholder `lock_id`** | Genesis uses `sha256("falcon-spv-lock-v1:{RAIL}")` — not a deployed contract/script hash |
| **Open `submitFalconRoot`** | Anyone can post a root; wrong roots fail claims, but production needs Falcon finality verify on-chain |
| **Light-client codecs** | EVM/XRP still substrate pairing; full MPT / XRPL inclusion is incomplete |
| **Live mint path** | Live indexer is headers + optional dry-run watches — not a production deposit pipeline |
| **HMAC-dev crypto default** | Fine for private net; public/money needs Falcon-512 + key ceremony |
| **No external audit / threat model freeze** | Optics of “authors can’t drain” need third-party review before value |
| **Ops** | Alerting, upgrades, peer discovery, public RPC, faucet rate limits |

---

## Recommended next real attempts

### Attempt A — **now** (R1): private 5-node 9003

```bash
cd falcon-dag-rs
./scripts/real_build_smoke.sh          # bare multi-node + protocol checks
# or interactive:
./scripts/start-testnet-9003.sh --bare
# RPC http://127.0.0.1:28001 … 28005
```

Success criteria: all validators ping, cross-node payment executes, free Issue blocked, rail synthetic mint+withdraw+claim_root, one validator restart keeps state.

### Attempt B — **now** (R2): public play testnet

**Package:** `deploy/public-testnet/` → network name **`falcon-dag-testnet-1`**

```bash
export SSH_F2=<peer-host>
bash deploy/public-testnet/scripts/start.sh
bash deploy/public-testnet/scripts/readiness.sh   # expect READY
```

Includes: dual-shard Falcon-512 mesh, facade, rate-limited faucet `:18550`, frozen genesis, watchdog, `PUBLIC_TESTNET.md`.  
Still foundation-run seed UNL (not open stake join). No real money.

### Attempt C — **later** (R3): real rails

1. Deploy lock contracts/scripts; set genesis `lock_id` to deployment hash.
2. Cert-gated `submitFalconRoot` (or challenge window).
3. Full family SPV codecs; live deposit indexers with multi-RPC + monitoring.
4. Security review of sealed accounts + claim trees + light clients.

---

## One-line status for stakeholders

> **Falcon DAG can stand up a real private multi-node network today with protocol-controlled DeFi and SPV *accounting*. It is not yet a venue for real BTC/ETH — lock finality and production light clients remain the main gap.**

Build version marker: `falcon-dagd/0.35.0-rust` (see node `server_info`).

### Sequence window (v0.35)

Mempool **admits** a bounded open range of sequences per account (`account_sequence_window`, default **256**):

- Execute still requires **exact next** sequence (no holes).
- Clients may pre-submit checks 1…N without waiting for each to clear.
- Past sequences and beyond the window still reject.
