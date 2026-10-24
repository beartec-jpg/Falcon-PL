> **HISTORICAL.** This file is not the live Falcon PL **2300** dest-lock specification.
> Current: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md) and [README.md](README.md).
> Falcon Ledger **1001** lock-mint is shut down.

# Falcon PL public testnet — implementation plan

**Source:** `~/PUBLIC_TESTNET_CHECKLIST.md` + `~/REVIEW_v2.6.0.md` (2026-08-13)  
**Code:** `host1:~/falcon-dag/falcon-dag-rs` (`fd-pl` product **2.7.0**, workspace crate **2.7.0**)  
**Live now:** Phase 1 (A1–A10) landed in tree. Fresh-genesis 3-hub soak: `scripts/pl-start-2.7-soak.sh` (network_id=2200, admin-sock + locked public wire). Private Tailscale only.

This is **not** a mainnet plan. Cut target is **one product version (2.7.0)** on **new `network_id=2300`**. Never flip 2200 public.

---

## 1. Checklist review (verdict)

The list is **correct, complete for first public, and sequenced right**. Confirmed against the tree:

| Checklist | Review | Code (confirmed) | Verdict |
|-----------|--------|------------------|---------|
| A1 key overwrite | #1 | `falcon_pl_node.rs` SubmitTx: `insert_public` **before** `submit_tx` | Real P0 |
| A2 unsigned votes | #2 | `LedgerOk`/`SkipPacker`/`ViewChange`/`OnlineSetVote` = bare `voter: String`; `vote_ok_ex` accepts any non-empty id | Real P0 |
| A3/A4 side-channel + admin | #3 | `WireMsg` still applies Vault/Claim/Bond/CreditAsset/WatcherCredit/Tick/ForceSettle/Rail*/SnapshotLoad/OperatorHardReset/SealNow locally | Real P0 |
| A5 pong-capped QC | #6 | `soft_votes_needed` still `min(soft, live_pong)` | Real (latent fork) |
| A6 sealed_at_ms clock | #7 | `verify_ledger_state_root` / `commit_ledger_body` set `now_ms = sealed_at_ms` then settle/unbond | Real |
| A7 catchup Propose | #8 | `ProposeLedger.catchup` → `ingest_ledger_catchup` (no lottery, no soft QC) | Real |
| A8 hmac refuse | #12 | `--scheme hmac` still a first-class path | Required for public |
| A9 integer lottery | #11 | `f64` pick in `lottery_packer_with_skips` | Required (cross-arch) |
| A10 version handshake | #9 | Hello has no `product_version`; crate 2.3.6 vs product 2.6.0 | Required |
| B11–18 money | #4 #5 | `CreditAsset` mints native; `WatcherCredit` self-writes work; rails are fixture merkle | Real |
| C19–28 soak | live | tip-49 stall = hung `load-net` Recv-Q + empty mempool, **not** a hash fork; soak is minutes not hours; droplet not in current 3-hub | Incomplete |
| D29–38 ceremony | docs | EXPANSION_PLAN still says “v2.3.0 public”; genesis buckets 196B/2B/1B/1B already locked | Correct freeze |

**Do not skip A.** Untrusted RPC or a fourth joiner can forge QC, steal keys, mint FPL, or load a snapshot.

**Do not skip C.** A public mesh that has only passed a 90s agree will freeze on day one.

**Do not skip D.** Mixing 2200 history, FL 1001 balances, or mixed crate/product versions is a fork class.

**Suggested sequence in the checklist is right**, with one refinement:

- Finish **C19–C27** on current 2.6 **except** C28 (unsigned-vote reject needs A2).
- Land **A** as **2.7.0-dev** on private 2200; then **B**; **re-run C** (join/exit, payday, adversarial C28).
- **D** only as a **new genesis 2300**.

**Out of scope (section E):** real ETH/BNB/XRP/USDC rails, governance, DHT, `lib.rs` split, mainnet branding. Do not let those leak into this cut.

---

## 2. What is already done (do not redo)

v2.6.0 private engine (keep):

