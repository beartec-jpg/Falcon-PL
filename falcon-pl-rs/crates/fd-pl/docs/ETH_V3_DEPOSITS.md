# ETH V3 deposits (2.9.61) — mint or refund

V3 peg-ins are gated at `TESTNET_2300_ETH_V3_FROM_HEIGHT = 414_000` (active on Testnet 2300 since 9 Oct 2026, following the Gloas light-client activation at `409,134`; see [ETH_LC_GLOAS.md](ETH_LC_GLOAS.md)):

- `TESTNET_2300_ETH_V3_FROM_HEIGHT = 414_000` on network id 2300.
- `DEFAULT_SEPOLIA_QC_V3_BRIDGE = Some("0xed4ee497f21a9255a59e87535ff2a6097592cfd4")` (`FalconQcBridgeV3` at `0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4`, Groth16 verifier `0xA03005921E1807587B942ab00cC4212650670aA6`). A seat's runtime
  `eth_v3_bridge` comes from `FPL_SEPOLIA_QC_V3_BRIDGE` or that default.
  Neither value is snapshotted. Consensus functions take the address as an
  argument and do not read the environment. Unset or blank env means the
  default. A set but malformed value (not a 20-byte `0x` address, or not
  UTF-8) stops the seat at start-up: `Node::new` panics and
  `Node::load_snapshot` returns an error, both naming
  `FPL_SEPOLIA_QC_V3_BRIDGE`. It is never read as "no V3 bridge", which
  would leave that seat stalling on the first `EthV3Deposit` after H2.
- The V3 address is **not** part of `sepolia_mint_bridge_addrs()`. V1 mint
  never treats a V3 log as a V1 deposit.

`TxBody::EthV3Deposit` is permissionless (any funded account, same allowlist
as `RailDeposit`). It applies at ledger height `tip + 1`.

## When a deposit resolves

`E` = `ETH_V3_DEPOSIT_EXPIRY_SECS` = 3 days. `W` = `ETH_EXEC_HEADER_WINDOW` =
50_400 execution blocks (about 7 days). A deposit older than W cannot be
submitted: the receipt proof fails with `deposit older than header window`
(permanent), same as a V1 mint below the prune floor.

LC-head time is the unix timestamp at execution-header RLP field 11 of the
stored header whose hash is `eth_lc.execution_block_hash` (the ancestry walk
already ends on that header). Post-London, Amsterdam, and Gloas leave field
11 as the timestamp. If that header cannot be parsed, the fallback is Sepolia
beacon genesis `1655733600 + 12 * eth_lc.finalized_slot`.

`expired` means `lc_head_time > log.timestamp + E`. Equal is still inside E.

| LC head | `credit_to` | result |
| --- | --- | --- |
| inside E | non-empty, `dest_id20(credit_to) == log.dest20`, and the name is creditable | **mint** |
| inside E | non-empty, hash matches, name is **not** creditable ("invalid dest") | **refund** |
| inside E | empty, or hash does not match `dest20` | error `V3 deposit dest unresolved: supply credit_to before expiry`. Not permanent. No state change. |
| after E, still inside W | anything, including empty or wrong | **refund** to `sender` |

Creditable means `NameRegistry::validate_name` accepts it, or it is a reserved
id on the fixed list `bridge::V3_CREDITABLE_RESERVED`: the genesis accounts
(`alice`, `bob`, `carol`, `dave`, `faucet`), which `validate_name` rejects but
V1 already credits, plus `treasury` and `watcher-browser` (the reserved names
auto-mint resolves). Other reserved ids (`admin`, `pool`, `bridge`,
`operator`, `v3`, …) refund, even if a `Pay` has already given them a ledger
entry. The stored mint name is the lowercase form (`Bob` →
`bob`). A short name such as `no` hashes to a `dest20` but is not creditable,
so it refunds inside E. Nobody can force that refund early by submitting an
empty or mismatched `credit_to`: that error does not record the deposit.

