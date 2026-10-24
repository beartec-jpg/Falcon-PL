> **HISTORICAL.** This file is not the live Falcon PL **2300** dest-lock specification.
> Current: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md) and [README.md](README.md).
> Falcon Ledger **1001** lock-mint is shut down.
> "Custodialess" below is the **target design**. The live 2300 BTC bridge is custodial and paused ([BTC_POOL_FREEZE_2026-10-16.md](BTC_POOL_FREEZE_2026-10-16.md)).

# Falcon PL (FPL) — Features & Tokenomics

**Product:** Falcon PL · **Ticker:** FPL · **Consensus:** Falcon Consensus · **Sigs:** Falcon-512  

This document fleshes out the **product surface** for Falcon PL: account types, transactions, economics, DeFi, and **custodialess bridges**. It draws from:

1. **Falcon Ledger (qXRP / FalconLedger)** — supply, keyless treasury, CID-style emission, PoPL spirit, live **BTC SPV** / stable bridge learnings.  
2. **Archived semi-prototype** in this monorepo (`archive/falcon-dag-research/`) — protocol **Vault** (destination-locked), Pool, LendingMarket, SPV rail txs.  
3. **Falcon Consensus** (this crate) — lottery packers + checkers (OK voters) as the participation surface.

**Status:** design aligned to **[EXPANSION_PLAN_v2.2.0.md](EXPANSION_PLAN_v2.2.0.md)** (locked private fleet → public testnet). PoC consensus exists; expansion surface is the v2.2.0 build target.

---

## 1. Design principles

| Principle | Meaning for FPL |
|-----------|-----------------|
| Participation | Bond → enter **packer lottery** and **checker** set |
| No empty farming | **No empty seals**; **no rewards when there is no economic activity** (no fees / no claimable pool) |
| Periodic pay | Rewards **epoch-settled**, not “only if tip advanced this second” — but pool size comes from **fees + emission rules**, not tip spam |
| Even pack/check work | Same bonded **validators** pack *and* check; **within** the validator pot, pack-work vs check-work weights are **even** (not two different populations) |
| LP inclusion | AMM / lending LPs: **0.5%** cap per provider; unfilled remainder → **validator pot** |
| Bridge watchers | Small **watcher pot**; **wallet-tied** verifiable work only; **tight per-account caps** (≪ LP caps) so early browsers cannot farm millions |
| Custodialess rails | Bridges are **protocol SPV / light-client + lock commitments** — no “bridge multisig owns user funds” as the end state |
| Hardcoded rails | Supported chains/assets are **genesis- or governance-listed**; not arbitrary user-added bridges day one |

---

## 2. What is live vs planned

| Layer | PoC today (`fd-pl`) | Target (this doc) |
|-------|---------------------|-------------------|
| Falcon Consensus | Lottery pack, soft/hard OK, skip, slash | Same + scored checkers, epoch claims |
| Payments | Simple account/seq transfers | + multi-asset, account types |
| Vault / Pool / Lending | — | Protocol-enforced account classes |
| AMM / lending | — | CreatePool, LP, swap, market ops |
| Bridges | — | Hardcoded rails; BTC SPV first (port Falcon Ledger) |
| Tokenomics | Fee → packer (proto) | Treasury, CID, pack/check/LP split, burn |

---

## 3. Token & supply (from Falcon Ledger, retargeted to FPL)

Keep the **honest bootstrap / keyless bulk** model that Falcon Ledger already socialized; rename unit to **FPL** (or keep a dual-display symbol if needed).

### 3.1 Fixed supply

| | |
|--|--|
| **Hard cap** | **200,000,000,000** FPL (200B) |
| **Protocol treasury** | **98%** (196B) — **no private key** |
| **Genesis circulating** | **2%** (4B) |

### 3.2 Genesis 2% split (public, not hidden premine)

