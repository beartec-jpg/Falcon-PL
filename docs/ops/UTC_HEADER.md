# UTC signed header (v5) — design notes & unfinished gates

**Status:** implementation started; **activation disabled** (`TESTNET_2300_UTC_FROM_HEIGHT = u64::MAX`).
**Not production-ready.** Do not merge as “UTC live”. Do not set a near-term `H_utc` from this document alone.

## What this change does

- Adds `Ledger.sealed_utc_ms` / `LedgerHeader.sealed_utc_ms` (serde default 0, omitted when zero).
- Defines `pl-ledger:v5|…|utc:{sealed_utc_ms}` in `utc_header::header_msg_v5`.
- When activation is **unarmed**, pack/verify continue to use **v4** preimages; field stays 0; mainnet/testnet behaviour matches pre-change binaries for sealed hashes.
- When activation is **armed** (`height >= H_utc`, `H_utc != u64::MAX`):
  - packer signs v5; `inner_hash` / `ledger_hash` / OK votes over `ledger_hash` cover UTC;
  - `seed_v2` uses the same domain-selected message;
  - live staging applies PROPOSED skew vs OS wall; commit/catch-up apply **historical-only** rules (no wall rejection on replay).
- Preserves `sealed_at_ms` as synthetic chain physics / reward time unchanged.
- Status exposes `tip_sealed_at_ms` + quality `chain_physics`, and `tip_sealed_utc_ms` only when non-zero.

## Activation (must stay unset on live)

| Knob | Live 2300 | Devnet / tests |
|------|-----------|----------------|
| `TESTNET_2300_UTC_FROM_HEIGHT` | `u64::MAX` (disabled) | n/a |
| `FPL_UTC_FROM_HEIGHT` | **ignored** on 2300 | honoured on other network ids |
| `FPL_UTC_MAX_AHEAD_MS` / `FPL_UTC_MAX_BEHIND_MS` | ignored on 2300 | optional overrides |

Arming requires a **separate** commit + preactivation signoff after seat readiness, Hello gate, and soak — not this PR.

## PROPOSED skew / bootstrap (rationale — not measured safe values)

| Rule | PROPOSED | Rationale sketch |
|------|----------|------------------|
| Max ahead of checker wall | 5_000 ms | Mirror existing `SEALED_AT_MAX_AHEAD_MS` order; refuse future-dated tips |
| Max behind checker wall (live only) | 60_000 ms | Tolerate short stalls / NTP wobble; long stall → operational recovery |
| Parent monotonicity | child ≥ parent when parent UTC > 0 | Prevent rewind of committed wall timeline |
| First block at `H_utc` | parent UTC may be 0; absolute skew only | Bootstrap without inventing parent UTC |
| Historical replay | no wall skew | Deterministic catch-up / archive restore |
| Stalled chain | no auto-widen on 2300 | Widening needs coordinated signoff; replay still works |

**Flagged for preactivation signoff:** exact ahead/behind numbers, peer Hello min-release window, whether monotonicity allows equal timestamps, monitoring alerts for tip UTC freshness.

## Test coverage in this PR (`utc_header` unit tests)

- Disabled-by-default on 2300; activation boundary
- v5 extends v4; mutated UTC changes commitment / inner hash
- Historical inactive rejects non-zero; active requires UTC + monotonic / first-anchor bootstrap
- Live skew boundaries; historical accepts ancient stamps wall would reject
- HMAC signature covers UTC field
- Stalled-chain: live BehindWall vs historical OK (operational recovery, not auto-widen)

## Unfinished gates (exact)

1. **Hello / product_version peer gate** — refuse pack/OK with peers below required release after `H_utc − safety window` (window undecided).
2. **Injectable pack wall clock** — `seal_v4_ledger` uses `wall_unix_ms()` when armed; tests need a controllable clock for deterministic pack under V5.
3. **QC attestation path audit** — OK votes cover `ledger_hash` (hence UTC once v5), but explicit dual-attest / slash evidence docs need a pass for v5 wording.
4. **Gossip / snapshot / disk round-trip soak** under armed devnet (BodyStore already stores full ledger JSON; needs armed rehearsal).
5. **Explorer 1970 fix (P0)** — Falcon-faucet-wallet must stop `Date(chain_ms)`; separate PR.
6. **Permanent fill IDs + cursor API (P2)** — beyond `RECENT_FILLS=200`; archive capacity not inspected.
7. **Public height+hash→UTC API (P3)** — only after `H_utc` live.
8. **Crypto candles (P4)** — do **not** merge #618 from this Go; needs `utc_committed` source.
9. **Devnet fork rehearsal** — old binary left behind; document observed fork.
10. **Release tag / version bump** — not chosen in this PR.

## Ready / not-ready

| Claim | Status |
|-------|--------|
| Field exists on Ledger wire | yes (default 0) |
| v5 domain + test harness | yes |
| Live activation on 2300 | **no** (`u64::MAX`) |
| Production-safe skew | **no** (PROPOSED only) |
| Market API / candles ready | **no** |
| Safe to merge as draft for review | intended |
| Safe to deploy / arm | **no** |


## Milestone t281u (capability + rehearsal harness)

Added (still draft / unarmed on 2300):

- Hello `capabilities` (advertises `utc-header-v5`); hard close only when
  `Node.utc_hello_enforce_from_height` armed (default `u64::MAX`).
- Thread-local wall + `from_height` overrides for deterministic rehearsal
  (no process-global env races).
- `utc_rehearsal_tests` — **in-process simulation harness** (not multi-node soak):
  pre/post activation, forged utc, serde restart, live vs historical skew,
  soft capability gap vs hard enforce, seed_v2 binds utc, production defaults unarmed.

Still unfinished: real multi-node isolated process soak, injectable Node pack
clock beyond thread override, QC wording pass, P0 explorer, P2 fill IDs, H_utc.


## Milestone t284u — multi-node rehearsal

- `docs/ops/tools/utc_multinode_rehearsal.sh`: real multi-process loopback nodes on
  `127.0.0.1:29101+`, network_id=2200, hmac-dev, scratch `/tmp/falcon-utc-mn-*`.
  Status UTC metadata + Hello `utc-header-v5` + old-peer Hello vs private-net
  `FPL_UTC_HELLO_ENFORCE_FROM_HEIGHT` (2300 still ignores env).
- In-process `rehearsal_multinode_network_utc_qc_agreement`: 6-seat Network seals
  across H_utc with agreed tip hash + sealed_utc_ms + QC.
- Production H_utc / Hello enforce on 2300 remain `u64::MAX`.
