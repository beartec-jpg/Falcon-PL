# Falcon PL — Amendments & Change Control

| | |
|--|--|
| **Product** | Falcon PL (FPL) |
| **Applies to** | Locked plan docs + on-chain-relevant parameters + protocol surface |
| **Status** | **LOCKED** versioning: **2.2.0** private testnet baseline → **2.2.x** private fixes/adds → **2.3.0** public testnet release |

This file answers: **how do numbers, rules, and surfaces change after the expansion plan is locked?**

Cross-refs: [EXPANSION_PLAN_v2.2.0.md](EXPANSION_PLAN_v2.2.0.md) §3–§5 (locked values), §9 (non-goals), §10–§11 (doc control + changelog).

---

## 1. Principle

| Rule | Meaning |
|------|---------|
| **No silent locks** | Anything in EXPANSION_PLAN §3–§5 may not change without a numbered amendment |
| **Code follows paper** | Implementation may ship ahead of soak, but **defaults** must match the lock or cite an AMD id |
| **Evidence for money rules** | Emission, fees, caps, burn — need soak/test note or explicit “private experiment” flag |
| **Version bump** | Private fixes/adds → **`2.2.1`, `2.2.2`, …** only. **`2.3.0` only** when opening **public** testnet |

---

## 2. Change classes

| Class | Examples | Who decides (v2.2 private) | Who decides (v2.3 public testnet) | Effective when |
|-------|----------|----------------------------|-----------------------------------|----------------|
| **A — Ops only** | `seal_interval_ms`, log level, host peers, snapshot path | Any fleet operator | Ops / release owners | Node restart / deploy |
| **B — Locked parameter** | `emission_bps`, fee burn 50%, soft=3, hard=0.6, LP/watcher caps, epoch_ms, min_bond, `min_fee` tiers | **Private Falcon operators** (see §3) | **Freeze sheet** at go-live; later AMD + announce | Next `params_version` + restart (or epoch boundary if hot-reload exists) |
| **C — Protocol surface** | New WireMsg, vault rules, rail add (ETH live), claim shapes | Operators + written design note | Same + public changelog | Versioned release |
| **D — Identity / product** | Rename ticker, consensus name | Explicit product decision | Explicit product decision | Doc + release |
| **E — Non-goal unlock** | Dynamic burn curve, perf-weighted lottery (§9) | Requires **new section** in expansion plan | Same | Major version intent |

**Class A never needs an AMD id.** Classes B–E do.

---

## 3. Decision authority (by phase)

### 3.1 v2.2.0 / v2.2.x — private fleet

| Role | Authority |
|------|-----------|
| **Propose** | Anyone building/running the fleet (engineer note, soak report, issue) |
| **Decide** | **Falcon private operators** — the people who control private keys + deploy hosts (not a public token vote) |
| **Quorum** | **Written multi-ack**: at least **two** operator acknowledgements recorded under the AMD entry (or one if solo-ops, noted as such) |
| **Not** | Bonded validators on the private chain are **not** a governance body yet (they are test subjects) |

### 3.2 v2.3.0 — first public testnet

| Role | Authority |
|------|-----------|
| **Launch freeze** | Operators publish a **v2.3.0 parameter sheet** (copy of locks + AMD deltas) |
| **Post-launch B/C** | Operators may still amend for safety; **must** announce on public channels + changelog with **≥1 epoch notice** when practical (safety patches may be immediate) |
| **Validator soft signal** | Optional later: bonded set can **signal** preference on-chain; **not binding** in v2.3.0 |
| **No** | On-chain plutocratic vote binding treasury or emission in v2.3 |

### 3.3 Mainnet (explicitly out of scope)

Not designed here. Public mainnet needs a separate governance ADR; do not invent it mid-testnet.

The **proposed** allocation and transition model (pre-mainnet airdrop, release-phase giveaway, First-35 seats, operators pool, genesis manifest) is written up in `docs/MAINNET_TRANSITION.md` as input to that ADR. It is a proposal: nothing promised, subject to change, and not a parameter lock.

---

## 4. Amendment record format

Every Class B–E change gets an id and a changelog row.

```text
AMD-YYYYMMDD-N
```

Example: `AMD-20260811-1`.

### Required fields

| Field | Content |
|-------|---------|
| **Id** | `AMD-…` |
| **Date** | ISO date |
| **Class** | B / C / D / E |
| **Title** | Short |
| **Old → New** | Concrete values or “n/a (new surface)” |
| **Rationale** | Why |
| **Evidence** | Test names, soak path, or “private experiment” |
| **Effective** | Version / epoch / restart |
| **Ack** | Operator initials or names |

### Where recorded

1. **This file** — §8 Amendment log (canonical short form)  
2. **EXPANSION_PLAN §11 Changelog** — one-line pointer to AMD id  
3. **Code** — comment or `params_version` bump when parameters change  
4. Optional: git commit message `AMD-…`

---

## 5. Process (happy path)

```text
1. Draft        → short note (old→new, class, why)
2. Implement    → code + tests (or doc-only if pure lock text)
3. Private gate → run relevant smoke/soak if money or consensus
4. Ack          → operators record ack on AMD entry
5. Merge        → changelog + this log + version bump if needed
6. Deploy       → private fleet restart; params_version visible in status
```

### Fast path (safety)

Security / OOM / consensus halt: ship fix first, **backfill AMD within 48h** with class B/C and evidence.

### Reject path

If soak falsifies a locked number (e.g. emission too hot): open AMD to **retune**, do not silently edit §4.

---

## 6. Fee market & metrics amendments

Fee **escalation schedule** (multipliers vs mempool util) is a **Class B** lock once published in EXPANSION_PLAN / this file.

| Item | v2.2.0 lock intent |
|------|-------------------|
| Base min fee | Configurable (`min_fee`); default **1** |
| Escalation | On by default; util tiers → fee multiplier (see metrics module / plan §4.5b) |
| Dynamic burn 40–70% | **Non-goal** until after v2.3 (§9) — requires Class E unlock |
| Replace-by-fee | Deferred; metrics may track pressure only |

Changing tier breakpoints or multipliers → **AMD + changelog**.

---

## 7. `params_version`

Nodes expose `params_version` in status (integer).

| When | Action |
|------|--------|
| Genesis / v2.2.0 start | `params_version = 1` |
| Each Class B parameter lock change that ships in code | Increment by 1 |
| Class C surface only | Increment if wire is incompatible; else note in AMD without bump if additive |

Clients and soak scripts should log `params_version` next to tip height.

---

## 8. Amendment log