| Bucket | Amount | Role |
|--------|--------|------|
| Community / airdrop | 2B (1%) | Distribution / contributors (split below) |
| Faucet | 1B (0.5%) | Onboarding. Mainnet faucet claims come only from this bucket |
| Builder pot | 1B (0.5%) | Pay for code, audits, growth (founder-operated, disclosed). Also funds the First-35 prize seats |

#### 3.2.1 Community bucket and related allocations (proposed)

> **Status: proposed allocation model (2026-10-14). Nothing is promised; every figure below is subject to change** until the mainnet parameter sheet is published. See [`docs/MAINNET_TRANSITION.md`](../../../../docs/MAINNET_TRANSITION.md).

| Allocation | Amount | Source bucket | How |
|---|---|---|---|
| Pre-mainnet airdrop | **1B** | Community (2B) | From the frozen testnet activity snapshot; credited at mainnet genesis to the snapshot account (name + bound key) |
| Release-phase giveaway | **1B** | Community (2B) | Paid per epoch during mainnet epochs 1–7 (no protocol emission then) for arcade, activity and similar; per-epoch and per-account caps |
| First-35 prize seats | **12M** (5 × 1M, 10 × 500k, 20 × 100k) | Builder pot (1B) | Separate from the airdrop; conditional on staying active and genuinely testing until mainnet |
| Operators pool | (sized in the parameter sheet) | Community airdrop (1B) | Separate pool for validator operators, ranked on the operators board; not mixed with user scores |
| Mainnet faucet | (drips) | Faucet (1B) | Unchanged; not part of the airdrop |

The protocol treasury (196B) is not used for any of these. Emission rules are unchanged: epochs 1–7 emit 0, emission starts at epoch 8.

Treasury funds leave **only** via protocol **epoch emission** rules — no human treasury withdrawal.

### 3.3 Emission (CID-style)

- **Continuous Inflationary Decline** over remaining treasury (integer bps per epoch).  
- **Bootstrap quiet:** first claimable emission at epoch **N** (Falcon Ledger used epoch 8; FPL can match or retune).  
- Epoch length: wall-clock or **ledger-count** based — prefer **time-based epochs** so “regardless of tip progression” holds: *slow tip does not stall pay calendar; **empty fee period still means empty fee share***.

### 3.4 “No txs → nobody gets paid” (precise rule)

| Source | If no txs this epoch |
|--------|----------------------|
| **Fee pool** | **Zero** — no fees collected ⇒ no fee distribution |
| **Emission pool** | Optional policy (recommended):  
  - **A (strict):** emission for the epoch is **0** if fee volume = 0 and no sealed non-empty ledgers (no activity).  
  - **B (soft):** emission still accrues to treasury schedule but is **unclaimable** unless packers/checkers/LPs posted **qualifying work** (seals, OK votes, LP share > 0).  

**PoC product choice: policy B** — treasury math can continue; **claims require participation**; pure idle bond earns **nothing**. Packers cannot farm empty ledgers (already banned). Checkers cannot farm OK votes on empty heights.

---

## 4. Reward pools (core economics)

Each epoch, form a **distributable pool**:

```text
epoch_pool = fee_reward_share(epoch) + emission_to_participants(epoch)
```

### 4.1 Fee split (per tx)

| Slice | Range (tunable) | Destination |
|-------|-----------------|-------------|
| **Burn** | 40–70% | Permanent destroy (deflationary) |
| **Consensus fee share** | remainder of fee after burn | → **Packer pool + Checker pool** (even split) |

Optional later: small **LP fee cut** from swap/borrow fees (DeFi-specific), separate from base payment fees.

### 4.2 Emission split (per epoch) — **55 / 5 / 20 / 20 + 1% LP caps**

**Important:** packers and checkers are the **same bonded nodes** (validators).  
There is one **validator pot**, not two different operator classes.  
“50/50 pack vs check” only splits **how work is scored inside that pot**.

**Watchers** (browser or VPS relays) are a **separate, smaller pot**: not validators unless they also bond.

