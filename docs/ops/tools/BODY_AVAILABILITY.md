# Body availability: archive vs non-archive (honest)

## Facts (code)
- `Ledger::has_body()` ⇔ `txs.len() == tx_ids.len()` (stripped when txs cleared).
- Non-archive nodes keep at most `max_body_ledgers` (default **64**) full bodies in RAM; older heights are stripped.
- `--archive` persists bodies under `<data>/bodies/` and can serve beyond the RAM window.
- `ledgers_from(from, max)` serves a **contiguous** body prefix; stops at the first missing body (never skips holes).
- Tip-0 catch-up prefers NeedSnapshot when a usable archive/hub snapshot is advertised; otherwise residual **NeedLedgers**.
- `catchup_peer_usable` requires advertised tip **>** local tip.

## What this means for fresh tip-0 NeedLedgers
- Short tips (tip < max_body_ledgers) on non-archive peers **should** still have bodies for h=1..tip — claimable only after `peer_body_inventory` (SnapshotSave) proves `has_body` per height.
- Once tip exceeds the RAM window without `--archive`, h=1 is stripped → `ledgers_from(1)` returns **empty** → NeedLedgers cannot advance a tip-0 joiner. That is not a bug in the pull wire; it is missing bodies. Use snapshot (hub/archive) or archive peers.
- Do **not** pretend bodies exist because CI is green or because NeedLedgers pulls were logged. Prove inventory, then prove tip advance / catchup_batch.

## Harness (`utc_fresh_catchup_rehearsal.sh`)
- Mode A (default): non-archive mesh with one `--hub`; records `peer_body_inventory.json` before joiner.
- Optional archive Mode B: peer with `--archive` (body store) — only when explicitly requested.
- Artifacts under `$ART`: RESULT, BODY_HOLE (if any), logs. Failure with NeedLedgers pull but tip stuck is an **unmet** catch-up proof, not release-ready.

## Process harness root cause (t287u evidence)
Prior Mode A FAIL (`/tmp/falcon-utc-fresh-artifacts` on brain): joiner logged `catch-up pull from_height=1` with `best_peer=2` but never `catch-up batch …→…`. Mesh logs: **`refuse inbound from v7: we dial them`** every ~500ms. Dial rule is lower seat id dials higher; mesh had no v7 in `--peers`, so nobody dialed v7 and inbound from v7 was refused. Tip was learned from the brief Hello before refuse — not a durable catch-up session. Fix: reserve v7 in all peer lists from start (bidirectional dial when joiner listens).