- Falcon-512 tx + packer seal; tip-deterministic lottery; skip = cursor not reseed.
- Tip only after soft QC; empty ledgers rejected; equivocation jail.
- Residual catch-up `from=tip+1`; no auto genesis wipe.
- Poison-only future-seq drop; sequential pipeline kept.
- Slim Hello (validators only, ~7KB not 1.9MB); hub no-fan SubmitTx; catch-up reply priority.
- Soft floor ≥2 when `n≥2` (anti solo-commit).
- Epoch settlement from sealed chain (dual-host claimable divergence class).
- `ctl pay` / `faucet` already go through `SubmitTx` (good pattern for B13).

---

## 3. Implementation phases

### Phase 0 — Finish private soak C (this week, product stays 2.6.0)

**Goal:** written evidence that 2200 3-hub is live for **hours**, not minutes. No protocol change unless a real freeze (hash diverge or open-height stuck with work).

| # | Work | How | Done when |
|---|------|-----|-----------|
| C19 | tip=49 / empty-mempool dig | Write `docs/DIG_20260813_tip49.md`: hung `load-net` Recv-Q + drained inject; restart advanced 49→100+ same hash. Not a consensus freeze. | Dig in tree, linked from AMD |
| C20 | Multi-host soak | host1 v1/v2 + host2 v3 + **droplet** as 4th observer or hub. 25 tps, **≥4–8h**. Monitor tip+hash every 30s. | Log: no sustained fork; same hash on all answering hubs |
| C21 | Join/exit under load | Reuse `retest-3hub-join-exit.sh` against **WAN mesh** (`--from alice` bond). Joiner catch-up from snapshot tail, not genesis wipe. | Report PASS: tip moves; joiner tip=hubs; unbond+exit |
| C22 | Snapshot restart | `kill` v2; start with `--snapshot`; catch-up residual; no `NeedLedgers from=1` storm. | Same tip/hash; `need_hard_resync=false` |
| C23 | Skip QC | Stop lottery winner process; next preselected seat packs; all hubs same winner after skip QC. | Unit already exists; **WAN proof** required |
| C24 | View-change | Inject poison + stall (or stall pending); soft-QC VC; no operator hard-reset. | Unit exists; WAN proof after A2 if votes unsigned is ok for private |
| C25 | 20–80 tps keep-up | Steady 20 / 50 / 80 for 30+ min each on ≥3 hubs. | Table: mean tip Δt, mp p50/p95, agree% |
| C26 | Spike 150–500 | Short burst; `max_chain_ledgers=20000`; snapshots on. OOM = fail; tip pause = ok. | No process death; hash still agrees after drain |
| C27 | Epoch payday | `--epoch-ms` short (e.g. 60s) on 3 hosts; compare `claimable` + `commitment_root` after settle. | Identical roots |
| C28 | Defer | After Phase 1 (signed votes). Existing `equivocation_jails` stays. | — |

**Ops (not protocol):** `load-net` Recv-Q hang past `--seconds` — treat as inject bug; kill+restart is acceptable until a dedicated drain-timeout fix. Do not “fix” it by deferring SubmitTx on cold tip=0 (that froze the whole mesh).

---

### Phase 1 — A Safety (product **2.7.0-dev**, still network 2200)

Land as a **stacked PR series** on private. Each PR must keep existing lib tests green and add targeted tests. After this, **untrusted joiners are allowed**.

#### PR A1 — Account key registry (checklist 1)

**Files:** `crypto.rs` (`insert_public`), `falcon_pl_node.rs` SubmitTx/Hello, `lib.rs` `submit_tx`.

- Never `insert_public` from wire **before** verify.
- If KeyRing already has `account` → verify against **existing** pk; mismatch = reject.
- If missing → reject on public path. First-seen only via genesis keys or a **Bond** ledger (validator) / faucet genesis.
- Hello: import validator pks only if unknown; never overwrite bonded `ValidatorInfo.public_key`.
- Test: attacker SubmitTx with alice id + attacker Falcon key is rejected; alice can still pay.

#### PR A2 — Signed consensus votes (checklist 2)

**Files:** `WireMsg` + `vote_ok_ex` / `vote_skip_ex` / `vote_view_change` / `vote_online_set` + node fan path.

