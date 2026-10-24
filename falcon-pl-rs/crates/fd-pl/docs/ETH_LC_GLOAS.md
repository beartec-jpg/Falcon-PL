# ETH light client: Sepolia Gloas (v2.9.60)

## What broke
Sepolia activated **Gloas** (fork version `0x90000076`) at epoch **353024**
(slot 11296768, the first slot of sync period **1379**, 2026-10-06 13:53:36 UTC).
The 2300 ETH light client froze at finalized slot **11296672** / period **1378**
(the last Fulu checkpoint). Two separate reasons:

1. `eth_lc.rs` had no `0x90000076` fork version (signature domain).
2. Gloas `LightClientHeader` (EIP-7732) no longer carries `execution`
   (the payload header). It has `execution_block_hash` + `execution_branch`.
   For a Gloas slot the leaf is
   `BeaconBlockBody.signed_execution_payload_bid.message.parent_block_hash`
   (gindex **2856**, depth 11). BeaconState gindices also moved (EIP-7688):
   finalized root 169 → **735**, current committee 86 → **2945**,
   next committee 87 → **2946**. `apply_update` failed with `fin exec`.

Spec: `ethereum/consensus-specs` `specs/gloas/light-client/{sync-protocol,fork}.md`.
Verified against real data from `lodestar-sepolia.chainsafe.io`
(Lodestar v1.49.0). `ethereum-sepolia-beacon-api.publicnode.com` is frozen at
the fork and does not serve Gloas updates.

## What 2.9.60 does (consensus change, height-gated)
`TESTNET_2300_ETH_LC_GLOAS_FROM_HEIGHT` (lib.rs). **Armed at H = 409134 on
testnet 2300** (PR #30: tip 399134 + 10000, read from
`https://falcon-fpl.com/api/status` at 00:54 BST 9 Oct 2026). The private
V22 network stays `u64::MAX` (never). Every seat must be on 2.9.60 before the
tip reaches H, because from H the Hello gate refuses pre-2.9.60 peers.
For txs applied in ledger height ≥ that value:

- `EthLcUpdate` / `EthLcBootstrap` use `eth_lc::apply_update_ex` /
  `apply_bootstrap_ex(.., gloas = true)`:
  - headers verify in either format: original (`execution`, gindex 25) for
    pre-Gloas slots; `execution_block_hash` with a *normalized* branch
    (leading zero padding) at gindex 2856 (Gloas slots) or 812 (Deneb+ headers
    upgraded per `upgrade_lc_header_to_gloas`);
  - finality / next / current committee gindices are chosen by the slot of
    the header whose `state_root` is the proof root (735/2946/2945 for Gloas);
  - the signature fork version is taken at `max(signature_slot, 1) - 1`
    (spec) and includes `0x90000076`;
  - extra spec checks: `signature_slot > attested.slot >= finalized.slot`;
    the next committee is stored only when the attested period equals the
    (possibly rotated) current period.