#### Top-level epoch emission (product default)

| Bucket | Share of `epochEmit` | Who |
|--------|----------------------|-----|
| **Validators** | **55%** base (+ remainders from LP/watcher buckets) | Bonded packer/checker set |
| **Bridge watchers** | **5%** | FPL accounts with **countable watcher work** this epoch |
| **AMM LPs** | **20%** | Active AMM liquidity providers |
| **Lending LPs** | **20%** | Active lending suppliers |

```text
epochEmit
  ├─ 55% → validator_pot_base
  ├─  5% → watcher_pot       (work-weighted; remainder → validators)
  ├─ 20% → amm_lp_bucket     (1% cap / provider; remainder → validators)
  └─ 20% → lend_lp_bucket    (1% cap / provider; remainder → validators)

validator_pot = validator_pot_base
              + unfilled_amm_remainder
              + unfilled_lend_remainder
              + unfilled_watcher_remainder
              + fee_reward_share   # fees still go to validators (see §4.1)
```

#### Per-provider caps (LP vs watcher — different)

Expect **many watchers doing little work** early; **few LPs with real capital**. Caps must not let a single browser session look like a “million-token job.”

| Bucket | Share of `epochEmit` | **Max per distinct account** | Notes |
|--------|----------------------|------------------------------|--------|
| **AMM LPs** | 20% | **0.5%** of `epochEmit` (tunable; was 1%) | Still needs many LPs to empty the 20% |
| **Lending LPs** | 20% | **0.5%** of `epochEmit` | Same |
| **Watchers** | 5% | **0.05%** of `epochEmit` (default; max **0.1%**) | Tight on purpose — high headcount, low work each |

```text
# Example first claimable epoch: epochEmit = 980M (50 bps × 196B treasury)
LP cap 0.5%     → 4.9M FPL max per LP account / epoch
Watcher cap 0.05% → 490k FPL max per watcher account / epoch
Watcher cap 0.1%  → 980k FPL max (ceiling; prefer 0.05%)
```

| Rule | Spec |
|------|------|
| Within bucket | Pro-rata by **work** (LP share / watcher_work), **then** apply hard cap |
| Cap binds early | Few providers ⇒ most of 20%/5% **remainder → validator pot** |
| Empty bucket | Full slice → **validator pot** |
| Full drain AMM/lend 20% | At **0.5%** cap: need **≥ 40** distinct providers at the cap (40 × 0.5% = 20%) |
| Full drain watcher 5% | At **0.05%** cap: need **≥ 100** distinct watchers at the cap (100 × 0.05% = 5%) |

**Watchers never earn for “opened the browser.”** Cap is only an upper bound after **on-chain countable work** (§4.4b). Zero accepted headers/deposits ⇒ **zero** claim.

#### Inside the validator pot: even pack-work vs check-work

Same people; two **work meters** for payday:

| Half of `validator_pot` | Weight |
|-------------------------|--------|
| **Pack half (50%)** | Epoch counters: mainly **`txs_packed`** (perf); see §4.5 |
| **Check half (50%)** | Epoch **qualifying OK votes** on accepted ledgers (1 vote / ledger / node) |

```text
validator_pot
  ├─ 50% pack_half   → split by txs_i (performance)
  └─ 50% check_half  → split by ok_votes_i
```

- Lottery win alone does **not** pay.  
- A node that only votes earns from **check half**; one that packs fat ledgers earns more from **pack half**.  
- Most healthy validators earn from **both**.

#### Slight edge for full-history runners (optional, small)

Full history is **ops service**, not a second consensus class. Optional **mild** boost so archives are not pure charity:

| Option | Spec |
|--------|------|
| **A (recommended)** | **+5–10% weight multiplier** on that node’s pack+check share if it attested **full-history service** this epoch (on-chain flag + peer/challenger checks later) |
| **B** | Separate **≤5%** of `validator_pot` “history stipend” split among full-history bonded nodes that answered history challenges |
| **Not** | Paying unbound RPC-only boxes from emission |

