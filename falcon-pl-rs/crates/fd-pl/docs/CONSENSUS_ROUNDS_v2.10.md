# Round-scoped votes and per-round proposer (v2.10.0)

Branch `fix/consensus-split-vote-liveness`. Code: `src/consensus_rounds.rs`
(+ hooks in `src/lib.rs`, `src/snapshot.rs`, `src/bin/falcon_pl_node.rs`).

## Incident (testnet 2300, 2026-09-28 04:37 UTC, h=369421)

* A timeout let two seats seal one height: v12 → `58b308…`, v10 → `00349f…`.
* 6 live validators, quorum 4. Votes split 3-3 (v1, v3, v12 | v6, v10, v11).
* Every node then preferred `00349f` (lower hash) but refused the `58b308`
  voters' later OKs as "checker equivocation": `pl-ok:v1` signs only
  `(height, hash)`, so *any* second hash at a height looked like a double vote.
* View-change never fired: `maybe_view_change_vote` matched the substrings
  "conflicting proposal" / "equivocation/fork" in `last_catchup_conflict`
  (the live text was "checker equivocation …"), and a dual **with** a pending
  body was gated out entirely (`open_stuck` required `pending.is_none()`).
* Rolling restarts could not help: peers re-gossiped the old round votes.

After the 2026-10-04 coordinated restart, competing proposals happened again
at 369428 and 369439 (resolved by luck, 4/6 on one side), and v10 got a
double-vote report. Double proposals happen in normal running.

### Why seats double-propose (root causes found in code)

1. **Idle view-change churn.** Legacy `maybe_view_change_vote` treated
   *committee silence* as a stall even with no transactions. Blocks are
   load-driven, so on a quiet chain every committee seat voted a VC every
   ~10 s; each applied VC skipped the "silent" packer and walked the cursor.
   Seats apply the QC at slightly different times, so when the next tx lands
   two seats can each believe they are the active packer.
2. **The skipped seat was chosen locally.** `apply_view_change` skipped
   "the packer of my pending / seen body, else my lottery winner" — a value
   that differs per node when they hold different bodies.
3. **Late body accepted after a skip.** A slow seat's body that lands after
   the cursor moved is still a valid claim and gets staged/voted by seats
   that have not applied the skip yet.
4. **Honest re-vote looked like a double vote.** After a QC-first switch to
   the other body, the seal loop re-fanned an OK for the new pending every
   progress window. Peers recorded that as checker equivocation (this is the
   likely origin of the v10 report at 369439), and the node even queued
   evidence against itself.

## Design

Active for heights ≥ `ChainConfig::vote_rounds_from_height` (0 = off).

