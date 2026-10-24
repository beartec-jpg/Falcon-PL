> **HISTORICAL.** This file is not the live Falcon PL **2300** dest-lock specification.
> Current: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md) and [README.md](README.md).
> Falcon Ledger **1001** lock-mint is shut down.

# Falcon PL — Expansion & Launch Plan

| | |
|--|--|
| **Product** | Falcon PL (Falcon Participation Ledger) |
| **Ticker** | **FPL** |
| **Consensus** | **Falcon Consensus** |
| **Signatures** | **Falcon-512** |
| **This plan** | **v2.2** private testnet line → **v2.3** first **public** testnet release |
| **Status** | **LOCKED** versioning + product surface (change only via AMD / 2.2.x notes) |

---

## 1. Version roadmap (locked)

| Version | Name | Audience | Goal |
|---------|------|----------|------|
| **v2.1.x** | Consensus PoC | Internal | Lottery packer, multi-host Falcon load (done / POC.md) |
| **v2.2.0** | **Private testnet baseline** | **Private Falcon servers only** | **Good enough to run private testnet**: consensus multi-host, wallets, vaults, pools/AMM, lend, emissions, watchers, bridge **framework** (BTC primary; ETH/BNB scaffold) |
| **v2.2.1, .2, .3, .4…** | Private harden / add | Private only | **Fixes + additions while private testnet runs** — bugs, bridge finalisation, UX, soak gaps. **Do not jump to 2.3** for these |
| **v2.3.0** | **Public testnet release** | Public | First **public** testnet cut: frozen sheet from surviving 2.2.x, public network_id/genesis, public RPC/faucet/wallet, retire Falcon Ledger v1 public testnet |

### 1.1 Versioning rules (operator lock)

| Rule | Meaning |
|------|---------|
| **2.2.0 = private go-live** | Start private testnet on 2.2.0 once multi-host tip-agree + core surface smoke pass |
| **2.2.x only while private** | Every fix and every new rail/feature during private testing is **2.2.1, 2.2.2, …** — not 2.3 |
| **2.3.0 = public release version** | Bump to **2.3.0 only** when operators declare private testnet successful enough for **public** testnet |
| **No “mainnet” marketing** | 2.2 = private dress rehearsal; 2.3 = public testnet; mainnet is a later series |
| **Rails not “all final” in 2.2.0** | BTC framework in baseline; ETH/BNB/XRP/USDC cross-network finalisation is **2.2.x work**, then carried into 2.3 when proven |

**One-liner:** *2.2.0 opens private testnet; 2.2.x hardens it; 2.3.0 is the public testnet release.*

---

## 2. Locked product identity

| Item | Value |
|------|--------|
| Product | **Falcon PL** |
| Long name | Falcon Participation Ledger |
| Ticker | **FPL** |
| Consensus name | **Falcon Consensus** |
| Crypto | **Falcon-512** always-on for txs + packer seals |
| Not | XRPL fork · DAG · Falcon Ledger (qXRP) consensus |

**One-liner:** Falcon-512 is crypto; Falcon Consensus is agreement rules; Falcon PL (FPL) is the product.

---

## 3. Locked Falcon Consensus (v2.2.0)

| Parameter | Locked value |
|-----------|--------------|
| Eligible set | Bonded, non-jailed validators |
| Lottery | Deterministic from network_id + height + tip + skips + bonds (equal bond weight day-1) |
| Pack | Time budget **1000 ms**; **no empty seals** |
| Soft OK | **3** distinct OK votes (or all checkers if fewer) |
| Hard OK | **0.6** of bonded non-jailed checkers |
| Progress timeout | **1000 ms** silent #1 → skip → #2 packs |
| Pipeline | Up to **3** soft-path seals per tick when still winner |
| Equivocation | Same packer + height + different hash → **100% bond slash** + jail |
| Mempool | Gossip new txs; **max_mempool_txs = 50_000** reject when full |
| History | **Prune/snapshot required** before long private soak (OOM lesson) — keep recent window + snapshots |

Packers and checkers are the **same bonded nodes**.

---

## 4. Locked tokenomics (v2.2.0)

### 4.1 Supply