A creditable name whose current balance of that asset cannot take the amount
without passing `u64::MAX` also refunds (balances are `u64`; in wei that is
about 18.4 ETH per account). The balance is never wrapped and nothing is
recorded as minted.

A `deposit_id` that is not 32-byte hex fails with
`V3 deposit_id must be 32-byte hex` (permanent).

A deposit id resolves at most once (`V3 deposit already resolved`, permanent).
Mint then refund of the same id is impossible, and so is the reverse. That
check runs before the receipt proof, and mempool admission also refuses an
already-resolved id (a map lookup), so a public replay costs nothing and the
submitter gets the error back instead of a tx that silently drops at pack.

Before activation, mempool admission and relay refuse the tx outright
(#34). While `tip_height + 1 < eth_v3_from_height` (that is,
`!eth_v3_at(tip + 1)`), a 2.9.61 node:

- refuses a local or wire `SubmitTx` of an `EthV3Deposit` with
  `V3 deposits not active` (not permanent), before verify, same-sequence
  eviction, or insert. The tx is not kept, is not gossiped, and does not take
  or evict anything at its account + sequence, so the submitter's next tx at
  that sequence is admitted normally;
- drops it from any peer `TxsBatch` (hub dish or `NeedTxs` reply) instead of
  storing it;
- never exports it in a `NeedTxs` reply or hub dish.

From `tip + 1 >= eth_v3_from_height` the same signed tx is admitted and
relayed like any other. This is mempool admission and relay only: apply and
verify are unchanged (an `EthV3Deposit` inside a block below H2 still fails
with `V3 deposits not active`), so state roots are unchanged.

## What a mint does, and what a refund does

**Mint** (same numbers V1 credits): `credit_asset` on ETH or USDC (native FPL
only if the asset were FPL, which V3 never is) and `total_minted += amount`.
No `claim_leaves` entry. In particular FPL never inserts a leaf under
`sha256("mint|" || depositId)`. V3 registers that mint note on the contract at
deposit time; a claim-root leaf would pay it out as a withdrawal and block
the refund.

**Refund:** `note = sha256("refund|" || depositId)` (7 ASCII bytes, then the
32-byte id). Append the `evm-wd-v1` leaf via `record_evm_withdraw` on that
asset's rail: dest = sender, amount and asset exactly as deposited, USDC flag
when the token is Sepolia USDC. No FPL balance change, no `total_minted`
change, no burn.

Token `address(0)` is ETH. Token `1c7d4b196cb0c7b01d743fbc6116a902379c7238`
is USDC. Anything else fails with `V3 deposit token is not ETH or USDC`
(permanent) and is not recorded. A tx whose `asset` does not match the log's
token fails with `V3 deposit asset mismatch` (permanent).

`depositId = keccak256(abi.encode(uint256 chainid, address bridge, uint256 nonce))`
— 96 bytes, not packed. Nonce is the pre-increment contract nonce (first
deposit is 0).

The log is `Deposit(bytes32,address,bytes20,address,uint256,uint64)`,
topic0 `e7783c72791736d4d8af95660d4367bed2f98d8331a095422373e557a4318728`.
topic1 = depositId, topic2 = sender (address, right-aligned). Data is
`abi.encode(bytes20 dest20, address token, uint256 amount, uint64 timestamp)`:
`bytes20` is left-aligned in its word, `address` is right-aligned, amount and
timestamp are right-aligned uints. Amount must fit in u64. Non-zero padding
rejects the log.

## Pools and fundPool

Refunds are paid later from the same `ethPool` / `usdcPool` as ordinary
peg-outs (spec note 4). FPL does not escrow a second pool and does not burn
on the way out.

`fundPool` emits `PoolFunded` and no V3 `Deposit`. A receipt that only has
that event does not mint, does not refund, and does not change `total_minted`.
The submit fails with `receipt has no matching V3 Deposit` (permanent).

## V2 notes on V3 (no double-pay)