| Id | Date | Class | Title | Old → New | Effective | Ack |
|----|------|-------|-------|-----------|-----------|-----|
| AMD-20260811-1 | 2026-08-11 | B | Fee escalation metrics + min-fee tiers | none → util-tier min fee + counters | v2.2.0 params_version=2 | ops |
| AMD-20260811-2 | 2026-08-11 | C | Amendment process locked | informal “changelog only” → this document | v2.2.0 docs | ops |
| AMD-20260812-1 | 2026-08-12 | C | Snapshot chain-tail persistence | load wiped `chain` → restore pruned tail + atomic write + `product_version` | **v2.2.1** snapshot schema 2 | ops |
| AMD-20260812-2 | 2026-08-12 | C | Memory / catch-up storm fix | tip-state as genesis + unbounded queues + info log spam → true genesis_accounts, bounded wire queues, rate-limited hard-resync, quiet serve logs | **v2.2.2** | ops |
| AMD-20260812-3 | 2026-08-12 | C | Hub mempool + NeedTxs | per-node gossip-only mempool → hub role, dual tx/consensus queues, NeedTxs/TxsBatch pull, safer progress timeout | **v2.2.3** | ops |
| AMD-20260812-4 | 2026-08-12 | C | Seq-poison pack freeze | no-applicable self-skip + “mempool non-empty ⇒ skip winner” → wait on seq holes; progress-timeout only if `has_applicable_tx`; genesis wipes load-seq | **v2.2.4** | ops |
| AMD-20260812-5 | 2026-08-12 | C | Join hard-resync tip-0 freeze | hard-resync re-inserted live bonders at genesis → h=1 state-root fail forever; track `genesis_validators` seed set; arm hard-resync on live forks; refuse pack during resync | **v2.2.5** | ops |
| AMD-20260812-6 | 2026-08-12 | C | Phantom tip_lag pack freeze | `best_peer_tip` high-water never fell after ghost tip ads → tip_lag>0 forever → no pack + skip thrash; recompute best from live pong tip ads | **v2.2.6** | ops |
| AMD-20260812-7 | 2026-08-12 | C | Future-seq mempool freeze | full mempool of sequence>need + no-applicable wait + no progress skip → tip freeze under load; drop future-seq poison on no-applicable pack | **v2.2.7** | ops |
| AMD-20260812-8 | 2026-08-12 | C | **P0 multi-host safety** | pong-seeded lottery + gossip skip + tip-on-self-seal + hard-resync-on-live-fork → tip-deterministic lottery, skip QC, soft-QC tip advance, fork-choice keep-first | **v2.3.0** | ops |
| AMD-20260812-9 | 2026-08-12 | C | **P1 multi-host liveness** | thin-mp seal without hub pull; `max_chain_ledgers=0` OOM; unbounded vote maps | pre-pack NeedTxs + grace; clamp chain>0; prune votes/pending | **v2.3.1** | ops |
| AMD-20260812-10 | 2026-08-12 | C | **P2 recovery** | auto hard-resync on catch-up fork → from=1 storms | residual QC catch-up; operator-only hard-reset (`ctl hard-reset`) | **v2.3.2** | ops |
| AMD-20260812-11 | 2026-08-12 | C | **Tip-only lottery** | skip QC in lottery seed → multi-val desync winners | lottery = tip+bonded set only; skip does not re-elect | **v2.3.3** | ops |
| AMD-20260812-12 | 2026-08-12 | C | **Seat-cursor lottery** | skip reseeded winners | tip-fixed order; skip advances preselected seat; backup depth 3 | **v2.3.4** | ops |
| AMD-20260813-2 | 2026-08-13 | C | **Open-height liveness / poison freeze** | non-applicable mempool + skip/view gates requiring has_applicable → permanent tip stall multi-host; **also** blind future-seq drop destroyed healthy pipelines (packer-only mempool wipe) | poison-only future drop; keep sequential pipeline; offline/lagging skip; view-change on poison/unavailable stall; all-lottery-seats-skipped reopen | **v2.6.0** | ops |
| AMD-20260813-3 | 2026-08-13 | C | **Public-safety surface (Phase 1 / A)** | unsigned votes, key overwrite, admin on mesh TCP, pong-capped QC, f64 lottery, mixed crate/product versions, hmac-dev always-on | registry-only Falcon keys; signed votes; unix `--admin-sock` + `--lock-public-wire`; hmac refuse without `--allow-dev-crypto`; integer lottery; Hello product+network; crate=`PRODUCT_VERSION`=**2.7.0** | **v2.7.0** | ops |
| AMD-20260813-4 | 2026-08-13 | C | **No live-tip rewind** | v2.3.9 lagging-fork auto-armed `NeedLedgers from=1` → `hard_resync` wiped tip 800→64 after a clock/state-root reject | Catch-up is residual-only. Genesis wipe only on **operator** hard-reset. Stale ONLINE epoch votes ignored. Honest packer cannot seal a second body at the same height. | **v2.7.1** | ops |
| AMD-20260813-5 | 2026-08-13 | C | **One apply path / ledger time** | `tick` settled epochs; verify/commit used local `now_ms` → catch-up failed after clock skew (v1/v2 stuck at h=1) | State root = f(parent, sealed_at, txs). `tick` is timeouts only. Genesis h=1 waits for live seats. | **v2.7.2** | ops |
| AMD-20260814-1 | 2026-08-14 | C | **One body per height + self-heal** | Skip-QC elected a second sealer while a proposal was in flight; soft=3 of 6 allowed two QCs (859 split, tip frozen at 871) | No pack/skip once any body is seen at the open height. Dual proposals pick the lower hash until unique QC. n≥6 soft = majority. View-change clears the height. | **v2.7.3** | ops |
| AMD-20260814-2 | 2026-08-14 | C | **Lottery committee QC** | Soft over all bonded → 51-of-100 at scale (slow tip); skip could draw a new packer set | Lottery freezes **1 packer + 5 checkers**. Only they soft-QC / skip / VC. Soft = majority of that 6 (4). Hard still `ceil(n×0.6)` over all bonded. Skip walks the same 6. | **v2.7.4** | ops |
| AMD-20260814-3 | 2026-08-14 | C | **Soft is 3 of 5 checkers** | 4-of-6 majority was slower than needed; two groups of 3 cannot form inside the 5 checkers | Soft = **3 of the 5 checkers**. Packer OK does not soft-pass. Hard unchanged (all bonded). | **v2.7.5** | ops |
| AMD-20260814-4 | 2026-08-14 | C | **VC redraws committee + catch-up adopt** | VC left the frozen 6; dead packer pinned the height (view=12 still v8). Same packer resealed after VC (h=24 equivocation). 5×1024-wallet nodes OOM'd host1 | VC redraws committee from the new view seed. Same packer cannot reseal. Lagging catch-up adopts the peer body over a local dual. Soak wallets 256. | **v2.7.6** | ops |
| AMD-20260814-5 | 2026-08-14 | C | **Header gossip / pull body** | Mesh fanned full Falcon ledgers (100–250 KB) into 8192-slot queues → 1 GB+/node, OOM | Gossip `ProposeHeader` only. Body via `NeedLedgerBody`/`LedgerBody`. Byte-capped queues. Strip bodies older than 64. Keyring = self SK + registry PKs. | **v2.8.0** | ops |
| AMD-20260814-6 | 2026-08-14 | C | **Lottery = f(tip) only** | ONLINE hash in the seed → same tip, two lotteries (epoch 16 vs 17, v4 vs v11) | Seed `lottery\|v6` = network+height+tip+prev+bonds+view. Seats = bonded. ONLINE/pong = `may_pack` only. Freeze does not filter the 6 by local ONLINE. | **v2.8.1** | ops |
| AMD-20260814-7 | 2026-08-14 | C | **On-chain 4-of-6 cert** | 3-of-5 checker tally + off-chain votes: no durable uniqueness proof; silent packer did not VC on empty mp; checker double-OK unpunished | Tip / skip / VC = **4 of the 6** (packer counts). `LedgerCert` rides `parent_cert` on the next header (v3). Checker two-OK hashes slashed. VC on packer silence. | **v2.9.0** | ops |
| AMD-20260814-8 | 2026-08-14 | A | **Archive bodies on disk** | `--archive` kept every Falcon body in RAM (`max_body_ledgers=max_chain`); pretty snapshots cloned 100MB+ strings; pack dry-run cloned the KeyRing per tx; inbound fan was slot-capped only | RAM always strips after 64. Archive writes `{hash}.json` and serves tip-0 from disk. Compact snapshots. Pack swap, no KeyRing clone. Inbound 8 MB byte-cap. | **v2.9.1** | ops |
| AMD-20260814-9 | 2026-08-14 | C | **Lottery = f(tip) only** | View in the seed + VC redraw of the 6: same tip, two committees (view 3 vs 0 after snap bounce). 4-of-6 deadlocked | Seed `lottery\|v7` = network+height+tip+prev+bonds. No view, no ONLINE. VC clears pending and skip-advances the **same** 6. Snap view is not consensus. | **v2.9.2** | ops |
| AMD-20260814-10 | 2026-08-14 | C | **Reopen failed height** | VC drop + `height_sealers` ban + skip-reopen: each seat packs once, gets banned, walk wraps, all 6 burned, h=1929 never commits | Full-committee skip-QC on an **uncommitted** height clears skips **and** sealer bans. Discarded pending is not a commit. Same packer may pack the new attempt. | **v2.9.3** | ops |
| AMD-20260814-11 | 2026-08-14 | C | **Unique pending is the attempt** | After 2.9.3, VC still dropped the only 1929 body; packer minted 131→271→1696-tx hashes; OKs reset to 0; dual keep-first; tip frozen | Never VC-drop a unique pending. Re-fan that hash until 4-of-6 or full-committee reopen. Dual still VC. Soak `max_txs=128`. | **v2.9.4** | ops |
| AMD-20260814-12 | 2026-08-14 | C | **sealed_at is chain time** | Snap `now_ms` after power-cut: host1 10:50 vs host2 13:33 (~2.7h). Ingest compared stamp to **local** now → `sealed_at too far in future` → honest 1929 dropped | Pack/ingest bound `sealed_at` to `[parent, parent+60s]`. Load `now_ms = max(snap, tip.sealed_at)`. A6 kept: one ledger cannot skip hours of settle. | **v2.9.5** | ops |
| AMD-20260814-13 | 2026-08-14 | C | **Pack root = f(sealed_at)** | 2.9.5 stamped `pack_sealed_at()` then still folded settle at snap `now_ms`. Live: now=17654800 vs sealed=7209800, last_settled=240 → packer self-rejected 1929 (`state root mismatch`) | `pack_as` folds + selects + roots at the same stamp it writes. Ingest already used `ledger.sealed_at_ms`. One apply path (2.7.2). | **v2.9.6** | ops |
| AMD-20260814-14 | 2026-08-14 | C | **Uncommitted OKs die with the attempt** | Snap kept `ok_by_height_voter` at h=1929 after pending was wiped. 2.9.6 packed a new honest body; every prior voter looked like a double-OK → 0 fresh votes, unique pending stuck | Load/reopen drop votes + slashes for `h > tip`. Double-sign is live-attempt only. Pending body is not restored, so its votes cannot be. | **v2.9.7** | ops |
| AMD-20260814-15 | 2026-08-14 | C | **Packer feed = hub inventory** | Drain: 370 applicable on v1, winner v9/v10 empty. 150ms NeedTxs grace + `len≥8` blocks pull + arrival-order export + hub-only 32-tx sample. Skip 1/4 then **12s VC** → ~20 tx/s | Applicable-first export. Winner re-pulls while `!applicable`. Grace 800ms. Hubs push heads to **all** peers on the tx plane. Progress-skip waits out the pull budget (P1/P6). | **v2.9.8** | ops |
| AMD-20260814-16 | 2026-08-14 | C | **Progress-skip is offline only** | 2.9.8 fed every val. All observers had applicable → skip-QC the live winner in 3.8s → 7880 skip-walk, 0 proposals | Progress-skip = stale pong / lag only. Live silence is view-change. Observer mempool is not a fault. | **v2.9.9** | ops |
| AMD-20260814-17 | 2026-08-14 | C | **Re-certify committed tip** | 2.9.8/9 snap at 7879 after catch-up: `certs[7879]` missing (cert rides h+1). Every packer `no 4-of-6 cert` → 12s VC walk, tip frozen | Committee re-OKs the committed tip hash until `certs[tip]` exists. Then pack carries parent_cert. | **v2.9.10** | ops |
| AMD-20260814-18 | 2026-08-14 | C | **Live pack timeout** | 2.9.10 drain: remote packers worked, then one **15s** VC hole (live winner, no proposal, 2.9.9 forbids skip). 2s raw skip would fire mid pack_budget+grace | Skip live winner only after `pack_budget+grace+1.5s` (~3.3s here) and **no proposal**. Unavailable still 3s. Poison/idle still no skip. | **v2.9.11** | ops |
| AMD-20260814-19 | 2026-08-14 | C | **Skip = network work, not voter mempool** | 2.9.11 flood: live-pack skip needed `has_applicable` on the **voter**. Light checkers empty → 1–2 skip votes, no 4-of-6, **12s VC** ×2, average 13 tx/s | `network_has_pack_work` = local applicable **or** fresh hub Pong `mempool_len>0`. Committee can skip a silent winner without holding the backlog. Idle hubs → no skip. | **v2.9.12** | ops |
| AMD-20260814-20 | 2026-08-14 | C | **Push body to checkers** | 10313: locals OK in 20ms; host2/droplet OK **+14.5s** (still voting previous height). Header gossip + NeedLedgerBody-to-forwarder: remotes ask a peer that has no body | Packer/hubs push `LedgerBody` on the **tx** plane (not cons queue). Checkers retry NeedLedgerBody until they have it. Not a full-mesh ProposeLedger flood. | **v2.9.13** | ops |
| AMD-20260814-21 | 2026-08-14 | C | **Skip = unavailable; dish winner+backup** | 2.9.11 skipped pong-fresh remotes after 3.3s with no staged body (v5 packed 11581 then skip-QC at 11585). Feed sprayed everyone *after* height open | Skip = stale pong / tip behind only. Header/`want_body` = already proposed. Hubs dish one ledger to **winner + first backup** at tip commit. Copies stay in mempool until commit. Gap only if both seats fail. | **v2.9.14** | ops |
| AMD-20260814-22 | 2026-08-14 | C | **Designated lanes; body to committee** | Drain 13098: 17-tx body QC +15s. Header on cons; body on the 4 MB tx pipe next to 128-tx dishes. Silent drop; remotes pulled header-only peers. Same-host 4-of-6 is luck | Three lanes: cons (header/OK/skip), **body** (LedgerBody to the QC 6 only), inventory (dish/submit, yields). Mainnet assumes remotes must OK. Not a full-mesh ProposeLedger. | **v2.9.15** | ops |
| AMD-20260814-23 | 2026-08-14 | C | **Catch-up lane** | v3: `NeedLedgers dropped (cons queue full)` while 27 behind. Pull shared the vote pipe; soak votes evicted join/residual | Fourth lane: `NeedLedgers` on **catchq**, prefer hubs. Votes stay on cons. `LedgersBatch` already directed. Public join must not starve. | **v2.9.16** | ops |
| AMD-20260814-24 | 2026-08-14 | C | **Startup wait for peer tips** | 2.9.16 resume: locals snap 16984 packed 16985 before droplet (snap 16993) Hello'd. New 16993 ≠ droplet 16993 → residual `bad parent`, catch lane flood | No pack until a peer tip ad or 12s grace. Catch-up conflict backs off 5s. Forked joiners: operator hard-reset, not auto-rewind. | **v2.9.17** | ops |
| AMD-20260814-25 | 2026-08-14 | C | **Settle = committed work** | Droplet residual-applied 20870–20921 then `state root mismatch at h=20922` (first ledger of epoch 1369). Epoch 1368 had 126 ledgers; `epoch_work_from_chain` used `txs.len()` on the RAM body window. v1 `--archive` stripped to 64. v4/v11 loaded v1's snap (`archive=true`), never opened a store, kept every catch-up body | Settle uses incremental pack/fees/watcher from commits (snap + accept). Checker pot stays equal-per-bonded. `fee_total` on the header survives strip. CLI `--archive` wins over snap; no store ⇒ strip RAM. Historical 20922 stays poison — joiner needs a snap taken after that height. | **v2.9.18** | ops |
| AMD-20260814-26 | 2026-08-14 | C | **Replay 2.9.17 settle** | 2.9.18 incremental rejected host2 residual at **34799** (packed under 2.9.17 body-window). First 2.9.18 pack after upgrade (35001) settled epoch 1481 incrementally even though that epoch's headers still have `fee_total=0` | The *trigger* ledger selects the formula: `fee_total=0` → 2.9.17 body walk; `fee_total>0` → incremental. Pack always incremental. | **v2.9.19** | ops |
| AMD-20260814-27 | 2026-08-14 | C | **Catch-up one inflight, byte-capped** | 6 remotes stuck 3–4k behind. Live +122 / 12s; remotes +0. Logs: `NeedLedgers dropped (catch lane full)` ×2 per pull. 2 MB / 64-slot catchq; fan to 3 hubs; `ledgers_from` skipped missing bodies and returned a later window the joiner cannot apply | One NeedLedgers to the first hub that accepts. Inflight until batch or 2.5s. `ledgers_from` is contiguous + 1.5 MB cap. Catch lane 16 MB. Joiners more than a few hundred behind take a live snap — they cannot Falcon-verify history faster than this soak packs. | **v2.9.20** | ops |
| AMD-20260814-28 | 2026-08-14 | C | **Archive serves snapshots** | Catch-up cannot close a multi-thousand height gap against a live head. Ops was scp'ing v1.json | Archive Hello/Pong ads `archive` + `snap_height`. Lag ≥ 1024 → `NeedSnapshot` / chunked file on the catch lane. Joiner `adopt_peer_snapshot` (keeps id/keys/CLI archive flag) then residual NeedLedgers. | **v2.9.21** | ops |
| AMD-20260814-29 | 2026-08-14 | C | **Pin served snap; archive-only** | v4 got offer 47213 / 932 chunks then `snap hash changed` (v1 re-saves every 60s). Fallback sent NeedSnapshot to v2 → `not archive`. New offers reset the transfer. Joiners froze (wants_snapshot blocks NeedLedgers) | Serve a pinned `.serve.json`; do not rotate while chunks are in flight. Snap msgs never fall through to a non-archive. Ignore mid-flight offers. No archive ad → residual catch-up. | **v2.9.22** | ops |
| AMD-20260815-1 | 2026-08-15 | C | **Join snap is tip + 128** | Overnight: 7 vals at 158k; v4/v5/v10/v11 frozen at 53k (104k behind). Wire snap was the 160 MB 20k-tail JSON as 612 hex chunks; droplet hung after the offer. `wants_snapshot` blocked NeedLedgers | Archive serves `to_join_snapshot` (tip state + 128 ledgers). Retry the current chunk. Residual NeedLedgers still runs if the snap transfer stalls. Adding a val = join-snap + residual, not from=1. | **v2.9.23** | ops |
| AMD-20260815-2 | 2026-08-15 | C | **Join snap is exclusive push** | v4/v11 got Offer (2.2 MB / 9 chunks) and never adopted under soak. 2.9.23 interleaved `NeedLedgers` after 8s stall; `LedgersBatch` (~1.5 MB Falcon verify) on the same inbound TCP as `SnapshotChunk` blocked the read task. Stop-and-wait + `try_send` drop of chunk 0 on a 500 ms DERP path. Ops scp'd v1.json again | Archive writes Offer **then every chunk** before any other line on that session. Pull owns the catch lane — no NeedLedgers until adopt or 15s abort (then `NeedSnapshot` again, never residual). Out-of-order assembler + 4-chunk hole fill. `SNAPSHOT_PULL_LAG` 1024 → 256 (residual cannot win a soak race). | **v2.9.24** | ops |
| AMD-20260815-3 | 2026-08-15 | C | **No residual while snap wanted** | Soak proof: v4 Offer→adopt 170538→171372 in 6s (2.2 MB / 9 chunks). v11 sent NeedSnapshot every 5s for 3+ min with no Offer. Ticker still called `take_catchup_pull` before an assembler existed; LedgersBatch owned v1's inbound | `take_catchup_pull` returns None while `wants_snapshot`. Catch lane is snap-only until adopt or no archive ad. | **v2.9.25** | ops |
| AMD-20260815-4 | 2026-08-15 | C | **Rotate stale serve pin** | After adopt 172587, v4/v11/v9 asked every 5s. Archive kept serving that same 2 MB file; `last_snap_chunk_ms` never went idle (45s rule); live join snap sat at 178k unused. Offers with tip ≤ local ignored. Lag 5k+ | Pin rotates when live−pin ≥ 256 **or** idle 45s. Mid-flight old sha → `snap hash changed` → new NeedSnapshot. Do not copy the 160 MB full snap over `.join.serve.json` at start. | **v2.9.26** | ops |
| AMD-20260815-5 | 2026-08-15 | D | **Public 2300 ceremony pack** | Need a cut sheet that is not the 2200 soak flags | Docs + `pl-start-public-2300.sh` (`CONFIRM=2300`). 7d epoch, first_claim=8, 14d unbond, ports 19301+, new keys. 2200 soak left running. | **v2.9.26** | ops |
| AMD-20260815-6 | 2026-08-15 | C | **No snap treadmill** | In-mesh vals were "fed" a new tip every 256 ledgers. Residual re-verified every Falcon tx so they could never outrun the head | Snap only at tip=0 or lag≥2048. Certified catch-up applies state without per-tx Falcon verify (packer+QC+root still bind). Adopt keeps archive ads. Live pack still verifies. | **v2.9.27** | ops |
| AMD-20260815-7 | 2026-08-15 | C | **Seat binaries** | One `falcon-pl-node` made 2-core vals load a hub tail | `falcon-pl-archive` / `-hub` / `-val` (same image, argv0 role). Val caps RAM chain at 128. | **v2.9.28** | ops |
| AMD-20260815-8 | 2026-08-15 | C | **Browser WatcherHeartbeat** | WatcherCredit self-writes work + bulk slots (mint-class). Browser product is presence ticks only | `TxBody::WatcherHeartbeat`: fee + current wall slot, **0 work**. ctl `heartbeat`. Account/status expose `watcher_in_slot` / `watcher_current_slot` for enter/exit. | **v2.9.29** | proto |
| AMD-20260815-9 | 2026-08-15 | B | **Close mint holes** | `TxBody::WatcherCredit` self-writes work; `CreditAsset` mints native FPL | WatcherCredit apply + WireMsg always `disabled`. CreditAsset / native `credit_asset_admin` require `allow_admin_wire` (off on `--lock-public-wire`). Work = rail header/deposit only. | **v2.9.30** | proto |
| AMD-20260827-1 | 2026-08-27 | C | **Lagging public hub freeze** | v1 restart + NeedLedgers pinned to first hub (v4 2.9.41) + `SubmitTx` `Ok deferred-until-catchup` + idle/header inject only at v1 → empty live mempool, 10h VC at h=237740, SPV lag 37. Skip correctly idle-gated. | Rotate NeedLedgers; skip hubs whose advertised tip is not ahead. Lagging archive join-snaps after 3× empty residual. `SubmitTx` while lagging is **Err** (not Ok). Header submitter / idle-tick fail over to a `tip_lag=0` hub. **Wire-compatible on 2.9.42** (no Hello bump). | **v2.9.42** | ops |
| AMD-20260812-13 | 2026-08-12 | C | **Soft QC static** | P0 set soft=`max(3,ceil(2n/3))` → tip slowed as set grew; soft>hard at n=13 | restore freeze: soft=`min(soft_ok_votes,n)` (3); hard=`ceil(n×0.6)` only | **v2.3.5** | ops |