Default product stance: **Option A at +5%** after a simple self-declared + slashable “I serve history” bond flag; tune later.

### 4.3 Fees

Fee reward share (after burn) goes **100% into `validator_pot`** (then pack/check 50/50 work split).  
LP fee cuts from swap/borrow can be added later **on top of** DeFi fees, separate from base payment fees.

**Fee escalation (v2.2, AMD-20260811-1):** base `min_fee` multiplies by mempool util tier (1/2/4/8/16×). Submits below the suggested min are rejected; burn share stays **fixed 50%** (dynamic burn is a post-v2.3 non-goal). Metrics expose accept/reject counters, `fee_p50`/`fee_p90`, and `fee_market` in status. See [AMENDMENTS.md](AMENDMENTS.md) for how tier schedules may change.

### 4.4 Periodic payout (independent of tip rate)

- **Epoch timer** opens **ClaimValidatorReward** (or ClaimPack / ClaimCheck line items) + **ClaimWatcherReward** + **ClaimAmmLpReward** / **ClaimLendLpReward**.  
- Claims **pull-based**, capped by remaining epoch allocation.  
- Tip may be fast or slow; **epoch still settles**.  
- No fees and no qualifying work → **zero** claims.

### 4.4b Bridge watcher pot (wallet-tied: **time × work** within the epoch)

**Goal:** Wallet-linked browser/VPS watchers earn from a **small watcher pot** on payday.  
Reward scales with **how long they were actually active in the epoch** and **how much verifiable work they did in that time**.  
One minute of activity → **dust**. Full ~**7-day** epoch of real activity → entitled to their **full pro-rata share** (still capped).  
**Opening a browser with no work still pays zero.**

#### Model: presence × work (not pure time, not work-only)

| Factor | Meaning |
|--------|---------|
| **`work_i`** | On-chain countable bridge work this epoch (headers, deposits, …) |
| **`presence_i`** | Fraction of the epoch they were **credibly active** ∈ `[0, 1]` |
| **`weight_i`** | `work_i × presence_i` (default) |
| **Cap** | Then `min(cap_w, pot × weight_i / Σ weight)` |

```text
# Epoch length e.g. 7 days = T seconds (or N slots)

presence_i = active_time_i / T     # clamp to [0, 1]
             # prefer slot method below for on-chain honesty

weight_i   = work_i * presence_i   # both required

# 1 minute of a 7-day epoch:
#   presence ≈ 1 / (7*24*60) ≈ 0.000099 → dust even if work_i > 0

# Full epoch always-on with steady work:
#   presence = 1 → weight_i = work_i → full fair share of pot (≤ cap)
```

**Intuition:** time **scales** the value of work. Same work crammed into one minute earns ~dust vs the same work spread / sustained over days of activity. Full-epoch runners are not diluted by pure lurkers; idle tabs without work still get **nothing** (`work_i = 0` ⇒ `weight_i = 0`).

Optional variant (still fine):  
`weight_i = work_i * (presence_i ^ α)` with `α ∈ (0.5, 1]` — slightly softer on short sessions; **default α = 1**.

#### How “active time” is measured (wallet / on-chain)

Do **not** trust “client said I was online 7 days.” Prefer **activity slots**:

| Mechanism | Spec |
|-----------|------|
| **Epoch slots** | Divide epoch into slots (e.g. **hourly** → ~168 slots / 7 days; or **15 min** → finer) |
| **Slot counts** | Slot is active for account `i` if ≥1 **verifiable** event in that slot: countable work **or** cheap **WatcherHeartbeat** (tiny fee) while wallet watcher is open |
| **`active_time_i`** | `slots_active_i / slots_total` → `presence_i` |
| **Heartbeat** | Optional; only fills presence; **does not** increase `work_i`. Fee stops pure free spam of heartbeats. **v2.9.29:** signed `TxBody::WatcherHeartbeat` (current slot only). `WatcherCredit` remains soak/admin and is **not** the browser path |
| **Bookend cheat** | Two heartbeats at start/end of epoch only fill **2 slots** → presence ≈ 2/168 ≈ **1.2%**, not 100% |