V3's `openClaim` refuses (`"anchor"`) every noteId the V2 anchor header commits,
at any leaf index, and any `fplHeight <= anchorHeight`. `anchorHeight` (353130 on
Sepolia; must equal V2 `fplTip()` at deploy) and `anchorSet` (a data-only
`FalconQcBridgeV3AnchorSet` holding the 9 notes of V2's header 353130 as
withdrawal preimages, so they cannot stand for inner nodes) are
immutable constructor args; the constructor rebuilds the set's FPL tree root and
requires it to equal V2's `claimRoot`. Membership, not an index cutoff: FPL
orders a header's leaves per rail (all ETH, then all USDC), so a new ETH
withdrawal shifts every USDC leaf's index. Note 9 (leaf 10) is not in V2's
header 353130 and opens on V3.

Residual risk: see V3 anchor residual risk notes (someone advancing
V2 past the anchor with their own proofs; capped at V2's 0.026 ETH + 50 USDC).

## V1 after H2

On the first ledger with `eth_v3_at(height)` whose light client has
`execution_number != 0`, pack, verify, and commit all set
`eth_v1_deposit_cutoff` to that number (once; never lowered). It lives on
`BridgeState` next to the V3 resolution map. Both are `serde` default and
skipped while zero / empty, so pre-H2 snapshots and the commitment root stay
byte-identical to 2.9.60. Snapshot adopt loads them with the rest of
`BridgeState`.

After the cutoff is set, `mint_evm_lc` (V1, V2, and DestLock) rejects
`proof.height > cutoff` with `V1 deposit after V3 activation` (permanent).
Heights at or below the cutoff still mint while the header is inside the
window. The cutoff check sits after `deposit amount 0` and before the shared
receipt proof, so a height that is still allowed hits the header-window floor
first, exactly as in 2.9.60. V3 deposits are not subject to the cutoff.

## Rollout

Every committee seat must be running a binary that reports
`eth_v3_from_height == H2` before the tip reaches H2, and
`FPL_SEPOLIA_QC_V3_BRIDGE` must be the same address on every seat before then.
The runtime fields are not in the snapshot, so a restarted seat picks them up
from the binary and the environment, not from the file it loads. Do not ship
the `u64::MAX` / `None` placeholders expecting deposits to mint.

The H2 rollout commit (the one that sets `TESTNET_2300_ETH_V3_FROM_HEIGHT`)
should also set `DEFAULT_SEPOLIA_QC_V3_BRIDGE` to the deployed V3 address, so
the seats do not depend on six hand-set env values agreeing. Keep the env var
only as an override. This PR sets neither.

Hello accepts any `2.9.x` peer (from 2.9.60), so 2.9.60 and 2.9.61 seats can
connect while the roll is in progress. Block H2 itself forks any seat still
on 2.9.60: the V1 cutoff arms at the start of the first activated block whose
LC head number is resolved, even when that block has no ETH tx, and the
cutoff is in the state root.

The cutoff is the LC execution head at H2, which lags Sepolia by roughly two
epochs. A V1/V2/DestLock deposit mined after that head cannot mint and V1 has
no refund path, so point the wallet's peg-in at V3 (and stop V1 deposits)
**before** H2, with margin for the LC lag.

Do not switch too early either. Before H2 an `EthV3Deposit` is refused at
mempool entry with `V3 deposits not active` (and would fail with the same
error at apply), so a V3 deposit made early just waits off-chain until it
can be submitted, and its clock is running:

- older than E (3 days) when it is finally applied → it **refunds**, not
  mints;
- older than W (~7 days of execution headers) → the receipt proof fails
  permanently and the deposit is **stranded on the contract**. V3 has no
  guardian, owner, or admin refund, so nothing can recover it.

So switch the wallet's peg-in to V3 **no more than ~2 days before H2**, and
no later than H2 minus the LC lag. Keep `pl-eth-auto-mint.py --v3` running
from the switch onwards (and through any seat restarts) so every V3 deposit is
submitted well inside E to mint, and in any case inside W.

auto-mint keeps a V3 deposit whose dest it cannot resolve yet (unknown
`dest20` inside E) in its state file under `v3_pending` and re-plans it every
pass: a mint once the name is known, otherwise the refund as soon as E has
passed. It submits only the exec headers above the node's stored range
(`eth_lc.eth_exec_tip`) when that range already covers the deposit block.
Exec headers go through the relayer's `submit_header_chain` (#28): chunks are
packed into byte-capped waves (at most 2 MiB of header JSON), each wave is
submitted at consecutive explicit sequences, and the next wave (or anything
else from the account) waits until that wave's last sequence commits. So
several header txs can be in flight inside one wave; operators must not
assume one chunk at a time. The deposit tx itself (mint or refund) is sent
alone after the headers have committed, and waits for the watcher's
committed sequence to move before the next job, because ctl re-reads that
sequence and the node evicts a same-account, same-sequence tx. A sequence move alone does not prove the
deposit tx was the one that committed (another submitter on the account,
e.g. the relayer, can take the slot), so auto-mint re-submits the deposit
until mempool admission answers `V3 deposit already resolved`
(`FPL_V3_CONFIRM_TRIES`, default 3). A lost header chunk makes the deposit
tx fail to commit; the wait times out and the job is never marked done. Any
such transient failure (RPC, lost chunk, sequence race) puts the job back in
`queued` with a backoff (`FPL_V3_RETRY_SEC`, default 60, doubling, max 1h).
Only the node's permanent V3 errors (no matching Deposit in the receipt, bad
token, asset mismatch, malformed id, older than the header window) leave it
in `error`. A first scan with no cursor starts at `FPL_V3_SCAN_FROM`, or one
header window back from the LC execution head. The scanner checks that
`ETH_RPC` is chain 11155111 (`FPL_V3_CHAIN_ID`) before it reads logs, plans,
or moves its cursor; a queued job and the one-shot `--txid` path check it
again. The cursor never passes the node's LC execution head (the finalised,
provable head) nor `latest - FPL_V3_CONFIRMATIONS` (default 96), and does
not move while the node reports no LC head. The one-shot path refuses a
receipt above that point and queues nothing.