### AMD-20260814-20 detail (v2.9.13 committee body push)

**Problem:** 2.8.0 gossiped `ProposeHeader` only and replied `NeedLedgerBody` to the header-forwarder. Locals on the packer host OK'd fat pending in ~20ms. Remotes (host2 / droplet) stayed on the previous height and OK'd **+14.5s**. Average TPS is then `pack_size / (pack + 14s remote QC)`, not pack_size / pack.

**New (v2.9.13):**

| Rule | Behavior |
|------|----------|
| Push | Packer fans `LedgerBody` on the **tx** plane at propose. Hubs re-push pending every 400ms. Light checkers do **not** flood |
| Pull | Header without body → `NeedLedgerBody` to **hubs** immediately, retry 200ms until staged or tip advances |
| Queue | TxsBatch yields at 75% of the 4 MB tx cap so a fat body still fits |
| Not | Full-mesh `ProposeLedger` on the consensus queue (2.8.0 OOM) |

**Tests:** `want_body_pull_retries_until_have_it`, `pending_body_push_is_packer_or_hub_only`.

### AMD-20260814-21 detail (v2.9.14 skip + dish)

**Problem:** One family of patches (2.9.8–2.9.13) treated “I have not staged this packer's body” as a packer fault. 2.9.11 then skip-walked **pong-fresh** remotes (v5/v9/v10/v3) in 3.3s while they were still pulling or packing Falcon. Light winners opened the height empty because hub feed started *after* the height opened and went to everyone.

