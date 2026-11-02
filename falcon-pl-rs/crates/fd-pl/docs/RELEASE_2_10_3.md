# Falcon PL 2.10.3 (local prep, not released)

Follow-up to the 1 Nov 2026 incident (crypto-dashboard stuck at 775,000,
up to 132 blocks behind, ~15 min). No consensus change; mixed 2.10.2 / 2.10.3
fleet is safe.

## Changes
- **Large-send gate (local, active):** the per-peer "large send in flight"
  flag is timestamped and treated as stale after 10 s, then retaken (logged
  `large send gate to <id> stale after N ms, cleared`). Before, a send whose
  completion was never seen paused every later large dish to that peer
  ("one already in flight").
- **Pre-dish chunking (local, active):** the pack-time TxsBatch dish is split
  into <=48 KB pieces (same splitter as the suffix dish), sent in order before
  the id body and header. Receivers already absorb TxsBatch incrementally.
- **Hello `release_version` (sent, enforcement OFF):** Hello now carries the
  real release (`2.10.3`), because `product_version` is still the legacy
  `2.9.100` wire string. Omitted when empty; 2.10.2 ignores the unknown field.
  Enforcement is gated by `TESTNET_2300_HELLO_MIN_RELEASE_HEIGHT = u64::MAX`
  (never) with `HELLO_MIN_RELEASE = "2.10.3"` reserved for a future release.
  Hello-gate closes are now logged with the reason.

## Not in this release
- Lagging-proposer skip (consensus). Kept on `fix/lagging-seat-resilience`
  for rework (v4 sealed next-committee path, snapshot-window determinism).

## Compatibility
- Lottery, seeds, ledger/cert hashing, activation heights: unchanged
  (BTC4 fork fix still 775,000).
- Wire: one optional Hello field; no other message changes.

## Rollout
One seat at a time, wait for it to return to tip and pack/vote before the
next; v10 last; nothing on falcon2 without explicit owner OK.

## Peer release observability (read-only)

- Each accepted-or-checked Hello records `peer_releases[id] = (release_version, now_ms)`
  in runtime memory (not snapshotted, not hashed, no consensus effect, no enforcement).
- `falcon.health.v1` → `peers.seats[]` gains `account`, `release_version`
  (`"none (pre-2.10.3)"` when the Hello lacked the field, `null` if no Hello seen yet),
  `hello_age_ms`; new `peers.releases[]` lists every Hello'd peer with `id`, `account`,
  `release_version`, `last_seen_ms`, `hello_age_ms`.
- `falcon-pl-ctl` dash and the health dash peers panel show the release column.
- Tests: `health_peer_release_versions`, `peer_release_is_observability_only`.