Until the node would admit a V3 tx (`status`: `tip_height + 1 >=
eth_v3_from_height` and `eth_v3_bridge` set), auto-mint submits nothing for
V3. Deposits seen before H2 stay `queued` and are processed in order once H2
is reached, instead of timing out into `error` and evicting each other at the
watcher sequence. V1 jobs are not held.

**Do not submit any `EthV3Deposit`** (auto-mint `--v3`, or
`falcon-pl-ctl submit-eth-v3-deposit`) until every seat runs 2.9.61. A 2.9.60
seat cannot decode the new tx variant, and a peer batch that carries one is
lost on that seat as a whole. A 2.9.61 seat refuses a pre-H2 `EthV3Deposit`
at mempool entry and never keeps or relays it (see above), so a pre-H2
submit to a 2.9.61 seat does not reach the mesh; but a 2.9.60 seat must
still never be sent one, and from H2 the tx is relayed, so every seat must
be on 2.9.61 before H2. Order: all seats on 2.9.61 → start auto-mint `--v3`
→ switch the wallet ≤ ~2 days before H2 → H2.

The `dest unresolved` error is not permanent on purpose, so a V3 tx without
a usable `credit_to` can stay in the mempool and be retried. If it is still
there when the deposit expires, that same tx applies as a refund (after E the
`credit_to` is ignored).

Status exposes `eth_v3_from_height`, `eth_v3_bridge` (`0x` + hex, or null),
`eth_v3_expiry_secs`, `eth_v1_deposit_cutoff`, `eth_v3_resolved_total` (the
size of the resolution map) and, from #38, the full map:

- `eth_v3_resolutions`: one entry per resolved deposit, in depositId order:
  `{deposit_id, kind: "mint", asset, amount, credit_to, eth_block}` or
  `{deposit_id, kind: "refund", asset, amount, refund_to, note_id, eth_block}`.
- `rails[].withdrawals` also lists each refund note as a row with
  `kind: "v3_refund"`, `from: ""`, `external_to: 0x<sender>`, `deposit_id` and
  `eth_block`, so `pl-eth-claim-proof.py --deposit-id` and walletd
  (`depositId`) can build its claim proof. `open_withdrawals` still counts
  RailWithdraw notes only.

Both are status-only views (nothing new is snapshotted or hashed). Status now
walks the map on every full status call, like `withdrawals` and
`claim_leaves`: cost and size grow linearly with resolved deposits, and the
map only grows. If that becomes too large, replace the dump with a paged or
per-deposit RPC. One id can also be read with `BridgeState::eth_v3_resolution`.

## Peg-out claims: V2 vs V3 (walletd)

FalconQcBridgeV3 refuses a note already opened on V2 (`paid-v2`), but
FalconQcBridgeV2 has no idea V3 exists. A note committed at or below the V3
anchor (the V2 `fplTip` the V3 constructor copied) is provable on **both**
bridges, so a note opened on V3 could otherwise be opened again on V2 and paid
twice. walletd's `eth-open-claim` therefore enforces this whenever
`FALCON_QC_V3_BRIDGE` is set:

- It routes to V2 only while V2 `fplTip <= FALCON_QC_V3_ANCHOR_HEIGHT`. Past
  the anchor, every note goes to V3.
- It never opens on V2 a note whose V3 `claims(note).dest` is non-zero, and
  never opens on V3 a note whose V2 `claims(note).dest` is non-zero.