| Piece | Rule |
|---|---|
| Vote payload | `pl-ok:v2|net|height|round|hash|voter`. `LedgerOk.round` (omitted on the wire when 0). |
| Equivocation | Two different hashes in **one (height, round)** only. Evidence (`SlashEvidence.round`) is kept across view-change / reopen / restart and rides the next ledger. Receivers verify both signatures over `pl-ok:v2` with the same round before applying a slash; a ledger with bad evidence is rejected (an honest later-round re-voter cannot be framed). |
| Commit | ≥ 4-of-6 OKs for one hash **in one round**. `LedgerCert.round` (omitted when 0; `cert_hash` appends `|r=` only when non-zero, so legacy certs are byte-identical). |
| Lock rule | After OK'ing `A` in round `r`, a seat may OK another body in a later round only if its view of round `r` proves `A` can never reach quorum (≥ n−q+1 = 3 non-equivocating others voted something else). Otherwise it re-votes `A`. In a 3-3 split both sides are dead ⇒ everyone free. |
| Proposer | Exactly one per (height, round): `lottery_order()[round % n]` (tip-frozen committee). The skip cursor is *derived* from the round (`sync_round_skips`), so `lottery_packer`, `skip_claims_for_packer` and `validate_proposal_seats` agree on every seat in that round. SkipPacker QC and progress-skip are disabled in round mode. |
| Late/early bodies | Validated and cached (`open_bodies`, cap 8) but not staged or voted unless they already lead the vote. A packer that seals two bodies in one rotation is refused (no local slash: that would fork the state root). |
| Round timeout | ViewChange QC (4-of-6) to `round+1`. **Work-gated**: no pending body, no applicable tx and no hub inventory ⇒ no clock, no cursor walk. Clock = max(round start, first time work was seen) and is not reset by header gossip. Body in hand: `proposal_timeout × (1 + min(r,4)/2)`. Proposer silent: `2 × live_pack_timeout` (or `progress_timeout` if the proposer is offline/lagging), same backoff. **Proven split ⇒ immediate.** A QC for any round ≤ cur+64 is adopted (restarted seats re-sync their round from peers). |
| New round | Each seat re-votes its target: live lock, else the body with most votes in any round, tie → this round's proposer body → lower hash. Re-vote, don't re-mint. A body that survives 3 round advances is dropped (unless this seat is still bound by a live lock) and the round's proposer seals fresh. A full rotation (round wraps `n`) clears sealer bans. |
| Stall detection | `ConsensusStall` enum (`DualProposal`, `CheckerEquivocation`, `SplitVote`, `IngestFork`) set where the condition happens. `last_catchup_conflict` is operator text only. |
| Watchdog | `liveness_tick()` (seal loop) logs `ERROR LIVENESS ALERT …` once per `liveness_alert_ms` (default 5 min, `--liveness-alert-secs` / `FALCON_PL_LIVENESS_ALERT_SECS`) when **work exists** (pending body, applicable txs or a fresh hub inventory ad) and the tip has not advanced. A flat height with no txs never alerts. Status JSON: `liveness`, `consensus_stall`, `vote_rounds_from_height`, `round_scoped_votes`. |
| Restart safety | Open-height `round_votes` persist in snapshots (`round_votes`, optional field) and in `<snapshot>.own-votes.json` (written synchronously on every own vote). A restarted seat keeps its lock and never signs a second hash in a round it already voted in. Re-gossiped old votes stay in their own round and cannot block a newer one. |

### Also on legacy heights (before activation, no protocol change)

* Typed dual / checker-split detection drives view-change, **including when a
  pending body exists** (the 369421 shape). Legacy apply then discards that
  attempt's OKs (v1 votes cannot tell a re-vote from a double vote).
* View-change on silence requires work (no idle cursor churn).
* A seat never re-fans / records an OK for a second hash at one height and
  never queues evidence against itself.
* Liveness watchdog + status fields.

## Tests (`consensus_rounds::tests`)

* `split_3_3_at_quorum_4_recovers_next_round` — 6 seats, quorum 4, 3-3 split
  in round 0, immediate round timeout, all converge and commit in round 1;
  no evidence, no jail; cert round = 1; next height seals.
* `same_round_double_ok_is_slashed_cross_round_revote_is_not` — real
  equivocation queued, survives reopen, jails on every node via the next
  ledger; a cross-round re-voter is untouched.
* `evidence_must_be_signed_in_one_round` — relabelled cross-round sigs are
  rejected; same-round sigs verify; v1 sig refused on a v2 height.
* `late_body_from_earlier_round_is_not_staged` — single proposer per round.
* `idle_chain_never_times_out_or_alerts` — no txs ⇒ no round timeout / alert;
  first tx after an hour idle gets a full proposer window; work + flat tip ⇒
  alert, rate-limited.
* `restart_keeps_own_round_vote` — snapshot and journal keep the lock.
* `round_jump_and_stale_votes_cannot_block` — QC jump; stale round-0 votes do
  not block round 2.
* `live_lock_blocks_switch_dead_lock_frees` — lock rule.
* `legacy_checker_split_now_triggers_view_change` — pre-activation recovery.

## Rollout

