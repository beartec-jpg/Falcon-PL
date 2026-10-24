# Disk hygiene (v2.9.53)

## Incident (2026-10-07)

host2 runs v3 (hub) and v11. Its root disk hit 100% (1.7 MB free of 232 GB).
The hub's disaster-recovery join serve wrote a new ~200 MB
`/tmp/pl-hub-join-v3-<tip>.json` on **every** `NeedSnapshot`, one per height
while a seat kept asking, and never deleted any of them: ~1,000 files and
173 GB, going back to Sep 8. v11 was the proposer at 374385, could not write
its snapshot (`write snapshot tmp`), and the chain stalled at 374384 from
02:17 UTC.

Side note: on mhr and hil `/tmp` is a 2–4 GB **tmpfs** (RAM). A hub there
would have filled RAM instead of disk.

## What the node does now

| Area | Before | v2.9.53 |
|------|--------|---------|
| Hub join serve | new `/tmp/pl-hub-join-<id>-<tip>.json` per request, never deleted | 1) reuse the periodic pinned `<snapshot-out>.join.serve.json` if < 32 heights old (or a joiner is mid-transfer and it is < 256 old); 2) reuse the current hub dump on the same rule; 3) otherwise overwrite **one** file `<data-dir>/<id>.hub-join.json` (tmp+rename). Deleted after 10 min with no chunk served. |
| Legacy dumps | — | At start: remove `$TMPDIR/pl-hub-join-<own id>-*.json[.tmp]` (exact seat-id match, so `v1` never removes `v11`'s), `$TMPDIR/pl-snap-pull-*.json` older than 10 min, the seat's stale `<id>.hub-join.json`, and leftover `<snapshot>.json.tmp` / `<snapshot>.join.json.tmp`. |
| Joiner pull file | `/tmp/pl-snap-pull-<tip>.json` (if no `--snapshot-out`) | `<data-dir>/<id>.pull.json` (one per seat) |
| Partial writes | failed write left `.json.tmp` (0 B or partial) | tmp removed on any write/rename error |
| Low-disk guard | none | see thresholds below |
| Status | — | `status.disk` = `{path, avail_bytes, total_bytes, free_pct, level, skipped_dumps, last_skip, hub_snap}` |
| Snapshot `.bak` | grew forever (`v3.json.<ts>.bak`, `v3.json.bak-<ts>`) | at start keep newest 3 per seat (`FALCON_PL_SNAPSHOT_BAK_KEEP`, `0` = never prune) |

`<data-dir>` is the parent of `--snapshot-out` (live: `<root>/snapshots`), else
the parent of `--snapshot`, else `<keys-dir>/../snapshots`.

### Low-disk thresholds (env, read at start)

| Env | Default | Meaning |
|-----|---------|---------|
| `FALCON_PL_DISK_WARN_PCT` | 10 | below: `WARN DISK WARN …` every 5 min |
| `FALCON_PL_DISK_MIN_FREE_PCT` | 5 | low-water mark (percent) |
| `FALCON_PL_DISK_MIN_FREE_MB` | 2048 | low-water mark (absolute); low = below **either** |
| `FALCON_PL_DISK_HARD_RESERVE_MB` | 256 | essential writes must leave at least this |

- **Optional dumps** (hub join serve, archive serve refresh, periodic
  `.join.json` and the `.join.serve.json` pin copy): skipped if free space is
  under the low-water mark, or would drop under it after the write. The
  skip is logged (`DISK: skipped …`) and counted in `status.disk.skipped_dumps`.
  A hub that can't dump answers `SnapshotNo{reason:"low disk: …"}`, so the
  joiner tries another seat.
- **Essential writes** (the seat's own `--snapshot-out`, a downloaded
  snapshot pull): only refused if they would leave < 256 MB. They are
  refused **before** anything is written, so a near-full disk is never
  pushed to 100%.
- Free space below the low-water mark logs an `ERROR DISK LOW …` every 60 s
  (and once at start).

Nothing here changes consensus, ledger hashes, votes or the wire format.

## Soak check

`crates/fd-pl/scripts/pl-prod-soak-check.py` now reports free disk per host on
every pass. It runs `df -P -B1 ~/falcon-pl-public-testnet-2300 /tmp`
(read-only) over the same `PL_SOAK_SSH_CMD` hop for `PL_SOAK_DISK_HOSTS`
(default: host1, host2, mhr, hel1-6, hil, fsn1), and also reads
`status.disk` from seats on 2.9.53 or later.

- free < `PL_SOAK_DISK_CRIT_PCT` (5%): **CRITICAL**
- free < `PL_SOAK_DISK_WARN_PCT` (10%): **WARN**
- `--disk-hosts off` skips the SSH `df`.

## Manual cleanup helper

`crates/fd-pl/scripts/pl-disk-prune.sh [--apply] [--keep N] [DATA_DIR]`
is a **dry run** unless you pass `--apply`. It lists legacy
`/tmp/pl-hub-join-*` (keeping the newest per seat, which a pre-2.9.53 hub
may still be serving), old `/tmp/pl-snap-pull-*`, snapshot `.bak` beyond
the newest N, and stale `snapshots/*.json.tmp` older than 10 min. It never
touches live snapshots, keys, bodies, logs or `backups/`.

## Log rotation

Example: `deploy/logrotate/falcon-pl-seats`. Seats log through systemd
`StandardOutput=append:<root>/logs/vN.log`, which has no reopen signal, so it
uses `copytruncate` (daily, `maxsize 200M`, keep 7, compressed). Install with
`sudo install -m 0644 deploy/logrotate/falcon-pl-seats /etc/logrotate.d/` and
test with `logrotate -d` first.