- Both reads (V2 `fplTip`, the other bridge's `claims(note)`), plus the target
  bridge's own `claims(note)`, run under `WRITE_LOCK` right before the send.
  If the note is already open on the target bridge, walletd answers
  `{ok: true, tx: "", alreadyOpen: true, taken, dest, noteId, fplHeight,
  bridge, bridgeVersion, mode}` (plus `kind`, `depositId` for a refund note)
  instead of sending a tx that would revert with `exists`. `taken` and `dest`
  are that bridge's `claims(note).taken` and `.dest`: with `taken: true` the
  note is already paid and `take()` would revert `state`, so a client must not
  call it; otherwise only `dest` can call `take()`. This covers two racing
  requests for the same note.
- With both `FALCON_QC_V2_BRIDGE` and `FALCON_QC_V3_BRIDGE` set, a missing or
  invalid `FALCON_QC_V3_ANCHOR_HEIGHT` makes walletd refuse **every**
  `eth-open-claim`, including notes that would go to V3 (it fails closed).
  With only V3 set, the anchor is not read.
- With both bridges set, walletd also checks that V3 `headers(anchor).hash`
  equals V2 `headers(anchor).hash` (read-only `eth_call`s), at startup and
  again before the first claim until it passes. A mismatch or zero hash
  refuses every claim until walletd restarts with a fixed config; a read error
  refuses claims and the check is retried on the next request. Other walletd
  actions keep working.

walletd reads only `FALCON_QC_V3_BRIDGE`. The seat/consensus
`FPL_SEPOLIA_QC_V3_BRIDGE` is not used for claims.
`pl-bridge-install-config.py` writes both `FALCON_QC_V3_*` values from
`run/bridge/sepolia-qc-v3.json` `{"bridge": "0x...", "anchor_height": N}`.
A record without a positive integer `anchor_height` stops the install.

**Residual risk: this guard covers walletd only.** `openClaim` is
permissionless on both bridges. Any caller (for example the note's own dest)
can open a note on V3 and then open the same note on V2 against the frozen
header, or race walletd between its read and its send. V2 never checks V3.
Freezing V2 headers stops V2 from committing new notes, but the notes already
committed at the anchor stay payable on both bridges while V2 holds liquidity.
Only an on-chain cutover closes that, for example:
- V3 refusing leaves the anchor header already commits; or
- V2's pools no longer covering those notes.

That is a contract/ops decision outside this change, and is open on #38.

### Runbook: at the V3 deploy

1. Record the anchor: `anchor_height` = V2 `fplTip()` at the V3 deploy block
   (by FM ruling 353130, since V2 headers stay frozen there). Write it with the
   address into `run/bridge/sepolia-qc-v3.json`, then rerun
   `pl-bridge-install-config.py` and restart walletd.
2. **Permanently stop and disable the V2 header prover**:
   `falcon-pl-falcon-qc-v2-prover` on the prover host
   (`systemctl disable --now falcon-pl-falcon-qc-v2-prover`; mask it so it
   cannot be restarted). V2 headers stay frozen at 353130 for good.
3. Never top up the V2 prover account (0xDb52…) again, so nothing can push a
   V2 header past the anchor. This does not stop `openClaim` against the
   frozen header (see the residual risk above).
4. Check: V2 `fplTip()` still equals `anchor_height`, walletd's startup log
   has no `V3 anchor check failed` line, and walletd's `eth-open-claim` for a
   note past the anchor answers `bridgeVersion: "v3"`.
5. Run **exactly one** walletd against these bridges. The double-pay guard is
   a per-process `WRITE_LOCK`; a second walletd (or a second host) does not
   share it and can race the first one.
6. While walletd is live, do **not** run `pl-eth-sepolia-e2e.py` `openClaim`
   sends against V3 (or V2). That script does not take walletd's lock or run
   its V2/V3 checks. Stop walletd first if a manual e2e open is needed.

These steps are run by the operator at the deploy. This document changes no
server.

## V3 prover key

`pl-falcon-qc-v3-prover.py --send` signs with a fresh, funded prover key named
`FPL_V3_PROVER_KEY` (never the old deployer key, never a path, never in argv).
The recommended launch is a systemd **system** unit with `LoadCredential`, so
the key is never in any environment:

```ini
# /etc/systemd/system/fpl-v3-prover.service
[Service]
User=fpl-prover
# root-owned, mode 0400, 32-byte hex on one line. Or use
# LoadCredentialEncrypted= with a `systemd-creds encrypt` blob.
LoadCredential=FPL_V3_PROVER_KEY:/etc/falcon-pl/creds/fpl-v3-prover.key
Environment=FALCON_QC_V3_BRIDGE=0x...
ExecStart=/usr/bin/python3 /opt/falcon-pl/scripts/pl-falcon-qc-v3-prover.py --send
Restart=on-failure
```

The script reads `$CREDENTIALS_DIRECTORY/FPL_V3_PROVER_KEY`. Setting the
credential and the env var together is refused. Only `--send` broadcasts;
`--no-dry-run` without `--send` is refused. Do not put the key in
`Environment=` (`systemctl show` prints it) or `EnvironmentFile=`.

If the key does come in through the env var, the script validates it and
re-execs itself in place (same pid, `os.execve`) without the variable; the
key crosses the exec in an inherited pipe that is read once and closed. The
running prover's `/proc/<pid>/environ` and cmdline then hold no key
(`test_pl_falcon_qc_v3_prover.py` checks this from a child of the running
prover). A bad or missing key on `--send` fails before the exec.

Residual exposure, all owner/root only: the key stays in the prover's heap
for its lifetime (readable by root or same-uid ptrace, so run it as its own
user); with the env route the launching shell or unit still had it, and this
pid's environ held it for the moment before the re-exec; with
`LoadCredential` the plaintext sits in systemd's per-unit credentials
directory until the unit stops (the script drops `CREDENTIALS_DIRECTORY`
from its environment after reading, so ctl / prove helpers / cast are not
handed the path, but a same-uid process that knows it can open the file); during a send the encrypted keystore and its
password are reachable through `/proc/<pid>/fd` until the fds close.