**New (v2.9.14):**

| Rule | Behavior |
|------|----------|
| Skip | `active_packer_unavailable` only (stale pong or tip behind) |
| Header | `want_body` / pending / seen body → do not skip |
| Live silence | View-change (12s bound), not a 3s walk |
| Dish | At tip commit, hubs send one ledger (`max_txs`) to **winner + first backup** |
| Copies | Unpacked txs stay in every mempool until commit. Next height dishes again |
| Gap | Only if both dish seats fail to seal (winner dead **and** backup dead) |

**Tests:** `live_pong_fresh_winner_is_not_skipped`, `want_body_blocks_progress_skip`, `light_checker_does_not_skip_live_winner`, `dish_seats_are_winner_and_first_backup`.

### AMD-20260813-2 detail (v2.6.0 open-height liveness)

**Problem (multi-host tip≈32 freeze):** Under load, mempools filled with **future-seq only** (`seq > account.need`, no next-seq head). Recovery gates required `has_applicable_tx` for pack progress, progress-skip, and view-change → all three closed → open height stuck for hours (snapshots only).

**Secondary bug (pipeline wipe):** An intermediate sanitize that **always** dropped `seq > need` before every pack destroyed healthy sequential queues (`need, need+1, …`). Only the packer lost `need+1…`; other nodes kept them → packer-empty / multi-height harness failure and production packer-local stalls.