So a user who **actually ran the watcher for ~full 7 days** (regular heartbeats and/or steady work) can reach **`presence ≈ 1`** and get **full weight from their work**. A one-minute open gets **≤ 1 slot** → dust.

#### What counts as `work_i` (chain-verifiable)

| Countable event | Weight (example) | Notes |
|-----------------|------------------|--------|
| Accepted **SubmitRailHeader** that advances a rail tip | medium | Dupes of same height ≈ 0 |
| Accepted **RailDeposit** this account submitted | **high** | Real deposit relayed |
| Challenge / fraud assist (later) | high | Dispute work |
| Redundant duplicate | **0** | Anti-spam |
| Heartbeat only | **0 work** | Only helps **presence** |

#### How it attaches to an FPL account

1. Watcher (browser PWA / extension / VPS) bound to **FPL wallet**.  
2. While active: heartbeats (optional) + real rail txs.  
3. Chain updates `work_i` and slot bitmap / counters.  
4. Payday: `ClaimWatcherReward` using `weight_i = work_i × presence_i`, then **cap**.

Browser and VPS use the **same** pot and formula.

#### Payday math

```text
watcher_pot = 5% of epochEmit
cap_w       = 0.05% of epochEmit    # default; governance ≤ 0.1%

presence_i  = slots_active_i / slots_total      # [0, 1]
weight_i    = work_i * presence_i               # 0 if no work

raw_i       = watcher_pot * weight_i / sum(weight_j)
reward_i    = min(cap_w, raw_i)

# unused pot (no workers / after caps) → validator_pot
```

| Situation | Outcome |
|-----------|---------|
| Opened browser 1 minute, one lucky work unit | **Dust** (tiny presence) |
| Online full epoch, **no** accepted work | **0** (`work_i = 0`) |
| Online full epoch, real work all week | **Full pro-rata** of pot from weight, **≤ cap** |
| Idle tab, heartbeats only | Presence can rise, **reward still 0** without work |
| Same key, many tabs | One account, one cap, one weight |

#### Feasibility (browser)

| Piece | Feasible? |
|-------|-----------|
| Wallet-tied session + sign heartbeats/work | **Yes** |
| Slot presence + work on-chain | **Yes** |
| Trust-free “I was online 7 days” without txs | **No** — slots fix this |
| Always-on reliability | VPS still best; browser earns when actually running |

**Product line:**  
*Run the FPL watcher from your wallet. Stay active over the epoch and do real bridge work — full-week runners get full proportional credit; a one-minute peek is dust; nothing for idle tabs. Payday share of the watcher pot, hard-capped.*

### 4.5 Performance bonus for lottery winners (big machine → more txs → more pay)

**Goal:** Faster packers that fit **more applicable txs** into their seal(s) earn a **bigger packer reward**, without turning wins into a spam contest.

#### Do **not** pay the performance bonus “on every win” (instant)

| Instant per-win bonus | Problem |
|----------------------|---------|
| Immediate payout each seal | Incentives to **spam height transitions** / micro-ledgers if any path pays “per win” |
| Couples money to **tip speed** | Self-dealing load, fee wars, or empty-adjacent farming pressure |
| Accounting noise | Every seal mutates balances mid-epoch; harder to audit |

Empty seals are already banned — but **tiny 1-tx seals** can still spam **height rate** if you reward “wins” more than **work**.

#### Do: **epoch work counters** + payday (recommended)

For each bonded packer `i` over the epoch, track only **accepted, non-empty** seals:

| Counter | Definition |
|---------|------------|
| `seals_i` | Number of ledgers sealed by `i` that made tip |
| `txs_i` | Sum of **tx counts** in those ledgers |
| `fees_i` (optional) | Sum of fees in those ledgers (aligns with real economic load) |

On **epoch settle / claim day** (inside **pack half** of `validator_pot` only):

```text
pack_half = 50% of validator_pot

# weights for bonded node i:
perf_weight_i = txs_i          # default: pure tx-count performance
# optional: α*seals_i + β*txs_i with α small

pack_reward_i = pack_half * perf_weight_i / sum(perf_weight_j)
# then optional full-history multiplier on (pack_reward_i + check_reward_i)
```

So:

1. **Lottery** still picks who may pack (equal bond weight day one).  
2. **Winning** alone does not pay.  
3. **Packing more txs** when you win raises `txs_i` → larger share of **pack half** on payday.  
4. Check half is separate (OK votes). Same node can earn both.  
5. All of this is **epoch claim**, not instant per-win mint.

#### Why this is not “spamming our own network”

| Attack | Mitigation |
|--------|------------|
| Self-spam txs to inflate `txs_i` | Attacker pays **fees** (burn + pools); net often negative; rate limits / mempool cap |
| Many micro-seals | **α≈0, β on txs** ⇒ one fat seal beats ten 1-tx seals for the same work; optional **min txs per seal** for perf weight |
| Win more lotteries by bonding | Lottery can stay **bond-equal**; bonus is **work**, not bond size |
| Stuff invalid txs | Only **accepted** ledger txs count |

#### Lottery vs reward (keep separate)

| Layer | What it does |
|-------|----------------|
| **Lottery** | Who *may* pack this height (fair access) |
| **Epoch counters** | `txs_i` (pack), `ok_votes_i` (check) on same bonded set |
| **Payday** | 55/5/20/20 emission (val / watcher / AMM / lend); LP+watcher caps; remainder + fees → validator pot; pot split 50/50 pack/check work |

**Optional v2:** a small **lottery weight bump** from trailing epoch `txs_i` — only after anti-spam is solid.

#### Bottom line

| Option | Verdict |
|--------|---------|
| Bonus paid **every win** (instant) | **No** — spam / tip-thrash risk |
| **Tx counter** per bonded node → share of **pack half** on payday | **Yes** — big machines earn more by packing more real txs |
| Packers ≠ checkers as separate people | **No** — same validators; two work meters |

---

## 5. Bonding & participation (lottery story)

| Action | Effect |
|--------|--------|
| **Bond** | Lock FPL; join **lottery set** + **checker set** |
| **Unbond** | Cooldown; leave lottery after lock |
| **Slash** | Equivocation (double-seal) burns bond; optional other offenses later |
| **Epoch counters** | `seals_i`, `txs_i` (packers); `ok_votes_i` (checkers) — drive payday shares |
| **Score (optional v2)** | Trailing performance may soft-weight lottery later; payday already work-weighted |

Day-one: **equal bond weight** for lottery eligibility; **tx-count (perf) weight** inside packer pool on epoch claim.

---

## 6. Account types (protocol-enforced)

Ported from the semi-prototype (`AccountType` in archived `fd-types`), adapted to Falcon Consensus state machine.

| Type | Rule |
|------|------|
| **Hot** | Normal spendable account |
| **Timed** | Allowlist + optional velocity limits |
| **Vault** | Cold / treasury-style: destinations set then **hard lock**; outflows **only** to allowlisted destinations |
| **Pool** | AMM vault: outflows only via protocol AMM ops (swap out / LP withdraw) |
| **LendingMarket** | Lending vault: outflows only via supply/withdraw/borrow/repay/liquidate |

### 6.1 Secure vault (single / few send addresses)

Falcon Ledger / prototype intent: **protocol-enforced cold path**.