| Item | Locked |
|------|--------|
| Hard cap | **200,000,000,000** FPL |
| Treasury | **98%** (196B), **keyless** |
| Genesis circulating | **2%** (4B): **2B** community/airdrop · **1B** faucet · **1B** builder |
| Community split (proposed, subject to change) | **1B** pre-mainnet airdrop (testnet snapshot, credited at genesis) · **1B** release-phase giveaway (mainnet epochs 1–7, per-epoch caps) |
| Builder pot use (proposed) | **12M** First-35 prize seats, separate from the airdrop |
| Operators | Separate operators pool for validators (size in the parameter sheet) |

### 4.2 Epochs & emission

| Item | Locked |
|------|--------|
| Epoch length | **7 days wall-clock** (not tip-dependent) |
| Bootstrap | Epochs **1–7** claimable emission **0**; first claimable pool **epoch 8** |
| Emission formula | `epochEmit = treasuryBalance × emissionBps / 10_000` |
| First claimable bps | **30 bps** (0.30%) — gentler than FL testnet 50 bps; still large, reassess after v2.2 soak |
| Year-1 target shape | CID decline toward ~**23 bps** average-ish; long-term floor ~**3 bps** (exact curve table in impl constants) |
| Claims | Pull-based per sector; hard-capped to remaining epoch allocation |

**First claimable epoch math (at full 196B treasury, 30 bps):**

```text
epochEmit ≈ 196B × 0.003 = 588,000,000 FPL (~588M)
```

### 4.3 Top-level split of `epochEmit`

| Bucket | Share | Remainder if unused/capped |
|--------|-------|----------------------------|
| **Validators** | **55%** | — |
| **Bridge watchers** | **5%** | → validators |
| **AMM LPs** | **20%** | → validators |
| **Lending LPs** | **20%** | → validators |

### 4.4 Per-account caps

| Role | Max of `epochEmit` per distinct account |
|------|----------------------------------------|
| AMM LP | **0.5%** |
| Lending LP | **0.5%** |
| Watcher | **0.05%** (governance ceiling **0.1%**) |

At 588M emit: LP cap ≈ **2.94M**; watcher cap ≈ **294k**.

### 4.5 Fees

| Slice | Locked launch value |
|-------|---------------------|
| Burn | **50%** of fee (fixed at v2.2; dynamic 40–70 later if needed) |
| Validator pot | **50%** of fee |

No fee share to LPs at launch (swap/borrow protocol fees can tip LPs later as DeFi fee, not base payment fee).

### 4.5b Fee escalation & metrics (AMD-20260811-1)

Congestion market **without** dynamic burn (dynamic burn remains §9 non-goal).

| Item | Locked v2.2.0 |
|------|----------------|
| Base min fee | `min_fee` (default **1**) |
| Escalation | **On** by default (`fee_escalate=true`) |
| Signal | Mempool util = `len / max_mempool_txs` |
| Multipliers | **1×** 0–25% · **2×** 25–50% · **4×** 50–75% · **8×** 75–90% · **16×** ≥90% |
| Enforcement | Submit rejects `fee < suggested_min_fee` (dupes exempt) |
| Metrics (status) | `params_version`, submit accept/reject counters, `fee_p50`/`fee_p90`, `fee_market{}` |
| RBF | **Not** in v2.2 (track pressure only) |

Change of multipliers/tiers → **Class B amendment** ([AMENDMENTS.md](AMENDMENTS.md)).

### 4.6 Inside validator pot

```text
validator_pot = 55% emit + LP/watcher remainders + fee reward share

  ├─ 50% pack_half  → weight = txs_packed (accepted non-empty seals only)
  └─ 50% check_half → weight = qualifying OK votes (1 per ledger per node)
```

- **No per-win instant pay.**  
- Lottery win alone does not pay.  
- Full-history edge: **off in v2.2.0** (revisit after private soak).

### 4.7 Watchers (presence × work)

| Item | Locked |
|------|--------|
| Pot | 5% of epochEmit |
| Formula | `weight = work × presence` |
| Presence | Hourly **slots** over 7d (168 slots); slot active if work or paid heartbeat |
| Work | Accepted useful `SubmitRailHeader` / `RailDeposit` (dupes 0) |
| Heartbeat | Optional tiny-fee; **presence only**, not work |
| Idle tab / no work | **0** |
| Full-epoch active + work | Full pro-rata then **cap** |
| 1-minute peek | Dust presence |