**New (v2.6.0) protocol rules:**

| Rule | Behavior |
|------|----------|
| Poison | `mempool_poisoned` ⇔ non-empty ∧ ¬`has_applicable_tx` |
| Sanitize | Always prune stale (`seq < need`). Drop future (`seq > need`) **only when poisoned**. Healthy pipelines stay intact. |
| Pack | `pack_or_skip` sanitizes first; on no-applicable forces poison clear; never self-skips on poison |
| Progress skip | Offline/lagging active seat → skip after `progress_timeout`. Live seat with applicable work → skip. Poison-only → no skip thrash |
| View-change | After `proposal_timeout` (4×progress): stuck pending **or** open stall with pre-sanitize (applicable \| poison \| no seat) → soft-QC view-change. **Not** idle empty + merely unavailable (wire thrash) |
| Soft QC floor | With ≥2 bonded checkers, soft need **≥ 2** (never solo soft-commit under asymmetric pongs — multi-host fork class) |
| Catch-up wire | Peer write priority: **catch-up reply → reply → cons → tx**; hubs never re-gossip SubmitTx; Hello **validators-only** publics (not 1k load wallets); no VC/skip while lagging or cold tip=0 |
| Reopen | When **every seat in lottery order** is skip-QC'd → clear skip set + reopen height (not \|bonded\| count) |