| Rule | Spec |
|------|------|
| Max destinations | **1–5** (product default: allow **1** for “single sender/receiver lock”, up to 5 for multi-sig style cold) |
| `SetAllowlist` | Only while vault **unlocked** |
| `VaultLock` | **Irreversible** hard lock |
| After lock | Native/asset **payments only to allowlisted destinations**; any other destination **rejected by protocol** |
| No key bypass | Hot key of the vault account cannot free-spend to arbitrary addresses |

This is the “**secure vault with only one sending address, protocol enforced**” feature — implement as **Vault + allowlist length 1 + VaultLock**.

---

## 7. Transaction types (target catalog)

### 7.1 Core

| Kind | Purpose |
|------|---------|
| Payment / Transfer | Move native or assets |
| Issue / Mint / Burn | Asset lifecycle (issuer rules) |
| OpenAccountType | Hot → Vault / Timed / Pool / LendingMarket |
| SetAllowlist / VaultLock / SetLimits | Vault & Timed guards |
| Bond / Unbond / Claim* | Participation & rewards |

### 7.2 AMM

| Kind | Purpose |
|------|---------|
| CreatePool | Register pool on Pool account |
| AddLiquidity / RemoveLiquidity | LP mint/burn |
| Swap | Constant-product (or chosen curve) with min_out |

### 7.3 Lending

| Kind | Purpose |
|------|---------|
| CreateMarket | Lending market for asset |
| Supply / WithdrawSupply | LP supply side |
| Borrow / Repay / Liquidate | Debt side |
| OracleVote | Median price votes for LTV (or hardened oracle later) |

### 7.4 Bridges (custodialess rails)

| Kind | Purpose |
|------|---------|
| SubmitRailHeader | Advance light-client tip for a **hardcoded rail** |
| RailDeposit | Mint wrapped asset after **SPV/inclusion proof** vs tip |
| RailWithdraw | Burn wrapped; open external unlock note |

**No** “federation mint with trusted operator key” as the security model.

---

## 8. Bridges — hardcoded, fully custodialess

### 8.1 Goal

**Fully custodialess pools and bridges:** user locks on source chain under a **known script/contract**; FPL mints only after **verifiable proof**; withdraw burns on FPL and unlocks via **proof / user-claim path**, not a hot wallet “bridge company” holding funds long-term.

### 8.2 Hardcoded rails (genesis or freeze list)

Initial rail set (from prototype + Falcon Ledger direction):

| Rail | Family | Lock model | Notes |
|------|--------|------------|--------|
| **BTC** | Bitcoin SPV | Vault/script commitment + header chain + merkle proof | **Falcon Ledger live testnet SPV path is the reference implementation** |
| **ETH** | EVM light / deposit tree | Contract lock + header + deposit proof | Prefer multi-sig **only** as interim; target user-claim or threshold with public verify |
| **BNB** | EVM | Same family as ETH | Hardcoded chain id + contract |
| **XRP** | XRPL ledger proofs | Escrow/payment inclusion | Optional |

Adding a rail requires **protocol upgrade / governance** — not a random RPC flag.

### 8.3 BTC SPV (from Falcon Ledger → FPL)

Falcon Ledger finalized a **BTC SPV / light-client style** bridge prototype on live testnet. FPL ports the **security model**:

1. On-chain **header chain** (or checkpointed headers) for BTC.  
2. **Deposit:** user pays to protocol lock; submit **tx + merkle path + header** → `RailDeposit` mints **wBTC** (or rail asset) to Falcon account.  
3. **Withdraw:** burn wBTC; BTC unlock via **pre-committed script path** / watchtower-free user claim where possible.  
4. Bridge accounting account `bridge:BTC` is **protocol-sealed** (no human free mint).

### 8.4 What “fully custodialess” means here

| OK | Not OK (end state) |
|----|---------------------|
| User-controlled lock scripts / contracts | Single EOA bridge operator as sole custodian |
| On-chain verification of inclusion proofs | “Trust the relay’s signature” as sole mint authority |
| Relays as **unprivileged** messengers | Relay holds mainnet private keys that can steal locks |
| Hardcoded verified contracts/scripts | Arbitrary user-declared bridge destinations without verify |