- A Gloas header proves only the execution **hash**. The LC state then has
  `execution_number = 0`, `receipts_root = ""` until the RLP-verified
  execution header with that hash is stored (an `EthExecHeaders` batch ending
  at the LC hash, or already stored). `BridgeState::eth_lc_resolve_exec_head`
  copies its number and receipts root into the LC state (deterministic, part of
  the state root). Mint (`mint_evm_lc`) still walks stored headers from the
  deposit block up to the LC hash, and from this same height it refuses a
  deposit below the exec-header floor (see [2.9.60 prune](#2960-prune)).

Below the height, apply is byte-for-byte 2.9.59 (Gloas updates fail with
`fin exec`, a non-permanent error, so they stay queued rather than dropped).
On Fulu-format data the Gloas rules produce the identical state (tested).

Status JSON exposes `eth_lc_gloas_from_height` so the soak check can confirm
all seats agree before the tip reaches it (testnet 2300: every seat must show
409134 and version 2.9.60 before the stop line 408634 = H − 500).

## Rollout (coordinated with the activation height)
1. **Done (#30):** `H` is fixed at **409134** on testnet 2300 (tip 399134 +
   10000, ~9.7 h at ~3.5 s/block). Do not pick a new H unless the stop rule
   fires (the tip reaches 408634 = H − 500 before every seat reports H); then
   stop and cut a higher H in a new rollout commit. Build **portable**
   (`RUSTFLAGS` unset, never `target-cpu=native`).
2. **Done 9 Oct 2026 00:07–00:44 UTC (all 7 seats report 409134).** Roll 2.9.60 one seat at a time, ~5–8 min soak each, in this order:
   **v3, v12, v13, v11** (restart only: host2 shares the bin), **v1, v6**,
   then **v10 last**. Before H, compatibility is one-way: a 2.9.60 seat
   accepts any 2.9.x peer in Hello, but a 2.9.59 seat exact-matches Hello and
   rejects 2.9.60 peers, so mixed seats do not all connect and this roll
   order is required (it is not order-independent). Block H
   itself forks any seat still on an older binary (anything before 2.9.60),
   because the prune cut at H is unconditional (it runs at the start of the
   activation block even when that block contains no ETH tx) and that cut is
   in the state root. A
   Gloas LC update in the same block would also fail apply on an old binary.
   Every seat must be on 2.9.60 and report `eth_lc_gloas_from_height == H`
   **before the tip reaches H**.
   After H (local tip ≥ H) a 2.9.60 seat refuses Hello from pre-2.9.60 peers
   (including the legacy empty version) and disconnects them, including a
   session opened before H (closed on its next Pong); 2.9.61+ still connects.
3. Do not let the relayer push Gloas data before H; if it does, the tx just
   waits in the mempool (non-permanent) — harmless, but it holds the relayer
   account's sequence.
4. After H: switch the relayer's beacon endpoint and encoder (below), then
   watch `eth_lc.period` 1378 → 1379 → … and `execution_number` resolve.

Rollback after H with a Gloas update committed requires the same binary
everywhere; a pre-2.9.60 binary cannot replay those blocks.

## 2.9.60 prune

ETH execution headers were never pruned: about 126k headers, ~199 MB of the
~207 MB state, plus ~7.2k headers / ~11 MB per day. Keeping
`number >= lc.execution_number - W` with **W = 50_400** (~7 days at a 12s
slot) brings that down to roughly **80 MB**. The floor is measured from the
LC head, not from wall-clock time, so a relayer outage does not strand a
deposit by pruning under a frozen head.

`BridgeState::eth_exec_prune_floor` (`#[serde(default, skip_serializing_if = "is_zero")]`):

- Armed only when the applying ledger height is `>= eth_lc_gloas_from_height`
  (same gate as Gloas; 409134 on testnet 2300, `u64::MAX` elsewhere). Below
  that the field stays 0 and is omitted, and header bytes match 2.9.59.
- `floor = max(old_floor, execution_number.saturating_sub(50_400))`, and only
  when `execution_number != 0`. A hash-only Gloas head (hash known, number
  not yet copied from a stored header) does not prune and does not lower a
  floor already set.
- Monotonic. A static LC head recomputes the same floor and deletes nothing.
- Headers with `number < floor` are removed. The map, its generation and its
  cached JSON stay put when nothing is removed. A process-local memo of the
  last two `(source generation, floor)` results lets pack, verify and commit
  share one pruned map; the kept headers (`number >= floor`) are the same
  either way. At block H the one-off cut copies the ~200 MB map once per
  distinct (source generation, floor) instead of once per apply path, and
  operators should still watch v10 (slowest seat) memory and latency at
  block H.

There is no single function that pack, verify and commit all call before
txs. Each builds a draft bridge; the cut runs at the start of that draft
(one-off on the activation block, including a block that contains no ETH
tx, then again every later block). Gated `eth_lc_bootstrap_ex`,
`eth_lc_update_ex` and `eth_add_exec_headers_ex` prune again after
`eth_lc_resolve_exec_head`, so a head that moves inside the block rolls the
floor in that block's root. Catch-up and snapshot replay use the commit
path. The floor is consensus state: once non-zero it is in the state root.
Adopting a snapshot loads whatever floor that snapshot committed.

After the floor is set, `eth_add_exec_headers` drops any header with
`number < floor` instead of re-inserting it (RLP and the parent links of
the submitted batch are still checked). A batch that is entirely below the
floor returns `execution headers below prune floor`
(`EXEC_HEADERS_BELOW_PRUNE_FLOOR`) and does not touch the map. That error
is in `is_permanent_tx_failure`, so the packer drops the tx from the mempool
(the floor only rises, so it can never succeed). Only a genuinely empty
submission returns the retryable `no headers`.
`connects_down` at exactly the floor is false, because parent `floor - 1`
was pruned: a kept suffix whose first header is the floor has to end at
the LC hash or connect upward. Extending the stored tip (`eth_exec_tip + 1`)
still connects downward, which is the relayer's normal batch.

`mint_evm_lc`: if `eth_exec_prune_floor > 0` and `proof.height < eth_exec_prune_floor`,
the error is `deposit older than header window`, before the stored-header
walk (not `no execution header` / `missing execution header`). It is a
permanent mempool failure.

While the Gloas head is hash-only (`execution_number == 0`, receipts root
empty), a mint whose stored headers do not already end on that hash fails
with `execution headers do not reach LC head`: the walk stops as soon as the
next number is above 0. `spent_deposits.insert` runs only after the mint
succeeds, so the deposit is not consumed and can be resubmitted once the
head resolves. The relayer should send the LC-head execution header (an
`EthExecHeaders` batch ending at the LC hash) in the same block as the Gloas
update so the head resolves before any mint.

Status JSON:

- Top-level `eth_exec_prune_floor` — the prune floor in state (0 until it arms).
- Top-level `eth_exec_window` — `50400`. Also inside `eth_lc`, next to
  `eth_exec_tip`.
- `eth_lc.eth_exec_floor` is the low end of the contiguous connected stored
  range (`eth_exec_bounds()`), not the prune floor. `eth_lc.eth_exec_tip` is
  the high end of that same range. After a prune, while the connected range
  is non-empty, the connected low is `>= eth_exec_prune_floor`. If the prune
  removes every stored header, `eth_exec_bounds()` reports `(0, 0)` while
  `eth_exec_prune_floor` stays non-zero.

## Relayer changes needed (NOT deployed; not part of this PR)
`crates/fd-pl/scripts/pl-eth-lc-relayer.py` (deployed copy differs from the
repo — patch the deployed one):

1. **Beacon endpoint:** set `SEPOLIA_BEACON` (or `BEACON_RPC` /
   `beacon_rpc` in the bridge config) to an endpoint that serves Gloas LC
   updates, e.g. `https://lodestar-sepolia.chainsafe.io`. publicnode is frozen
   at period 1378.
2. **Period catch-up order:** the LC is at period 1378 with the 1379 committee
   known; first submit `updates?start_period=1379&count=1` (Gloas, signed by
   the 1379 committee), then 1380, …, then `finality_update`. The existing
   `sig_period > period + 1` branch already does this once the endpoint serves
   Gloas data.
3. **Execution header RLP (EL Amsterdam):** post-fork Sepolia headers carry
   two more fields after `requestsHash`: `blockAccessListHash` (bytes32) and
   `slotNumber` (uint). `encode_exec_header` must append
   `rlp_bytes(hx(block["blockAccessListHash"]))` and
   `rlp_int(uhex(block["slotNumber"]))` when present, otherwise
   `header_obj` raises `header rlp hash mismatch` for every post-fork block
   (verified on blocks 11856686 and 11864394). The node side already accepts
   extra trailing header fields.
4. **Hash-only LC head:** after a Gloas update `eth_lc.execution_number` is 0
   until headers reach the LC hash. Resolve the number with
   `eth_getBlockByHash(eth_lc.execution_block_hash)` and submit
   `EthExecHeaders` from the stored `eth_exec_tip + 1` up to that block (the
   batch must end at the LC hash or connect to the stored chain). Do not treat
   `execution_number == 0` as "LC at block 0".
5. If the ETH relayer still shares an account with the BTC header submitter,
   a queued Gloas update before H holds that account's sequence. The
   concurrent `fix/btc-submitter-polling` work moves it to its own account.
6. V3 mint-or-refund deposits (2.9.61) use a separate height gate. Rules, the
   3-day expiry, the V1 cutoff, and rollout are in
   [ETH_V3_DEPOSITS.md](ETH_V3_DEPOSITS.md).