**C19 (2026-08-13):** tip=49 / empty mempool / load-net still up was **inject Recv-Q hang**, not a consensus freeze. Restarting load-net only advanced 49→158 same hash on 3 hubs. See `DIG_20260813_tip49.md`.

**2.7.0 (fleet-cut):** A1 registry-only Falcon keys; A2 signed votes (`require_vote_sigs` on Falcon node); A3 mesh TCP admin lock + unix `--admin-sock` (ctl tick/hard-reset/snapshot/rails via `--admin-sock`; bond/unbond/claim/vault/amm already `SubmitTx`); A4 tip-committed `soft_need` (no pong cap); A6 live commit ignores `sealed_at_ms` (catch-up still syncs); A7 gossip `ProposeLedger.catchup` no longer skip-QC; A8 `--scheme hmac` refused without `--allow-dev-crypto`; A9 integer lottery `seed % total_bond`; A10 Hello `product_version` + `network_id`; crate and `PRODUCT_VERSION` both **2.7.0**.

### AMD-20260813-3 detail (v2.7.0 public-safety)

**Problem:** A public or extra joiner on the mesh TCP port could overwrite account keys, forge unsigned QC, mint via `CreditAsset`/`WatcherCredit` WireMsgs, load a snapshot, or mix crate/product versions.

**New (v2.7.0) protocol / ops rules:**

| Rule | Behavior |
|------|----------|
| Keys | `insert_public` never overwrites; Falcon SubmitTx verifies the registered pk |
| Votes | `LedgerOk` / `SkipPacker` / `ViewChange` / `OnlineSetVote` signed; `require_vote_sigs` on Falcon fleets |
| Mesh TCP | Admin WireMsgs (`Tick`, `SealNow`, `Snapshot*`, `OperatorHardReset`, rails, local Vault/Bond/Credit…) return `admin-only` when `allow_admin_wire=false` |
| Operator | `--admin-sock` unix listener (0600) applies those msgs; ctl `--admin-sock` |
| hmac | Node refuses `--scheme hmac` unless `--allow-dev-crypto` |
| Version | Workspace crate = `PRODUCT_VERSION` = `2.7.0`; Hello rejects mismatch |

**Tests:** `sanitize_keeps_sequential_pipeline`, `no_applicable_drops_future_seq_poison`, `poison_mempool_height_stall_view_change`, `offline_active_seat_progress_skipped`, `all_lottery_seats_skipped_reopens_height`, `hmac_multi_height`, `falcon512_sign_and_chain`.

### AMD-20260814-2 detail (v2.7.4 lottery committee)

**Problem:** Soft QC over all bonded is 51-of-100 at scale — tip waits on the whole set. Skip that re-lotteries mid-height can elect a second packer group (859 stall class).

**New (v2.7.4) protocol rules:**

| Rule | Behavior |
|------|----------|
| Draw | At tip commit, freeze **committee** = first `committee_size` (default 6) of the tip-fixed lottery: 1 packer + 5 checkers |
| Soft | Majority of that committee when `c ≥ 6` (4 of 6). Outsider OK votes do **not** soft-pass |
| Hard | Still `ceil(n × 0.6)` over all bonded, capped by \|ONLINE\|. Does not block the tip |
| Skip / VC | Only committee votes count. Skip walks the same 6 — no second lottery |
| ONLINE upgrade | First certified ONLINE (bootstrap → roster) redraws the frozen committee. Later mid-height ONLINE flips do not |
| Unbond | Frozen seats that drop out of ONLINE / bond are filtered; if the whole freeze is gone, redraw |

### AMD-20260814-3 detail (v2.7.5 3-of-5 checkers)

**Problem:** 2.7.4 used majority of the 6 (4) so two groups of 3 could not both commit. That is stricter than needed. The 5 checkers already cannot split into two groups of 3.

**New (v2.7.5):**

| Rule | Behavior |
|------|----------|
| Soft | `min(soft_ok_votes, \|checkers\|)` = **3 of 5**. Packer (seat 0) OK is ignored for soft |
| Unique | Two disjoint 3-subsets cannot exist in 5 |
| Hard | Unchanged: `ceil(n × 0.6)` over all bonded |
| Skip / VC | Same 3-of-5 checker bar |

### AMD-20260814-4 detail (v2.7.6 VC redraw + catch-up adopt)

**Problem:** 11-val 2.7.5 soak stalled at h=23/24. v2 sealed two bodies; host1 rejected catch-up as equivocation. View-change 12 times still showed winner v8 because the frozen committee was not redrawn. Five nodes × 1024 Falcon wallets then OOM-killed host1 (~3 GB RSS each).

**New (v2.7.6):**

| Rule | Behavior |
|------|----------|
| VC | Redraws `schedule_committee` from the new view seed. ONLINE freeze stays |
| Reseal | A packer who already produced a body at `height` cannot pack again after VC |
| Catch-up | If lagging (`h = tip+1`) with a local uncommitted dual, adopt the peer body. No slash on that path (slash would break the state root) |
| Soak | `--load-wallets 256` |

### AMD-20260814-5 detail (v2.8.0 header gossip)

**Problem:** `ProposeLedger` fanned the full Falcon-signed body (100–250 KB JSON) into 8192-slot per-peer queues. Five nodes on host1 grew to 1–3 GB RSS and OOM'd.