### 4.8 Bonding

| Item | Locked |
|------|--------|
| Min bond | **1,000 FPL** |
| Unbond cooldown | **14 days** (testnet may use 1 day flag for private fleet only) |
| Slash | Equivocation 100% enforced; other offenses deferred |

### 4.9 No activity rule

- No fees ⇒ no fee distribution.  
- No qualifying work in a sector ⇒ that sector’s claim is 0; unused emission slice → validators (except val base always available to workers who did work).

---

## 5. Locked feature surface for v2.2.0 (must test on private fleet)

### 5.1 Accounts & vaults

| Feature | Spec |
|---------|------|
| Hot | Default spendable |
| Vault | Allowlist max **1** destination (single-send cold); `VaultLock` irreversible |
| Timed | Optional allowlist + velocity (test if time) |
| Pool | Outflows only via AMM ops |
| LendingMarket | Outflows only via market ops |

### 5.2 DeFi

| Feature | Spec |
|---------|------|
| AMM | CreatePool, Add/RemoveLiquidity, Swap (constant-product v1), min_out |
| Lending | CreateMarket, Supply, Withdraw, Borrow, Repay, Liquidate |
| Oracle | Simple **OracleVote** median for v2.2 private; harden before v2.3 if fragile |

### 5.3 Bridges

| Feature | Spec |
|---------|------|
| Rails hardcoded | **BTC** first; ETH/BNB stubs or second if private time allows |
| Model | Custodialess SPV / light-client; mint only on verified proof |
| Relays | Unprivileged; **no custody keys** on relay hosts |
| Watchers | Browser + VPS; same pot |
| Port | Falcon Ledger live **BTC SPV** learnings / paths |

### 5.4 Wallets & UX (private)

| Surface | Requirement |
|---------|-------------|
| Keys | Falcon-512 account create / backup / restore |
| Pay | Send/receive FPL + rail assets when live |
| Vault UI | Create vault, set single dest, lock |
| DeFi UI | Minimal AMM + lend flows (can be ctl/RPC first, then wallet) |
| Watcher | Wallet-attached watcher session + claim |
| Validator | Bond / claim pack+check rewards |

### 5.5 Emissions testing matrix (private — required)

Prove **each sector** pays correctly under controlled scenarios:

| Sector | Test cases |
|--------|------------|
| **Pack half** | Equal work vs skewed `txs_packed` (big machine more) |
| **Check half** | OK vote counts; no vote ⇒ no check share |
| **Watcher pot** | Full-week presence+work vs 1-min; idle heartbeats only = 0; cap binds |
| **AMM LP** | 1 LP (cap binds, remainder→val); 5 LPs; ≥40 toward full 20% |
| **Lend LP** | Same as AMM |
| **Remainders** | Empty LP/watcher buckets inflate val pot |
| **Quiet epoch** | No txs ⇒ fee share 0; claims only for real work |
| **Bootstrap** | Epochs 1–7 claim emission 0 |

### 5.6 Scoring (private)

| Score | Definition in v2.2.0 |
|-------|----------------------|
| Pack score | Epoch `txs_packed` |
| Check score | Epoch OK votes |
| Watcher score | `work × presence` |
| LP score | Time-weighted liquidity / supply share |
| **Not in v2.2** | Full FL latency EMA composite (optional v2.2.x if needed) |

Dashboard/RPC: expose per-account epoch counters before claim.

---

## 6. Private fleet (v2.2.0) — “main style” but closed

### 6.1 Environment

| Item | Locked intent |
|------|----------------|
| Hosts | **Private Falcon servers** only (no public DNS marketing) |
| Topology | ≥ **3** bonded validators (prefer **5**); ≥1 full-history capable box; ≥1 RPC; ≥1 watcher host; optional browser watchers |
| network_id | **New** (e.g. **2200** for private expansion) — **not** Falcon Ledger 1001 |
| Data | Fresh genesis for v2.2.0 (do not mix FL ledger history) |
| Secrets | Private keys on servers; not in git |

### 6.2 Function test checklist (exit criteria for v2.2.0 → freeze)

All must pass on private fleet before planning v2.3.0:

- [x] Multi-node tip agree under load (no sustained fork) — dual-host + shared-skip; see PRIVATE_GATE
- [x] Bond / unbond / slash path
- [x] Epoch tick + all claim types (val pack/check, watcher, AMM, lend)
- [x] Emission math matches §4 within rounding — EMISSION_MATRIX.md
- [x] Vault single-dest + lock enforcement (reject free payment)
- [x] AMM lifecycle + LP claim
- [x] Lending lifecycle + LP claim + liquidate smoke
- [x] BTC rail deposit/withdraw smoke (operator-custodial path; not custodialess, see BTC_POOL_FREEZE_2026-10-16.md)
- [x] Watcher presence×work + cap
- [x] Wallet: create, backup, pay, claim — pl-wallet-stage + falcon-512 dress
- [x] History prune / no OOM on agreed soak profile
- [x] Adversarial: double-sign slash; progress timeout standby
- [x] Document parameter deltas actually used vs this file — network_id **2200**, PRIVATE_GATE

### 6.3 Soak profiles (private)

| Profile | Purpose |
|---------|---------|
| Steady 20–80 tx/s | Baseline |
| Spike 150–500 | Keep-up vs soft wall |
| Epoch boundary | Emission + claims under load |
| Bridge event days | Deposit bursts + watcher competition |

---

## 7. v2.3.0 — first public testnet

### 7.1 After private alterations

1. Freeze **v2.2.x → v2.3.0** parameter sheet (copy of this doc + changelog).  
2. New public **network_id** (e.g. **2300**).  
3. Clean genesis ceremony (2% buckets, treasury, rails).  
4. Public RPC + bootstrap peers.  
5. **Retire Falcon Ledger v1 public testnet** (network 1001 / current FL testnet).  
6. **Rewire faucet + wallet infrastructure** to FPL v2.3.0 endpoints, assets, claim flows.

### 7.2 Falcon Ledger v1 retirement

| Step | Action |
|------|--------|
| Announce window | “FL v1 testnet deprecated as of v2.3.0 go-live” |
| Stop relays / faucet against FL | Cut env to FPL |
| Archive FL testnet docs/endpoints | Mark historical |
| Do **not** auto-migrate balances | Testnets are disposable unless explicit snapshot program (none by default) |
| Keep FL codebase | Separate product history; FPL is greenfield |

### 7.3 Faucet & wallet cutover (v2.3.0)

| Component | Action |
|-----------|--------|
| Faucet | Point to FPL RPC; drip **FPL**; fund from faucet genesis bucket |
| Web wallet | Network config → FPL chain id / RPC; Falcon-512 keys; pay/vault/DeFi/watcher entry points as ready |
| Validator installers | New one-liners for FPL bond/claim (not qXRP 1001) |
| Bridge UI | FPL rails only; no FL mint issuers |
| Env secrets | New keys; never reuse FL mainnet-bound material |

### 7.4 Public testnet exit criteria (soft)

- Public can faucet, send, (as features enable) LP/watch/claim  
- Docs: IDENTITY, POC, this plan, public endpoints  
- Monitoring: tip, peers, epoch, faucet balance  

---

## 8. Implementation workstreams (v2.2.0)

| Stream | Deliverables |
|--------|----------------|
| **WS0 Stability** | Snapshot/prune, durable store, multi-host soak |
| **WS1 Consensus+bond** | Bond objects, epoch clock, counters, claims |
| **WS2 Accounts** | Vault/Timed/Pool/LendingMarket enforcement |
| **WS3 AMM** | Pool ops + LP claims |
| **WS4 Lend** | Market ops + LP claims + oracle v0 |
| **WS5 Bridge** | BTC SPV + watcher slots/work + ClaimWatcher |
| **WS6 Wallet/faucet** | Private wallet against fleet; faucet staging |
| **WS7 Ops** | Deploy scripts on Falcon private servers, metrics |

Order: **WS0 → WS1 → WS2 → WS3/WS4 parallel → WS5 → WS6 → private gate §6.2**.

---

## 9. Explicit non-goals until after v2.3.0

- Public mainnet  
- Dynamic fee burn curve  
- Perf-weighted lottery election  
- Full-history emission class  
- Paying unbound RPC/archive from emission  
- Custody bridge operators as security model  

---

## 10. Document control