Relays may still **submit** headers/proofs; they must not be **necessary trusted custodians**.

### 8.5 Pools + bridges

- AMM/lending may list **rail assets** (wBTC, wETH, …) only if rail is **active** and mint path is proof-gated.  
- LP rewards apply to those pools the same way as native pairs.  
- No protocol “bridge LP” that rehypothecates locked BTC outside the SPV rules.

---

## 9. Falcon Consensus integration (who is pack vs checker)

| Role | How you join | Work that pays |
|------|----------------|----------------|
| **Packer** | Bonded + lottery eligible | Seal **non-empty** ledgers when elected (or as standby after skip) |
| **Checker** | Same bonded set (or subset policy) | OK votes on accepted ledgers; optional soft/hard thresholds |
| **LP** | Provide AMM or lending liquidity | Time-weighted share in LP emission buckets |

Lottery election remains **deterministic and tip-based** (Falcon Consensus). Economics **do not** change the winner; they reward **work** after the fact each epoch.

---

## 10. Fee & safety knobs (defaults to refine)

| Knob | Suggested default | Notes |
|------|-------------------|--------|
| Soft OK | 3 | Pipeline |
| Hard OK ratio | 0.6 | Strong confirm |
| Pack budget | 1000 ms | Time-fill ledgers |
| Progress timeout | 1000 ms | Standby packer |
| Max mempool | 50_000 | Backpressure |
| Vault max destinations | 1 (strict) or 5 | Product choice; “single address vault” = 1 |
| Epoch length | 30–60 min testnet; longer mainnet | Time-based preferred |
| Burn bps | Dynamic 40–70% | Falcon Ledger style |

---

## 11. Implementation phases (suggested)

| Phase | Deliverable |
|-------|-------------|
| **P0** | History prune / snapshot (stop OOM); stable multi-host |
| **P1** | Bond/unbond + epoch skeleton + pack/check claim (even split) |
| **P2** | Account types: Vault (single-dest) + VaultLock enforced |
| **P3** | AMM create/add/remove/swap + AMM LP claims |
| **P4** | Lending market + LP claims + oracle policy |
| **P5** | BTC SPV rail (port Falcon Ledger testnet path) + `bridge:BTC` |
| **P6** | ETH/BNB rails; harden custodialess withdraw |
| **P7** | Governance for rail list / burn bps / epoch params |

---

## 12. One-page product story

**Falcon PL (FPL)** is a participation ledger under **Falcon Consensus**: bond in, enter the lottery, pack **and** check as the same validators, get paid from **fees + emission** on an **epoch schedule**. Emission defaults **55% validators / 5% bridge watchers / 20% AMM LPs / 20% lending LPs**, with **0.5% max per LP** and **0.05% max per watcher** (≤0.1%) of epochEmit; remainders → validators. Watchers are **wallet-tied** and paid only for **on-chain verifiable work** — not for opening a browser. Inside the validator pot, **pack/check 50/50**, **tx-count** on pack half at payday. **No empty seals, no idle free money.** **Vaults**, **AMM/lending**, and **custodialess SPV bridges** complete the product surface.

---

## 13. References (in-repo / sibling)

| Source | Use |
|--------|-----|
| [IDENTITY.md](IDENTITY.md) | Naming |
| [POC.md](POC.md) | Consensus PoC results |
| `archive/falcon-dag-research/crates/fd-types` | AccountType, TxKind, BridgeProof, rails |
| `archive/falcon-dag-research/crates/fd-exec/bridge.rs` | SPV substrate sketch |
| FalconLedger whitepaper + `docs/btc-spv` + `MAINNET_BRIDGE.md` | Supply/CID/PoPL + live bridge ops |

---

*This is the product blueprint for Falcon PL beyond the consensus PoC. Implementation tracks phases in §11.*
