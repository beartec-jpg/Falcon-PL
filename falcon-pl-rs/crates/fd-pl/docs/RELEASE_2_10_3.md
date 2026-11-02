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

## Chunked ("piece") transport for large lines (incident 2 Nov 2026, crypto-dashboard @ 779,474)

- Any wire line > 64 KB (LedgerBody/LedgersBatch, TxsBatch replies to NeedTxs /
  NeedMempoolTxs, tx dish, inbound-seat sends) to a peer whose Hello said
  `release_version >= 2.10.3` is sent as `{"type":"piece","pid","i","n","d"}` lines
  (24,000 raw bytes each, ≤ 48 KB serialized), in order, on the same queue, all-or-nothing.
  Pieced sends skip the "one large in flight" gate (no single line > 64 KB is written),
  so the consensus lane can interleave between pieces.
- 2.10.2 peers (no `release_version`) always get the legacy single line. If the queue
  lacks room for the whole run, the legacy line is used.
- Receiver (`fd_pl::pieces::PieceAssembler`, per connection): in-order only, ≤ 2048
  pieces, ≤ 48 MB joined, ≤ 8 open runs, 30 s timeout; a broken run is dropped and the
  normal retry/catch-up path re-requests. The joined line is handled exactly like a
  line received whole (Hello gate and dispatch unchanged).
- Piece lines are must-send in the writer (never dropped mid-run).
- Transport only: no consensus/hash/state change.
- Tests: `pieces::tests::*` (7), node bin `pieces_only_to_2_10_3_peers`, `mixed_version_queueing`.