| Doc | Role |
|-----|------|
| [IDENTITY.md](IDENTITY.md) | Naming |
| [POC.md](POC.md) | v2.1 consensus PoC results |
| [FEATURES_AND_TOKENOMICS.md](FEATURES_AND_TOKENOMICS.md) | Long-form design (aligned to this lock) |
| [AMENDMENTS.md](AMENDMENTS.md) | **How changes are proposed, decided, recorded** |
| **This file** | **v2.2.0 / v2.3.0 expansion & launch lock** |

**Amendment rule (summary):** See **[AMENDMENTS.md](AMENDMENTS.md)** for full process.

| Phase | Who decides Class B/C locks |
|-------|----------------------------|
| v2.2 private | Falcon **private operators** (written multi-ack; not token vote) |
| v2.3 public testnet | Operators + public changelog; ≥1 epoch notice when practical |
| Mainnet | **Not designed** here |

Every Class B–E change gets `AMD-YYYYMMDD-N`, a row in AMENDMENTS §8, a line here in §11, and a `params_version` bump when parameters change in code.

---

## 11. Changelog

| Date | Note |
|------|------|
| 2026-08-11 | Initial lock: v2.2.0 private full expansion; v2.3.0 public testnet; FL v1 retire + faucet/wallet rewire |
| 2026-08-11 | **Impl start:** `economy.rs` (emission 55/5/20/20, caps, settle, presence×work weights); `accounts.rs` (Vault single-dest); chain **prune** (`max_chain_ledgers`); epoch auto-settle + claimable; fee burn 50%; pack/check epoch counters; status exposes FPL fields. Tests: 21 pass. Next: AMM/lend ops, wire claims to ctl, BTC rail, private deploy. |
| 2026-08-11 | **Continue:** `defi.rs` AMM (x*y=k) + lending LTV; LP weights into epoch settle; WireMsg vault/claim/pool/swap/market/watcher/tick; `falcon-pl-ctl` subcommands; watcher credit API. **25 tests pass.** Next: BTC SPV rail, private fleet deploy, wallet wire-up. |
| 2026-08-11 | **AMD-20260811-1:** Fee escalation metrics + util-tier min fee (`metrics.rs`, status `fee_market`). **AMD-20260811-2:** Amendment process locked in [AMENDMENTS.md](AMENDMENTS.md). BTC rails + snapshots + private fleet script earlier same day. `params_version=2`. |
| 2026-08-12 | **AMD-20260812-1 / v2.2.1:** Snapshot schema 2 — persist pruned chain tail + atomic write; restore `seen_at_height`; status `product_version`. Residual catch-up after restart without full-history plaster. |
| 2026-08-11 | Account seq/balance in status + `AccountQuery`; genesis `faucet`; ctl `account`/`faucet`/`pay --sequence 0`; wallet stage script; epoch payday harness (`pl-epoch-payday.py`); `--epoch-ms`; undistributed emission refunds to treasury. |
| 2026-08-11 | Watcher soak tightened: `presence_slots` / `--full-presence` bulk mark (no 7d wait); status `watcher_weights_preview`; payday asserts full-presence pay + dust=0 (alice **294k** cap, bob 0). |
| 2026-08-11 | **Sprint A:** AMM+lend LP claims (credit-asset, payday LP path); bond/unbond cooldown + complete; dual-host soak script; ledger catch-up buffer + rebroadcast window. Note: local progress-timeout can diverge lottery under lag — dual soak defaults timeout=0 until shared-skip harden. |
| 2026-08-11 | **Shared-skip permanent fix:** `apply_shared_skip` sole lottery-mutation path; progress-timeout only if winner **pong stale** (not lag); post-skip grace; proposal resets timeout clock. Dual soak re-enabled `progress_timeout=1000`. |
| 2026-08-11 | **Private gate A–G:** network_id **2200**; emission matrix doc+test; BTC withdraw in smoke; adversarial + snapshot-restart scripts; Falcon-512 dress; `pl-gate-private-v22.sh` green (incl dual + f512). |
| 2026-08-11 | **Tip lag production fix:** pull catch-up (`NeedLedgers` on peer Pong tip); catch-up ingest without lottery re-check; fan state ops (bond/vault/defi) once to mesh so state roots match. Dual-host: **3 nodes tip 103 same hash** after load. |

---

*Falcon PL (FPL) — private main-style expansion, then public testnet. Falcon Ledger v1 testnet retires at v2.3.0 cutover.*
