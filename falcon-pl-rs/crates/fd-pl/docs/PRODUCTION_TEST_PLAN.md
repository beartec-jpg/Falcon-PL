# Falcon PL — production readiness test & improvement plan

**Network:** public testnet 2300 (Falcon-512)  
**Code:** `falcon-pl-rs/crates/fd-pl` (scripts: `scripts/pl-prod-chaos-harness.sh`, `scripts/pl-prod-soak-check.py`)  
**Consensus doc:** `crates/fd-pl/docs/CONSENSUS_ROUNDS_v2.10.md`  
**Live as of 2026-10-05:** tip ~370427, Phase 2 on from **H=369998**, committee v1/v3/v6/v10/v11/v13 (v12 jailed), product `2.9.46`  
**Branch for new tests:** `test/prod-readiness-suite` (follow-up to PR #5). Phase 2 H baked as a 2300 network constant; status `bridge_cfg` + soak config-drift check added.

British English. Concrete pass/fail. No secrets in this document.

---

## 1. Goals

Take Falcon PL from “mostly works on a quiet testnet” to **production standard**:

1. **Liveness under fault** — dual proposers, silent leaders, split votes, rolling restarts, and jail/join never leave the tip permanently flat while work exists.
2. **Safety** — no false self-jails; real equivocation still jails; certs/rounds verify across upgrades.
3. **Operability** — idle ≠ stalled; alerts fire only when mempool/work is waiting; soak checks run without Tailscale keys in chat.
4. **Upgrade discipline** — Phase-style flags and binary rollouts are rehearsed locally before touching live seats.
5. **Bridge/wallet continuity** — bridge-out burns resume cleanly after stalls; catch-up never blocked by stale `bridge.json`.

**Definition of “production standard” (exit criteria)** — all must hold for ≥14 days on 2300 *and* in the local 6-seat harness:

| # | Criterion | Pass |
|---|-----------|------|
| P1 | No tip stall > 5 min while `has_applicable_tx` or mempool work | Soak + watchdog |
| P2 | Competing proposals resolve via view-change / next round within 2 round timeouts | Harness dual-proposer + live soak |
| P3 | Rolling restart of one seat never re-stalls peers with stale votes | Harness restart-while-pending |
| P4 | Committee size stays 6 after jail; spare bonded seat enters lottery | Unit + harness jail+bond |
| P5 | No validator self-queues slash evidence; peers still slash real double-OKs | Unit tests |
| P6 | Idle chain (empty mempool, no work) never raises liveness alert or idle VC churn | Unit + soak |
| P7 | Catch-up from archive snapshot reaches tip within SLA (≤15 min from lag ≥500) with current bridge config | Ops drill |
| P8 | Coordinated binary upgrade (Phase N) rehearsed locally; Hello/`product_version` match | Upgrade checklist |
| P9 | Bridge-out: pending burn resumes; no double-burn on retry | Wallet PR + e2e |
| P10 | Unit suite green on CI for consensus/lottery/slash; known non-consensus failures tracked | CI gate |

---

## 2. Risk areas (from incidents + gap analysis)

### 2.1 Consensus / liveness
- **3-3 split at quorum 4** (h=369421, 369521): two bodies, neither reaches 4; legacy re-vote treated as double-vote; VC substring miss; `open_stuck` gated on empty pending.
- **Dual proposers** from idle VC churn, local skip cursor, late bodies after skip.
- **Rolling restart** cannot clear: peers re-gossip old-round votes.
- **False double-vote / self-incrimination** when seal loop re-fans OK after pending switch.

### 2.2 Lottery / bonding / jailing
- Committee capped at 6; jail drops a seat; spare bonded (v13) must enter draw.
- Pack/QC paths that refuse when bonded &lt; 6 (v4 LC) vs slash that still jails at 6.
- Bond in-flight while tip stalled (v13 bond stuck until restart).
- Online-set / lottery overlap when pongs lag.

### 2.3 Catch-up / snapshots
- Lagging seat blocked by **stale `bridge.json`** (v13 incident) — config skew, not snapshot thrash.
- Snapshot + `own-votes.json` journal must preserve round locks across restart.
- Archive hub must serve bodies for lag ≥ hundreds.

### 2.4 Bridge-out / walletd
- Burn submitted, tip stalled → wallet “withdraw note did not pack”; retry must resume not re-burn (wallet PR #58).
- walletd / Sepolia openClaim path after burn lands.

### 2.5 Upgrades / rollouts
- Phase 1 (safe mixed fleet) vs Phase 2 (activation height, all seats same H).
- `PRODUCT_VERSION` / Hello rejection if binary drift.
- Bake `vote_rounds_from_height` into network constants so missing env cannot mis-verify history.

### 2.6 Ops / Tailscale
- Eight hosts; public proxy (retired 2026-10-09; site uses the host1 gateway); host1 archive `:19301`.
- Coordinated stop-all vs rolling; backups under `~/falcon-pl-public-testnet-2300/snapshots`.
- Soak must be read-only by default (this programme: **no live restarts/unjails/flag flips** unless Scott orders them).

---

## 3. Test layers

```
┌─────────────────────────────────────────────────────────────┐
│ L4  Chaos on live 2300 (Scott-approved only)                │
│     kill one seat, lag inject, config skew — never default  │
├─────────────────────────────────────────────────────────────┤
│ L3  Live soak (read-only) — pl-prod-soak-check.py           │
│     tip/mempool/committee/jailed/lag/liveness every N min   │
├─────────────────────────────────────────────────────────────┤
│ L2  Local multi-node Falcon-512/HMAC harness                │
│     pl-prod-chaos-harness.sh — 6 seats, injected faults     │
├─────────────────────────────────────────────────────────────┤
│ L1  Unit / in-process Network (hmac)                        │
│     cargo test -p fd-pl — consensus_rounds + slash/lottery  │
└─────────────────────────────────────────────────────────────┘
```

| Layer | Tooling | Runtime | When |
|-------|---------|---------|------|
| L1 | `cargo test -p fd-pl --lib` | ~1–3 min | Every PR |
| L2 | `scripts/pl-prod-chaos-harness.sh` | ~5–15 min | Every consensus PR + nightly |
| L3 | `scripts/pl-prod-soak-check.py` | continuous / cron | Always on 2300 |
| L4 | Manual runbook | as needed | After L1–L3 green |

Existing scripts to **reuse**, not replace: `pl-multi.sh`, `pl-adversarial-local.sh`, `pl-private-fleet-v22.sh`, `pl-start-*-soak.sh`, `pl-6h-endurance.py`, archive `failure_matrix.sh` / `multinode_smoke.sh`.

---

## 4. Pass / fail per scenario (L2 harness)

Harness root: `/tmp/falcon-pl-prod-chaos` (override `PL_CHAOS_ROOT`). Scheme default `hmac` for speed; `PL_SCHEME=falcon-512` for dress rehearsal.

| Scenario | Inject | Pass | Fail |
|----------|--------|------|------|
| **baseline** | 6 nodes, pay load | tip ≥ N, all tips equal | tip 0 or disagree |
| **silent-leader** | kill current lottery winner; keep paying | tip advances on survivors within 30s | flat tip &gt; 45s with mempool |
| **restart-while-pending** | submit txs; SIGTERM one voter; restart | tip advances; restarted seat catches up | permanent stall or fork tips |
| **dual-proposer pressure** | short `progress-timeout` + load with rounds from H=1 | tip advances; logs show VC/round≥1 OK | stall &gt; 60s |
| **jail+bond rotation** | (unit-primary) slash one seat with spare bonded | committee 6, tip advances | tip stuck or committee &lt; 6 |
| **coordinated restart** | stop all six, start all | tip continues from same height | height regresses or 3-3 freeze |

Exit code 0 = all selected scenarios pass.

---

## 5. Ordered backlog (bugs / gaps → fixes)

Priority = (production impact × likelihood) / effort. Items already mitigated on 2300 noted.

| Pri | Item | Status | Next action |
|-----|------|--------|-------------|
| **0** | Tip 0→1 soft-QC deadlock: `accept_ledger` records local OK before fan, `fresh=false`, peers never hear OKs | **Fixed on branch** (`falcon_pl_node` fan-on-stage) | In follow-up PR to #5; needs merge + next binary rollout |
| **1** | Dual-proposer root (idle VC + local skip + late body) | Phase 2 live (one proposer/round); H=369998 baked as 2300 constant on branch | Roll baked binary (needs approval); add L2 dual pressure to CI |
| **2** | Split-vote recovery / typed stall | Phase 1+2 live; unit covered | Keep L3 alert on `liveness.stalled` |
| **3** | Self-incrimination / false double-vote | Code + new unit tests | Clear/audit any remaining pending evidence; policy for unjail |
| **4** | Rolling restart vs stale votes | Round-scoped votes + journal | L2 restart-while-pending nightly |
| **5** | Idle vs stalled alert | Watchdog live | L3 soak must distinguish mempool work vs empty |
| **6** | Lottery after jail (spare bonded) | v13 in committee; new unit test | Automate bond+keyring join runbook |
| **7** | Catch-up blocked by stale bridge.json | Fixed ad-hoc on v13 | Done: status `bridge_cfg` (sha256 + qc_cfg_digest + drift); soak fails on qc_header_version/qc_start_height/vote_rounds/digest mismatch |
| **8** | Wallet bridge-out resume | PR #58 merged 2026-10-05 | Confirm Vercel prod deploy; e2e burn→claim |
| **9** | Minimal bonded / jail floor clarity | v4 pack refuses &lt;6; slash still jails | Document + test “jail at 6 with spare” vs “refuse pack at 5” |
| **10** | Known lib test failures (~7–10 on main) | Tracked separately | Quarantine non-consensus; fix or ignore with ticket |
| **11** | Two-phase BFT across rounds | Design risk in CONSENSUS_ROUNDS | Research spike; not blocking testnet |
| **12** | Droplet Tailscale SSH / key access | Ops | Scott grant access for proxy logs |

---

## 6. Gap analysis of current tests (~250 lib tests)

**Strong today**
- `consensus_rounds::tests` — 3-3 recover, slash vs cross-round revote, evidence sigs, late body, idle/alert, restart journal, stale votes, lock rule, legacy VC, **+ self-slash guards + jail rotation** (this branch).
- Parent cert / forged cert / undersized cert; basic equivocation jail; offline lottery smoke (`pl-adversarial-local.sh`).

**Weak / missing**
- Multi-process dual-proposer & restart-while-pending (harness fills this).
- Config-skew catch-up (bridge.json version assert).
- Bridge-out resume under stalled tip (wallet + chain paired test).
- Soak asserting committee/jailed/lag without secrets (soak script fills this).
- Upgrade rehearsal (Hello version matrix).
- Falcon-512 L2 dress (hmac-only is default for speed).

---

## 7. Cadence

| Cadence | Actions |
|---------|---------|
| **Every PR** | L1 consensus_rounds + slash tests; harness `baseline,silent-leader` |
| **Nightly** | Full L2 harness; L3 soak report; tip/committee diff vs previous day |
| **Weekly** | Falcon-512 L2 dress; snapshot restore drill; wallet bridge-out dry run on Sepolia |
| **Before mainnet** | 14-day P1–P10; chaos L4 once; external review of CONSENSUS_ROUNDS |

---

## 8. Explicit non-goals (this programme)

- Do **not** restart live validators, flip Phase flags, deploy binaries, or unjail seats unless Scott orders it.
- Do **not** paste Tailscale auth keys or validator private keys into chat or this tree.
- Do **not** treat a quiet tip with empty mempool as an incident.

---

## 9. Artefacts in this folder

| Path | Role |
|------|------|
| `PRODUCTION_TEST_PLAN.md` | This plan |
| `pl-prod-soak-check.py` | Read-only live soak / alert checklist |
| `README.md` | How to run everything |
| Repo: `crates/fd-pl/scripts/pl-prod-chaos-harness.sh` | Local multi-node chaos harness |
| Repo: `crates/fd-pl/src/consensus_rounds.rs` tests | L1 regressions |

