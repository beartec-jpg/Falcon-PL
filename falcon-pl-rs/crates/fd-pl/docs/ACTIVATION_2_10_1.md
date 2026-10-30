# Falcon PL 2.10.1: BTC4 header fork fix (activation block 797,000)

Gate: `params_v2::TESTNET_2300_BTC4_FORK_FIX_HEIGHT = 797_000` (2300); other networks
`FPL_BTC4_FORK_FIX_FROM_HEIGHT` (default never). Active when `tip + 1 >= 797,000`.

Before the height 2.10.1 applies BTC4 `RailHeader` txs byte-for-byte like 2.10.0
(`submit_header_ex2(.., fork_fix=false)` == `submit_header_ex`), and the new
`RailState.btc_fork_alt` stays empty and unserialized, so state roots match 2.10.0.

From the height (BTC4 rail only, strict/rv2 path):
- A header forking off the stored main chain that is lighter than the pending fork
  no longer fails with "less work than the pending fork": it starts a competing
  branch (`btc_fork_alt`) that accumulates from the common ancestor.
- Once the competing branch's cumulative work is strictly greater than the pending
  fork's, it replaces it, then the normal rule (strictly more work than the main
  suffix) switches the rail onto it.
- A pending fork/competing branch is dropped when the main tip is more than
  `BTC_FORK_STALE_N` (6) headers past its tip with strictly more work.
- Re-submitting a header already in the competing branch is a duplicate error:
  submitters must continue with the next height.

Status (non-consensus): `status_req` rails gain `btc_fork` / `btc_fork_alt`
(`len, start_height, tip_height, tip_hash, work`, or null).

Hello: unchanged (`HELLO_WIRE_VERSION` 2.9.100), accepted by 2.9.x and 2.10.0 peers.
Every seat must run 2.10.1 before block 797,000; a 2.10.0 seat diverges after it
only if a BTC4 competing-branch header is applied.