**New (v2.8.0):**

| Rule | Behavior |
|------|----------|
| Gossip | `ProposeHeader` only (hash, packer, roots, sig). Votes stay hash-only |
| Body | `NeedLedgerBody` / `LedgerBody` on the requesting connection. Never mesh-fan bodies |
| Retention | Strip `txs` from committed ledgers older than `max_body_ledgers` (64). Catch-up serves only ledgers that still have a body |
| Keys | Node loads registry publics + **own** secret. Not 1000 wallet SKs |
| Queues | 8 MB consensus / 4 MB tx per peer. Headers drop first at 75% so votes still form QC |

### AMD-20260814-6 detail (v2.8.1 tip-only lottery)

**Problem:** Seed included `online:{local tag}`. Droplet missed epoch-17 ONLINE QC, catch-up froze height 180 with epoch-16 tag. Same tip 179, packer v4 vs v11.

**Invariant:** `lottery(A) == lottery(B)` whenever `A.tip_hash == B.tip_hash`.

**New (v2.8.1):**

| Rule | Behavior |
|------|----------|
| Seed | `lottery\|v6\|network\|height\|tip\|prev\|set:id:bond\|view` — no ONLINE |
| Seats | Bonded, not jailed, as of that tip |
| ONLINE / pong | `may_pack` only. Skip walks the same 6 |
| Freeze | `schedule_committee = draw_committee()` from the tip seed. Do not filter that 6 by local ONLINE |

### AMD-20260814-7 detail (v2.9.0 on-chain 4-of-6 cert)

**Problem:** 3-of-5 checker tally moved balances with no durable committee proof on the next header. Packer OK did not count. Silent packer + empty local mempool did not view-change. Checker double-OK was free.

**New (v2.9.0):**

| Rule | Behavior |
|------|----------|
| Commit / skip / VC | **4 of this height's 6** committee members. Packer counts. n &lt; 6 keeps the small-set rule |
| Cert | Those 4 `(voter, sig)` form a `LedgerCert`. Next ledger carries `parent_cert` and binds its hash in header v3 |
| Height 1 | `parent_cert = None` |
| Equivocation | Packer two bodies → slash (existing). Checker two OK hashes at one height → slash |
| View-change | After `proposal_timeout`, every committee seat may VC even with an empty local mempool |
| Hard | Still `ceil(n × 0.6)` of bonded. Confirm flag only. Does not replace the 4-of-6 cert |

**Tests:** `n6_commit_is_4_of_6`, `packer_ok_counts_toward_4_of_6`, `parent_cert_rides_next_ledger`, `checker_double_ok_is_slashed`, `view_change_on_silent_packer_empty_mempool`.

### AMD-20260814-8 detail (v2.9.1 archive on disk)

**Problem:** `--archive` set `max_body_ledgers = max_chain_ledgers` and skipped strip. v1 held every Falcon-512 body in RAM (~243 KB/ledger). Pretty snapshots cloned that into a 100MB+ `String` every 60s; glibc kept the high-water mark. Pack dry-run cloned the whole KeyRing per candidate tx. Inbound joiner queues were 512 slots, not byte-capped. Non-archive RSS still ~50× the 20 MB snapshot and climbing.

**New (v2.9.1):**

| Rule | Behavior |
|------|----------|
| RAM | Every node strips txs older than `max_body_ledgers` (64). `--archive` no longer raises that cap |
| Disk | Archive opens `{snapshot-parent}/bodies/{id}/{ledger_hash}.json` (compact). `ledgers_from` / `NeedLedgerBody` read disk |
| Tests | `archive=true` with no store still keeps RAM bodies (in-process tip-0 test) |
| Snapshot | Compact JSON (`to_vec`), not pretty-print |
| Pack | `try_protocol_tx` swaps draft slices onto `self` — no KeyRing / Node clone |
| Inbound | Same 8 MB byte-cap as outbound consensus |

**Tests:** `archive_disk_store_strips_ram_and_serves_tip0`, existing `archive_keeps_bodies_so_tip0_can_catch_up`, `stripped_bodies_cannot_serve_tip0_catchup`.

### AMD-20260814-9 detail (v2.9.2 tip-only lottery)

**Problem:** `lottery|v6` included `height_view_round`. VC QC called `draw_committee()` and wiped skips. Snap load zeros view. After a subset bounce, host1 sat at view=3 / `[v5…]` and remotes at view=0 / `[v3…]` on the **same tip hash**. Skip 1/4, no 4-of-6, tip frozen.

**New (v2.9.2):**

| Rule | Behavior |
|------|----------|
| Seed | `lottery\|v7\|network\|height\|tip\|prev\|set:id:bond` — no view, no ONLINE |
| Freeze | `schedule_committee = draw_committee()` at tip commit only |
| ONLINE cert | Updates live ONLINE / status. Does **not** redraw the 6 |
| View-change | 4-of-6 of the **tip** 6. Clears pending, skip-advances current packer, no redraw, no skip wipe |
| Restart | View=0, skips empty, 6 recomputed from tip. Same tip ⇒ same 6. Silent #1 is skipped again |

**Revokes:** AMD-20260814-4 (2.7.6) “VC redraws committee.” Skip cursor on the tip-fixed 6 is the recovery.

**Tests:** `same_tip_divergent_view_same_lottery`, `view_change_does_not_redraw_committee`, `view_change_clears_stuck_pending`, `view_change_same_packer_cannot_reseal`, `same_tip_divergent_online_same_lottery`.

### AMD-20260814-10 detail (v2.9.3 reopen failed height)

**Problem:** VC drops pending and records the packer in `height_sealers`. Skip walks the same 6. When all 6 are skip-QC'd, skips reopen but **bans stay**. Second lap: every seat that already packed logs `already sealed h=N after view-change`. After one full cycle the committee is burned and the height never commits (live 2.9.2, h=1929, view 170).