Domain-separated payloads (exact strings locked in AMD):

```
pl-ok:v1|{network_id}|{height}|{ledger_hash}|{voter}
pl-skip:v1|{network_id}|{height}|{packer}|{tip_hash}|{voter}
pl-vc:v1|{network_id}|{height}|{tip_hash}|{round}|{voter}
pl-online:v1|{network_id}|{epoch_id}|{hash}|{voter}
```

- Add `signature: String` to the four vote types (serde default `""` for one private transition; **reject empty** once all 2200 nodes upgraded).
- Verify Falcon-512 (or hmac only if `--allow-dev-crypto`).
- Reject: empty voter, unknown id, jailed, `bond==0`, voter≠key id, bad sig.
- Node loop signs with local `keys.sign(&id, payload)`.
- Tests: unsigned / wrong key / jailed voter do not count; 2 honest sigs still QC.

#### PR A3 — Public wire allowlist + operator socket (checklist 3–4)

**Files:** `falcon_pl_node.rs` `process_msg`, new `falcon_pl_admin` unix listener, `falcon_pl_ctl`.

**Public / mesh TCP** (listen port) may handle only:

`Hello, Pong, NeedLedgers, LedgersBatch, NeedTxs, TxsBatch, SubmitTx, ProposeLedger, LedgerOk, SkipPacker, ViewChange, OnlineSetVote, StatusReq, AccountQuery, Ok, Err`

**Drop / `Err("admin-only")`:** Vault*, ClaimRewards, CreatePool, AddLiquidity, Swap, CreateMarket, LendSupply, WatcherCredit, Bond, RequestUnbond, CompleteUnbond, CreditAsset, Tick, ForceSettleEpoch, Rail*, SnapshotLoad, OperatorHardReset, SealNow.

**Unix admin socket** (`--admin-sock /run/falcon-pl/admin.sock`, default off): SnapshotSave/Load, OperatorHardReset, Tick (private soak only). Mode 0600, local only.

**ctl:** `pay`/`faucet` already SubmitTx. Convert `bond`/`unbond`/`claim`/`vault-*`/`swap`/… to **signed `TxBody` + SubmitTx** (B13). `hard-reset` / `snapshot-load` → `--admin-sock`.

Private soak scripts (`pl-epoch-payday.py`, join/exit) must be updated in the same PR.

#### PR A4 — Tip-committed QC + clock + catchup (checklist 5–7)

- `soft_votes_needed` / `hard_votes_needed`: **only** bonded checkers and certified `|ONLINE|`. **Remove pong-fresh cap.** Pong stays `may_pack_locally` only.
- Tests: asymmetric pongs → **same** `soft_need` on same tip (replace “cap by live=2” expectation).
- `verify_ledger_state_root` / `commit_ledger_body`: **do not** set `now_ms = sealed_at_ms`. Optional: reject `sealed_at_ms` > local+skew (e.g. 30s). Never `maybe_settle_epochs` as a side effect of **verify**.
- `ProposeLedger.catchup=true`: ignore unless this connection is an **outbound pull** we opened, or `need_hard_resync`. Live gossip always `ingest_ledger` (stage + soft QC). `LedgersBatch` stays the catch-up apply path.

#### PR A5 — Product hygiene (checklist 8–10)

- Workspace `version = "2.7.0"`; `PRODUCT_VERSION = "2.7.0"`.
- Hello: `product_version`, `network_id`. Reject mismatch (log + drop).
- `--scheme hmac` requires `--allow-dev-crypto` or refuse to start.
- Integer lottery: `u128` pick `seed % total_bond` over sorted `(id, bond)`. Drop `f64`. Test: same winner as a pinned fixture on both schemes.
- Backfill AMD rows for 2.4 ONLINE, 2.5 view-change/QC-first, 2.6 liveness (already partial), 2.7 safety.

---

### Phase 2 — B Money (still 2200 private, then freeze params)