**Protocol change: yes for phase 2.** After the activation height the vote
payload, cert format and evidence check differ; a non-upgraded seat cannot
verify `pl-ok:v2` certs and would stall or fork. Every node that verifies live
ledgers or catches up (all 6 validators, archive v1/host1, hubs) needs the
binary **and the same activation height**, permanently (catch-up past H
needs it to verify certs).

Note: `Hello` rejects any `PRODUCT_VERSION` mismatch. This branch does not
bump it. Build from the exact commit the fleet runs + this patch and confirm
`product_version` in status matches the live seats, or every peer will be
refused.

1. **Pre-flight.** Run a local 6-seat Falcon-512 soak with
   `--vote-rounds-from-height` set a few hundred heights in, including a
   forced dual and a seat kill/restart.
2. **Phase 1, rolling (safe):** deploy the new binary with activation 0
   (default), one seat at a time, waiting for each to rejoin the tip. Gains:
   typed stall + dual-with-pending VC, no idle churn, no self-incriminating
   re-votes, watchdog. Mixed old/new fleet is fine.
3. **Pick H** a comfortable margin above the tip. Heights only move with
   load, so drive txs (faucet) if needed and watch ETA.
4. **Phase 2 config (still before H):** set
   `FALCON_PL_VOTE_ROUNDS_FROM_HEIGHT=H` (or `--vote-rounds-from-height H`)
   in every node's systemd unit; rolling restart one at a time; verify all 6
   report `vote_rounds_from_height == H`. Abort = set it back to 0 (rolling)
   while tip < H.
5. **Cross H** under light load; watch `round_scoped_votes: true`,
   `height_view_round`, `consensus_stall`, `liveness`.
6. **Bake H in** (network constant / bridge.json) so a missing env var can
   never mis-verify history. Done for 2300 — see "Baked activation / rollback".

## Post-incident re-enable (2026-10-05)

After the soft-QC / slash freeze, Phase 2 was paused with
`FALCON_PL_VOTE_ROUNDS_FROM_HEIGHT=999999999` + `ALLOW…OVERRIDE=1`
(`zzz-legacy-unjam.conf`). Re-enable only with **v2.9.48+** (pause window, quorum slash drop
+ round sealed-body fix). Full checklist: [`PHASE2_REENABLE.md`](PHASE2_REENABLE.md).

## Pause window (v2.9.48, testnet 2300)

The 2026-10-05 pause left **legacy-signed certs at heights ≥ 369998** (371037
onward), so re-enabling with the activation height alone stalled on
2026-10-06 (`cert bad sig` on the legacy tip cert). v2.9.48 adds a half-open
window `[vote_rounds_pause_from, vote_rounds_resume_at)` in which heights use
`pl-ok:v1`; round-scoped rules apply on both sides. 2300 bakes
`[371037, 375000)`. Same precedence (env `FALCON_PL_VOTE_ROUNDS_PAUSE` > CLI
`--vote-rounds-pause` > bridge.json > constant) and the same
refuse-unless-`FALCON_PL_ALLOW_VOTE_ROUNDS_OVERRIDE=1` rule. Tests:
`vote_rounds_pause_*`, `vote_rounds_at_respects_pause_window`,
`legacy_tip_cert_after_pause_needs_window_then_resumes_round_scoped`,
`catch_up_across_pause_window_verifies_every_parent_cert`,
`bridge_cfg_pause_window_digest_and_drift`. Procedure, restart steps and how
to bump the resume height: [`PHASE2_REENABLE.md`](PHASE2_REENABLE.md).

## Baked activation / rollback (testnet 2300)

Phase 2 crossed live at **H = 369998** on 2026-10-05 via a systemd drop-in on
every seat (`/etc/systemd/system/falcon-pl-<seat>.service.d/vote-rounds.conf`
→ `Environment=FALCON_PL_VOTE_ROUNDS_FROM_HEIGHT=369998`). From this commit H
is also a network constant (`TESTNET_2300_VOTE_ROUNDS_FROM_HEIGHT`), so a seat
that starts without the env (new host, lost drop-in, hand-run binary) still
verifies round-scoped certs ≥ H correctly.

