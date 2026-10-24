# Phase 2 re-enable (testnet 2300)

British English. Operator procedure after the 2026-10-05 soft-QC / slash
freeze and the failed 2026-10-06 re-enable. Supersedes the v2.9.47 version of
this file, which was **wrong** (see "2026-10-06: why re-enabling at 369998
stalled").

## History in one table

| Cert heights | Signed as | Why |
|---|---|---|
| ≤ 369 997 | legacy `pl-ok:v1` | before Phase 2 |
| 369 998 – 371 036 | round-scoped `pl-ok:v2` | Phase 2 live from H=369998 |
| 371 037 – tip | legacy `pl-ok:v1` | paused 2026-10-05 ~14:10 UTC by `zzz-legacy-unjam.conf` (`FALCON_PL_VOTE_ROUNDS_FROM_HEIGHT=999999999`) |

Pinned on 2026-10-06 by checking every parent cert in the archive (v1)
snapshot against both payloads: certs for 369998..371036 verify **only** as
v2, certs from 371037 on verify **only** as v1. No mixed certs.

## 2026-10-06: why re-enabling at 369998 stalled

v2.9.47 was rolled out with a coordinated restart and the pause drop-in
removed, so every seat ran `vote_rounds_from_height=369998`. Tip was 371477,
whose cert was legacy-signed during the pause. `verify_parent_cert` checks a
cert with `vote_rounds_at(cert.height)`, which was true for every height ≥
369998, so every pack failed self-accept with `cert bad sig for v12` and no
seat could extend the tip. Rounds climbed 2→10. Rolled back to the pause
drop-in (legacy) within 4 minutes; chain moved again on v2.9.47.

No single activation height can describe the table above. Hence v2.9.48.

## v2.9.48: the pause window

`ChainConfig::{vote_rounds_pause_from, vote_rounds_resume_at}` define a
half-open window `[from, resume_at)` inside round-scoped history. Heights in it
use the legacy payload; round-scoped rules apply on both sides:

```
vote_rounds_at(h) = vote_rounds_from_height > 0
                    && h >= vote_rounds_from_height
                    && !(from <= h < resume_at)
```

Baked for 2300 (`TESTNET_2300_VOTE_ROUNDS_PAUSE_FROM` / `_RESUME_AT`):

* **from = 371 037** — first legacy-signed cert (pinned, see above)
* **resume_at = 375 000** — Phase 2 is round-scoped again from here

375 000 was ~3 460 blocks above the tip when chosen (371 538 on 2026-10-06
09:20 UTC): about one day at the 2026-10-05 healthy rate (~160 blocks/h,
~3 900/day), i.e. room for review, merge and a fleet restart. The chain only
makes blocks under load, so in practice it is usually more.

Resolution mirrors the activation height (logged at start as
`consensus: … pause=[from, resume) (source=…)`):

1. env `FALCON_PL_VOTE_ROUNDS_PAUSE=FROM..RESUME` (or `off`) if set
2. `--vote-rounds-pause FROM..RESUME` (or `off`) if given
3. `vote_rounds_pause_from` + `vote_rounds_resume_at` in `keys/lc/bridge.json` if set
4. network constant (2300 → `371037..375000`; other networks → none)

A result that differs from the baked window **refuses to start** unless
`FALCON_PL_ALLOW_VOTE_ROUNDS_OVERRIDE=1` (same flag and rule as the activation
height; `check_vote_rounds_startup`). Malformed windows are always refused.

With the pause drop-in still present (`999999999` + override) the window is
inert: the seat starts (override warning) and stays legacy for every height.

Status JSON: `vote_rounds_pause_from`, `vote_rounds_resume_at`,
`vote_rounds_pause_network_default`, `vote_rounds_paused_now`, plus
`round_scoped_votes` (false inside the window, true from resume). `bridge_cfg`
flags `vote_rounds_pause_differs_from_running` if bridge.json carries a
different window. bridge.json files without a window keep their exact
pre-v2.9.48 `qc_cfg_digest`.

## Restart steps (after the PR is merged) — needs Scott's OK

Inside the window a v2.9.48 seat without the drop-in and a v2.9.47 seat with
the drop-in behave identically (legacy), so the upgrade is safe as a rolling
restart **as long as every seat is done before the tip reaches 375 000**. If
the chain is currently stuck on a legacy dual proposal, use the coordinated
form below, which also clears that.

1. **Build** from the live tree, as for hotfix2 / v2.9.47: `deploy/pr8-on-live`
   (= `deploy/pr7-on-live` + PR #8) plus this PR cherry-picked. Confirm
   `product_version` 2.9.48 and `consensus_rounds` tests pass. Record sha256.
2. **Back up** each seat (data dir snapshot, current binary, drop-ins) with a
   timestamp, as in `backups/pre-2.9.47-*`.
3. **Stage** the binary as `bin/falcon-pl-node.2.9.48` on every seat (+ ctl on
   host1).
4. **Coordinated restart** (or rolling, one seat at a time, if the chain is
   healthy):
   ```bash
   # every seat, all at once: stop
   sudo systemctl stop falcon-pl-vN          # v1 v3 v6 v10 v11 v12 v13; verify all inactive
   # every seat: swap binary, drop the pause override, keep vote-rounds.conf
   mv bin/falcon-pl-node.2.9.48 bin/falcon-pl-node
   sudo rm -f /etc/systemd/system/falcon-pl-vN.service.d/zzz-legacy-unjam.conf
   sudo systemctl daemon-reload
   # every seat: start
   sudo systemctl start falcon-pl-vN
   ```
5. **Verify** on every seat:
   ```text
   product_version         = 2.9.48
   vote_rounds_from_height = 369998
   vote_rounds_pause_from  = 371037
   vote_rounds_resume_at   = 375000
   vote_rounds_paused_now  = true
   round_scoped_votes      = false   # until tip+1 >= 375000
   ```
   Journal: `consensus: vote_rounds_from_height=369998 (source=env, …) pause=[371037, 375000) (source=network-constant)` and **no** OVERRIDE warning.
   `pl-prod-soak-check.py --once --ssh-via falcon@host1` shows no
   `vote_rounds_from_height=999999999` CRITICALs and an
   `INFO Phase 2 paused by window 371037..375000: resumes in N blocks` line.
6. **Watch the resume edge** (h=375000, automatic, no restart): ledger 375000
   carries legacy cert 374999; certs from 375000 are round-scoped
   (`round_scoped_votes=true`). Expect normal view-change behaviour, no
   `cert bad sig`, no false jails, and no `v4 pack needs 6 bonded` spam.
   Occasional `drop slash that would leave <6 bonded` is expected while the
   committee is exactly 6.

## Bumping the resume height

Do this if the tip gets within ~1 000 blocks of 375 000 (the soak check goes
CRITICAL for any seat that is not ready) and the fleet is not yet fully on
v2.9.48 without the pause drop-in.

* **Not yet deployed:** change `TESTNET_2300_VOTE_ROUNDS_RESUME_AT` in a small
  PR (tests pin `from`, not `resume_at`, apart from "above the tip"), rebuild,
  deploy.
* **Already deploying / deployed:** on **every** seat add
  `/etc/systemd/system/falcon-pl-vN.service.d/zz-vote-rounds-pause.conf`:
  ```
  [Service]
  Environment=FALCON_PL_VOTE_ROUNDS_PAUSE=371037..<NEW>
  Environment=FALCON_PL_ALLOW_VOTE_ROUNDS_OVERRIDE=1
  ```
  `daemon-reload` and restart every seat (rolling is fine while the tip is
  below the old resume height). Then bake `<NEW>` in a follow-up PR and remove
  the drop-in at the next binary roll. Update the soak check expectation
  (`PL_SOAK_EXPECT_VOTE_ROUNDS_PAUSE` / `--expect-vote-rounds-pause`).

All seats must agree on the window before the tip reaches the **smaller** of
the old and new resume heights. A seat that flips to round-scoped at a height
where the others stay legacy cannot verify their certs (and vice versa), which
is exactly the 2026-10-06 stall.

## Rollback

* **Before 375 000:** restore `zzz-legacy-unjam.conf` on every seat
  (`FALCON_PL_ALLOW_VOTE_ROUNDS_OVERRIDE=1`,
  `FALCON_PL_VOTE_ROUNDS_FROM_HEIGHT=999999999`) and restart. The window is
  inert; behaviour is the current legacy mode. Reverting to the v2.9.47 binary
  is also safe with that drop-in.
* **After 375 000:** do **not** use the `999999999` drop-in: certs from 375000
  are round-scoped and a legacy seat cannot verify them (the same trap in
  reverse). Fix forward, or add a second window in code first.

## Do not

- Do not set an activation height or window that is already history unless it
  matches how those certs were actually signed (check with the cert scan).
- Do not mix seats with and without the window across the resume height.
- Do not re-enable Phase 2 without the quorum-slash drop (v2.9.47+).
- Do not unjail into a committee of exactly 6 if pending slash evidence still
  targets a live seat.