| # | Change | Notes |
|---|--------|--------|
| 11 | Remove or genesis-admin-only `CreditAsset` native mint | Secondary assets: soak-only behind `allow_credit_asset` **off** for public |
| 12 | Drop `TxBody::WatcherCredit` / WireMsg credit | Presence only from sealed pack/check work already on chain |
| 13 | All economy via sequenced txs | Done if A3 ctl conversion is complete |
| 14 | New keys | Ceremony (Phase 4); do not reuse `~/falcon-pl-private-testnet-v22/keys` |
| 15 | Public defaults | `unbond_cooldown_ms=14d`, `min_bond=1000`; private flags stay for soak |
| 16 | Freeze fee table | `params_version` bump; 50% burn; util tiers unchanged after cut without AMD |
| 17–18 | Rails | `synthetic_rails` default **false**; public mint/burn off. If ever on: `root == hdr.merkle_root` only; unique external txid; no withdraw replay |

---

### Phase 3 — Re-run C on 2.7 private

Repeat C20–C27 on the **hardened** binary (signed votes, no side-channel). **C28** now required: unsigned vote rejected; double-sign slash. Join/exit + payday must still pass (ctl now SubmitTx/admin-sock).

Do **not** start D until this re-soak is green.

---

### Phase 4 — D Ceremony (public 2300, product 2.7.0)

1. One version everywhere (crate, `PRODUCT_VERSION`, AMD, binaries, status).
2. Publish `docs/PUBLIC_PARAMS_2.7.md`: soft=3, hard=0.6, pack 1000ms, progress timeout, mempool 50_000, emission 30 bps, 7d epoch, bootstrap epochs 1–7, surviving AMDs.
3. **New genesis, `network_id=2300`.** Empty history. No 2200 snapshot, no FL 1001 balances.
4. Genesis buckets (locked): 196B keyless treasury, 2B community, 1B faucet, 1B builder. Record genesis hash + genesis validator publics.
5. Topology: ≥3 hubs (prefer 5 bonded genesis), ≥1 public RPC (firewalled, not naked `0.0.0.0`), ≥1 archival snapshot node.
6. Bootstrap peer list; status exposes tip, peers, epoch, faucet balance, `params_version`, `product_version`.
7. Wallet + faucet pointed **only** at 2300; Falcon-512; pay + claim minimum.
8. Docs: IDENTITY, checklist, param sheet, endpoints, “not mainnet / not a BFT proof.”
9. Retire Falcon Ledger v1 public testnet (announce, stop FL faucet/relays). **No auto-migrate.**
10. Announce window + go-live date. Safety patches immediate; Class B money-rule ≥1 epoch notice.

---

## 4. Suggested PR / week cadence

```
Week 0–1   Phase 0  C19 dig + 8h 3-hub soak + join/exit + snapshot + skip/VC WAN
Week 1–2   Phase 1  A1 → A2 → A3 → A4 → A5   (stack; private 2200 rolling restart)
Week 2     Phase 2  B11–B13, B16–B18 flags
Week 2–3   Phase 3  Re-soak C20–C28 on 2.7
Week 3–4   Phase 4  2300 genesis + RPC/faucet/wallet + FL retire + announce
```

Private 2200 fleet stays the soak lab forever. Public is a **new** cut.

---

## 5. Risks / non-goals

- **A5 (drop pong cap):** if one of 3 hubs is truly down, soft=3 stalls. That is **correct** vs forking. Mitigate with 5 genesis validators so 3-of-5 still live.
- **A3 ctl break:** all soak scripts must move in the same PR or private ops die.
- **Vote sig size:** Falcon-512 votes are large; keep epidemic re-fan rate-limits (already on skip/VC).
- **Do not** split `lib.rs` before Phase 3 (review #10).
- **Do not** ship rails as SPV.
- **Do not** reuse private keys or 2200 snapshots.

---

## 6. Immediate next action (after plan approval)

1. Write C19 dig (`DIG_20260813_tip49.md`) from this session’s evidence.
2. Start Phase 0 8h soak (host1+host2+droplet) with the current 2.6 binary + running 25 tps load.
3. Then implement **PR A1** (key overwrite) — smallest P0, no wire format change, unblocks treating extra joiners as hostile.