Resolution (`resolve_vote_rounds_from_height`, logged at start as
`consensus: vote_rounds_from_height=… (source=…)`):

1. `FALCON_PL_VOTE_ROUNDS_FROM_HEIGHT` if set (even `0`)
2. `--vote-rounds-from-height` if non-zero
3. `vote_rounds_from_height` in `keys/lc/bridge.json` if non-zero (optional field)
4. network constant (2300 → 369998; every other network → 0/off)

If the result differs from a non-zero network constant the node **refuses to
start** unless `FALCON_PL_ALLOW_VOTE_ROUNDS_OVERRIDE=1`. Status JSON reports
`vote_rounds_from_height` and `vote_rounds_network_default`.

**Applying it.** The live seats already run with the env, so nothing changes
until each seat runs a binary built from this commit. That is a binary swap
(rolling restart of all seats, one at a time) and needs operator approval. Keep
the `vote-rounds.conf` drop-ins in place throughout and afterwards — they agree
with the constant and protect any seat still on the previous binary.

**Rollback.**
* *Binary rollback* (back to the pre-bake Phase 2 binary): safe as long as the
  drop-ins are still present — the old binary reads the env exactly as before.
  Never remove the drop-ins while any seat might run a pre-bake binary.
* *Disabling round-scoped votes*: no longer a soft rollback. Heights ≥ 369998
  carry round-scoped certs; a seat with activation 0 cannot verify them. Turning
  it off would require a coordinated fork at a new height (all seats, with
  `FALCON_PL_ALLOW_VOTE_ROUNDS_OVERRIDE=1`) and is not supported here.
* Before H was crossed, rollback was "set the env back to 0 (rolling)"; that
  window has closed for 2300.

## Bridge config fingerprint (status `bridge_cfg`)

Status JSON carries `bridge_cfg` for `keys/lc/bridge.json`: `present`,
`sha256` (raw bytes), `qc_cfg_digest` (canonical digest over network_id,
start_height, qc_header_version, qc_start_height, vote_rounds_from_height,
key_root — formatting independent), the file's field values, the values this
process loaded at start (`loaded_qc_*`), the startup fingerprint
(`startup_sha256`, `startup_qc_cfg_digest`), `node_network_id`,
`running_vote_rounds_from_height`, and `drift` + `drift_reasons`:
`file_changed_since_start` (raw bytes), `qc_cfg_digest_changed_since_start`,
`file_qc_fields_differ_from_loaded`, `network_id_differs_from_node`,
`vote_rounds_differs_from_running` (non-zero file value ≠ running height). Any
reason ⇒ restart (or fix the file) to apply. `pl-prod-soak-check.py` fails a seat whose qc_header_version /
qc_start_height / bridge `network_id` (vs the node's) / digest disagree with the
expected 2300 values or that reports drift — this is
how the stale v13 `bridge.json` (2026-10-04) would have been caught.

## Risks / follow-ups

* Single-phase QC: the lock rule is safe against crashes, delays and
  non-equivocating validators. A validator that equivocates (and is slashed
  for it) plus adversarial delays could in principle fake a dead-round proof.
  Full BFT safety across rounds needs a two-phase (prepare/commit) protocol.
* Failover for an offline proposer is a round timeout (≈2–7 s, backed off)
  rather than the 2.5 s skip QC.
* Legacy (pre-activation) dual recovery still discards that attempt's OKs,
  including any genuine double-OK evidence at that height.
* Pending legacy evidence already queued live (v10 @ 369439) will jail v10
  when it lands; it is most likely an honest re-vote (cause 4 above).
* Only in-process tests so far; no multi-host soak yet.

## Unjail / escalating rebond

Jail is no longer permanent. Full bond (and pending unbonding) is seized into
the prize pot on jail; operators clear it with an explicit `Unjail` tx at
`min_bond × 2^jail_count`. See [`UNJAIL.md`](UNJAIL.md).