**Invariant:** A discarded pending is **not** a commit. Sealer bans last for **one walk** so the next seat is tried. An agreed failure of the walk (all 6 skip-QC'd, tip still T) **reopens the attempt**.

**New (v2.9.3):**

| Rule | Behavior |
|------|----------|
| Walk | VC / skip still bans that packer from a second body *this walk* |
| Reopen | When every committee seat is skip-QC'd at `tip+1`, clear skips, skip votes, `height_sealers`, and `seen_at_height` for that height |
| Never | Reopen a height that is already the committed tip |
| Equivocation | Still slash two different OK hashes from the same packer at one height while both are live |

**Tests:** `full_committee_skip_reopens_clears_sealer_bans`, existing `view_change_same_packer_cannot_reseal` (ban holds during the walk).

### AMD-20260814-11 detail (v2.9.4 unique pending)

**Problem:** 2.9.3 let the packer propose 1929 again. `maybe_view_change_vote` still treated unique pending as stuck and VC-dropped it. Each drop the packer `pack_as`'d a fatter body (131 → 1696 txs). Checkers saw two hashes, both 0 votes. Tip 1928 froze.

**New (v2.9.4):**

| Rule | Behavior |
|------|----------|
| Unique pending | The attempt body. Seal loop re-fans header + OK. VC does **not** drop it |
| Dual / silence | VC still runs (no pending, or conflicting hashes) |
| New hash | Only on full-committee reopen (new attempt) or first pack of the walk |
| Pack size | Private soak `--max-txs 128` so a Falcon body can QC inside the timeout |

**Tests:** `unique_pending_survives_view_change`.

### AMD-20260812-8 detail (P0)

**Problem:** Private multi-host mesh forked at ~h=130 with **no load** within ~36s of restart. Root causes:

1. Lottery seed included per-node **pong-fresh live set** → different winners on same tip  
2. Progress-timeout **SkipPacker** applied without quorum → divergent skip sets  
3. Tip advanced on **self-accept** before mesh soft OK  
4. Live fork armed **hard-resync from genesis** → thrash amplifier  

**New (v2.3.0):**

| Rule | Behavior |
|------|----------|
| Lottery | Seed = `H(network_id‖height‖tip_hash‖skip:…‖set:…)` over **bonded non-jailed** set only. Pong is pack permission, not election. |
| Soft need | ~~`max(soft_ok_votes, ceil(2n/3))`~~ → corrected **v2.3.5** to static `soft_ok_votes` (see AMD-13) |
| Skip | `SkipPacker{voter}` tallies votes; lottery mutates only after soft QC |
| Tip | Live proposals **stage**; tip advances only after soft QC. No multi-height pipeline without soft. |
| Fork | Conflicting proposal at height → reject, keep first; **no** live hard-resync. Catch-up state-root may still hard-resync. |

**Tests:** `lottery_agrees_with_asymmetric_pong_views`, skip-QC standby path, full `fd-pl` lib suite (57).  

**Deploy:** Fresh fleet restart on **2.3.0** only (do not mix with 2.2.7). Expect slower height open until soft QC (correct).

### AMD-20260812-13 detail (soft static)

**Problem:** P0 soft threshold tracked `ceil(2n/3)`, so at n=13 soft_need=9 while hard_need=8 — soft became stricter than hard and tip lag grew with the validator set. Contradicts freeze sheet / GROWTH prototype: **soft=3 static, hard scales**.

**New (v2.3.5):**

| Rule | Behavior |
|------|----------|
| Soft need | `min(soft_ok_votes, n)` with default `soft_ok_votes=3` — **does not grow with n** |
| Hard need | `ceil(n × hard_ok_ratio)` with default ratio **0.6** — **only hard scales** |
| Skip QC | Still uses soft threshold (fast failover, same 3-vote bar) |

**Tests:** `soft_static_hard_scales_with_n`.  

**Deploy:** Fleet-wide **2.3.5** (rolling restart OK; no genesis wipe required).

### AMD-20260812-9 detail (P1)

**Goal:** Liveness under multi-host without re-introducing forks or OOM.

| Rule | Behavior |
|------|----------|
| Pre-pack NeedTxs | Lottery winner with thin mp / no applicable **must** issue NeedTxs once per open height, then wait `NEED_TXS_PRE_PACK_GRACE_MS` (150ms) for `TxsBatch` before sealing empty. |
| Chain bound | `max_chain_ledgers == 0` **rejected** → clamp to 20_000 (CLI warn + `ChainConfig::sanitize`). |
| Vote / catch-up bound | `pending_ledgers` cap 256; prune `ok_votes`/`soft_ok`/`hard_ok` to live hashes; prune skip maps; TX wire queue 2048. |
| Pong | Liveness + tip ad + pack permission only (election already tip-deterministic from P0). |

**Tests:** `pre_pack_need_txs_gate`, `max_chain_ledgers_zero_clamped`, full lib suite.

**Deploy:** 2.3.1 fleet-wide; keep `--max-chain-ledgers` ≥ 1 (never 0).

### AMD-20260812-10 detail (P2)

**Goal:** Heal lag without genesis thrash; intentional rebuild is operator-only.

| Rule | Behavior |
|------|----------|
| Residual catch-up | Default pull is always `from=tip+1`. Serves **committed** chain only (`ledgers_from`). |
| No auto hard-resync | Catch-up / live fork conflicts set `last_catchup_conflict` and keep tip. **Never** arm `need_hard_resync` automatically. |
| Operator hard-reset | `WireMsg::OperatorHardReset` / `falcon-pl-ctl hard-reset --reason …` arms rebuild; then `NeedLedgers(from=1)`. |
| Peer tip hash | `best_peer_tip_hash` tracked from Pong for diagnostics / status. |

**Ops recover from divergence:**  
1) Prefer re-bootstrap lagging nodes from a **healthy peer snapshot**.  
2) Only if full rebuild needed: `falcon-pl-ctl --addr HOST:PORT hard-reset --reason "…"`.

**Tests:** `catchup_conflict_no_auto_hard_resync`, `operator_hard_reset_arms_flag`, kill-winner multi-node agree (existing + extended).

**Deploy:** 2.3.2 fleet-wide; do not mix with auto-resync 2.2.x.

### AMD-20260812-11 detail (tip-only lottery)

**Problem:** Partial skip QC meant some nodes had `skips=['v3']` (winner v6) and others `skips=[]` (winner v3) → nobody packed.

**Rule:** Lottery seed is **only** tip-committed:

```text
H(network_id | height | tip_hash | prev_packer | set:id:bond,…)
```

- **No** pong live-set  
- **No** skip QC map  
Same tip + same bonded registry ⇒ same winner on every val.

Skip gossip is observability only (future: tip-bound view-change certs for offline failover). Progress-timeout skip no longer re-elects.

**Tests:** `lottery_same_on_all_nodes_despite_asymmetric_skips`, `shared_skip_does_not_change_lottery`.

---

## 9. Checklist for authors

- [ ] Class identified (A–E)  
- [ ] Old → new values explicit  
- [ ] Tests or soak pointer  
- [ ] AMD id + §8 row  
- [ ] EXPANSION_PLAN §11 line  
- [ ] `params_version` bumped if Class B  
- [ ] Non-goal list checked (§9) — no sneak Class E  

---

*Falcon PL does not use “whoever has most FPL rewrites the rules” during private expansion or first public testnet. Operators document, soak, and ship; token-weighted governance is a future mainnet design problem.*
