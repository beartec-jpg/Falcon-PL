# V3 test plan: anvil Sepolia-fork dry run

Plan step: Falcon ETH plan Phase 3 test item ("an anvil Sepolia-fork dry run with
4 real proofs before spending anything") and Phase 4/5 checks (mint inside E,
refund after E, double submit refused, refund leaf provable against a V3 header).
Sources: the Falcon ETH plan (kept outside this repo), Release B spec notes,
handoff issue #25, [ETH_V3_DEPOSITS.md](ETH_V3_DEPOSITS.md), `docs/ops/qc-bridge-v3.md` §5 and §7,
`docs/ops/HANDOVER.md`.

**Nothing here touches Sepolia.** The script starts a local `anvil --fork-url`
of public Sepolia and sends every transaction to that local node. Accounts are
anvil's public dev accounts. The prover key is anvil dev key #1, given to the
prover only through `FPL_V3_PROVER_KEY` (never argv). The old deployer
`0xDb52…` is never used, and the one blocked note paid to it is skipped.
Every transaction goes through `Fork.send()` / `Fork.deploy()`, or (S2, S7b, S7c, S8
headers) the prover's own send path via `prover_send()`, which calls the same
guard before `prov.emit` and also checks the prover's argv names only the fork's
`--rpc-url`. The guard refuses to run unless the URL is this script's own
`http://127.0.0.1:<port>` anvil and the sender is not `0xDb52…`.

## How to run

```bash
# foundry (anvil, cast, forge) in ~/.foundry/bin; cargo for the FPL step
python3 falcon-pl-rs/crates/fd-pl/scripts/v3-fork-dry-run.py --out /tmp/v3dry
#   --fork-url URL      default https://ethereum-sepolia-rpc.publicnode.com
#   --fork-block N      pin the fork (default: latest)
#   --skip-rust         stop after the auto-mint steps (no cargo); verdict PARTIAL
#   --expect-known-findings   failing F1/F4 (open) are reported KNOWN and do not fail the run;
#                             F2/F3 (fixed in #41) and every S check stay fatal
```

Verdict (printed last, and `verdict` in `report.json`): **PASS** (exit 0) only
when S0–S8 (S7a/b/c included) and F1–F4 all ran, every S passed, and every F
passed (with `--expect-known-findings`, F1/F4 may fail as KNOWN). **PARTIAL** (exit 3) with `--skip-rust`. **FAIL**
(exit 1) on any scenario failure, an abort, or a dropped scenario. `report.json`
and the summary are written even when a send guard stops the run.

It runs `cargo test -p fd-pl --lib eth_v3_fork_dry_run_tests::v3_fork_dry_run_case -- --ignored`
with `RUSTFLAGS` removed from the environment. That test is `#[ignore]`, so the
normal test run and its baseline are unchanged; run by hand with `--ignored` and
no `FPL_V3_FORK_CASE`, it fails instead of passing having tested nothing. Output: `report.json` (every scenario, real vs mocked,
evidence), `fork-case.json` / `fork-result.json` (the Rust step's input and
output), `automint/` (auto-mint job state and the mock ctl's record), and
`anvil.log`. Takes about a minute after the first cargo build.

Files:

| File | Role |
| --- | --- |
| `scripts/v3-fork-dry-run.py` | Driver. Starts anvil, runs every scenario, prints the summary. |
| `scripts/v3-dry-run-mock-ctl.py` | **Mock** `falcon-pl-ctl` (status / account / submit-eth-v3-deposit). Records submits; no node. |
| `src/eth_v3_fork_dry_run_tests.rs` | FPL consensus replay of the recorded submits on the fork's real headers and receipts. |

## What is real and what is mocked

Real on the fork: FalconQcBridgeV2 / V1 / DestLock, Sepolia USDC and the
production Groth16Verifier `0x2Cb7…4F80` as deployed; FalconQcBridgeV3Checks and FalconQcBridgeV3 bytecode
built from this repo in one build (solc 0.8.20, via_ir, optimizer 200); the four real Groth16 proofs
for header 353131 in `scripts/fixtures/v2-groth16/plus1-submit.json`; the V2-era
claim leaves in `scripts/fixtures/eth-claim-proof/v2-header-353130.json` (#33);
`pl-falcon-qc-v3-prover.py`'s send path (`MemfdSigner` + `cast send`);
`pl-eth-auto-mint.py --v3` (log scan, plan, pending/replan, receipt MPT from the
relayer, the exact `submit-eth-v3-deposit` argv); `pl-eth-claim-proof.py`; the
fd-pl consensus code on three in-process nodes (`EthExecHeaders` with the fork's
header RLP, `EthV3Deposit` pack/apply, mempool admission).

Mocked, and why:

| Mock | Why |
| --- | --- |
| FPL node behind ctl (`v3-dry-run-mock-ctl.py`) | No node runs locally. It records each submit and answers admission for a repeat id with the node's text. The real mint/refund decision is made in the Rust step. |
| ETH light client | Anvil blocks have no beacon chain. The Rust step sets `eth_lc` on every node to the fork block used as the head; auto-mint's LC step (`relayer cmd_update`, which calls Lodestar) is replaced by the same head. The mock slot is `(head time − beacon genesis) / 12`, so it is consistent with the head time as a real LC's would be. |
| 3-node `Network` harness | Stands in for the 2300 seats. |
| Groth16 for headers after 353131 | A header that commits fork-made refund leaves has no committee signatures. After the real-proof step the verifier address gets always-true code (`anvil_setCode`). Only S7b, S7c and S8 use it (S7c: a header carrying the real 10-leaf FPL claim root `4146c6c6…`, which no real-proof header carries yet). |
| USDC balances | Dev accounts get USDC with `anvil_setStorageAt` (the FiatToken balance-mapping slot is found by probing `balanceOf` on the fork; it was 9 in every run so far). |
| EIP-7702 code on dev accounts | On Sepolia, anvil's public dev accounts carry 7702 delegations (sweepers that forward incoming ETH). The fork clears that code on the 5 accounts used, or balances would measure the sweeper, not V3. |
| `take()` by a real note's dest | S7c impersonates note 9's dest `0x722b…` on the fork only. |

## Scenarios and pass criteria

| ID | Scenario | Pass when |
| --- | --- | --- |
| S0 | Pre-flight | Fork is chain 11155111 and anvil; V2 `fplTip` = 353130 and `headers(353130)` equals the fixture anchor; V2 verifier is `0x2Cb7…`. The real FPL leaves (#33 fixture) hash to V2's `headers(353130).claimRoot` at exactly 9 leaves; those 9 become the anchor set. |
| S1 | Deploy on the fork: FalconQcBridgeV3Checks, FalconQcBridgeV3AnchorSet(9 notes), then FalconQcBridgeV3 | Each deployment uses under 15,000,000 gas (EIP-7825 cap is 16,777,216). The set's code is exactly `0x00 ‖ entries` (noteId ‖ dest ‖ amount ‖ isUsdc) and `Checks.anchorSetRoot(set)` equals V2's anchor root. V3's `checks()` is the Checks address; a V3 constructed with any other contract there reverts `checks`. Constructor args `(usdc, groth16, v2, v1, destLock, 50000, checks, 353130, anchorSet)`; anchors from real V2: tip, committee root and the 6 leaves match; `anchorHeight()` 353130, `anchorSet()`, `anchorLeafCount()` 9; a constructor with anchorHeight 353131 reverts `tip`, with no set or a non-set contract reverts `set`; `maxJump` 50000; network 2300. `owner()`, `guardian()`, `pause()`, `rescue()` all revert on both contracts. |
| S2 | Prover, real proofs | The prover reads tip/maxJump/leaves from V3 and picks H = 353131. A tampered proof and tampered `nextLeaves` are rejected by the real verifier. The send goes through `MemfdSigner` (key not in argv, env cleared) from dev account #1, is accepted, `fplTip` moves to 353131 and the committee rotates. Re-submitting the same header reverts `range`. |
| S3 | Deposits | `depositEth` / `depositUsdc` emit the V3 `Deposit` with topic0 `e7783c72…`, `depositId = keccak256(abi.encode(chainid, bridge, nonce))` from nonce 0, sender in topic2; `pegIns` stored, unresolved. D1 ETH and D2 USDC go to `dest20("sally")`; D3 ETH and D4 USDC to an unknown name. `fundPool` emits only `PoolFunded`. Zero amount and zero dest revert. |
| S4 | auto-mint `--v3` inside E | After 110 confirmations the scan queues D1/D2 as mints and submits them with `--credit-to sally`; D3/D4 stay in `v3_pending`; `--v3 --txid <fundPool tx>` says "no V3 Deposit". |
| S5 | Refund after E | `evm_increaseTime(E + 1h)` and 110 blocks. The next pass re-plans D3/D4 as refunds (`credit_to` empty) and submits them. `--v3 --txid <D1>` submits nothing; a forced re-submit of D1 gets "already resolved". |
| S6 | FPL consensus | On 3 nodes: the fork headers apply through `EthExecHeaders`; inside E, D3/D4 with no `credit_to` fail "dest unresolved" (not permanent, no state change); D1/D2 mint `sally` (`total_minted` moves); after E, D3/D4 refund to the sender, `evm-wd-v1` leaf under `sha256("refund|" ‖ id)`; every resolved id is refused at admission on every node and at apply, with any `credit_to` (12 replays); the fundPool receipt fails "receipt has no matching V3 Deposit"; state roots agree. The refund note ids and leaves equal the contract's `refundNoteId` / `withdrawLeaf`. |
| S7a | V2-committed note refused | `pl-eth-claim-proof.py --header-claim-root` builds a proof for a real ETH note committed by V2's header against header 353131 (real proofs, claim root `9671942a…` = V2's anchor root); `openClaim` reverts `anchor` against 353131 and against the anchor 353130. Leaf 8 at index 8 reverts `anchor`; its padded copy at index 9 (same root) reverts `pad`. |
| S7c | Note 9 opens on V3 | Header carrying the real 10-leaf root `4146c6c6…` (mock verifier). The claim tool finds Scott's note 9 (USDC, leaf index 9, not in V2's header 353130) ready; `openClaim` + `take` pay its dest exactly once (reopen `exists`, other caller `dest`, second take `state`). The S7a note under this root still reverts `anchor`. |
| S7b | Refund claim | A header (mock verifier) carrying the cumulative root in FPL's per-rail order (`lc::evm_claim_leaves`): real ETH rail + the Rust run's ETH refund, then real USDC rail + the Rust USDC refund. The ETH append moves V2-anchor USDC leaf 8 to index 9; opening it there reverts `anchor` (set membership, not an index cutoff); note 9 moves to index 10 and its path still verifies; the Rust root/paths still equal `build_tree` over the Rust leaves alone; each refund leaf is opened with the same tree code as withdrawals (`openClaim` gas is recorded; F4 compares it with walletd's limit) and `take` pays the original sender the exact amount (ETH and USDC); `pegIns.resolved` is set; reopen `exists`, other caller `dest`, second take `state`. |
| S8 | Double-resolve on-chain | With a hand-built root (real ETH rail, then two ETH leaves FPL never emits, at 5 and 6, then the real USDC rail): the `mint|` note of an already-refunded deposit reverts `resolved`; a refund leaf for an unresolved deposit but a different dest reverts `refund-dest`. |
| F1–F4 | Bug checks | See findings. Each fails until its bug is fixed (F2/F3 are fixed by #41). A full run must run all four. `--expect-known-findings` reports a failing F1 or F4 (the open ones) as KNOWN instead of failing the run; a failing F2/F3 stays fatal. |

Not covered here: jump proofs for a recent H (needs seat attestations and
`pk.bin`; only the committed +1 fixture is available locally); the real beacon
LC; committee-signed headers carrying refund leaves; seat rollout; Sepolia gas
prices.

## Last run

2026-10-09 05:20:41–05:21:46 BST, fork of Sepolia block 11874913 via publicnode,
anvil/cast/forge 1.8.5, final contracts (Checks + AnchorSet of 9 note preimages + V3),
prover with F2/F3 fixed, `--expect-known-findings`. S0–S8 (S7a/b/c included) all
**PASS** (including the `rescue()` check); F2, F3 **PASS**; F1, F4 **KNOWN** (open).
**VERDICT: PASS**, exit 0. (Earlier: 02:31 BST before the split, S0–S8 PASS, F1–F3
FAIL; 03:02 BST two-contract split, PASS; 04:44 BST block 11874735, same figures, PASS.)

| | Measured on the fork |
| --- | --- |
| Deploy 1: `FalconQcBridgeV3Checks` | **8,103,275** gas (runtime 5,106 bytes) |
| Deploy 2: `FalconQcBridgeV3AnchorSet` (9 notes) | **1,100,663** gas (runtime 550 bytes) |
| Deploy 3: `FalconQcBridgeV3` | **14,690,093** gas (runtime 8,525 bytes) |
| Total deploy | 23,894,031 gas over three transactions, each under 15 M (2^24 cap = 16,777,216) |
| `Checks.anchorSetRoot(set)` | `0x9671942a…cfa5` = V2 `headers(353130).claimRoot` |
| Before the split (one tx) | 17,205,421 gas (runtime 9,947 bytes); Sepolia `eth_estimateGas` 17,343,139 |
| `submitHeaderWithProofs` 353131, 4 real proofs | 2,208,069 gas; prover default limit 2,760,087 (estimate × 1.25), status 1 |
| `openClaim`, note 9 (USDC, 4-level proof, 10-leaf root) | **650,814** gas; `take` 176,526 |
| `openClaim`, refund D3 (ETH) / D4 (USDC) | **670,290** / **670,341** gas; `take` 45,985 / 78,606 |
| V2-anchor USDC leaf 8 after one ETH append | moves to index 9; `openClaim` there reverts `anchor` |
| USDC balance slot found by probing | 9 |

## Findings

1. **F2, prover gas limit (script bug, blocks the real jump). Fixed in #41.**
   `pl-falcon-qc-v3-prover.py` `GAS_LIMIT_DEFAULT = 2_000_000`, but a real
   4-proof `submitHeaderWithProofs` uses 2,207,860 gas on the fork. With the
   default, the send runs out of gas and burns 2.0 M gas (seen on the fork: status 0,
   gasUsed 2,000,000). Workaround: `--gas-limit 2700000`. Proposed fix: default
   ≈2.7 M (measured + ~20%).
2. **F1, no claim tooling for refund notes (script gap).** A refund is a
   claim leaf (`rails[].claim_leaves` / `lc.claim_leaves`) but not a rail
   `withdrawals` entry, and status does not list V3 resolutions. So
   `pl-eth-claim-proof.py` (and walletd, which uses it) answers "withdraw note not
   packed yet" for a refund note. Users cannot get a refund proof from the wallet.
   Proposed fix: let the claim tool take `--note` + `--dest` + `--amount` +
   `--asset` (or the deposit id) and find the leaf directly in `claim_leaves`,
   and expose refunds (note id, sender, amount, asset) in status or a ctl query.
3. **F3, auto-mint LC head time with cast 1.8.5 (script bug, low impact). Fixed in #41.**
   `head_block_timestamp()` runs `cast block N timestamp`; cast 1.8.5 needs
   `cast block N --field timestamp` and errors, so the function always returns
   `None` and planning falls back to `1655733600 + 12 * finalized_slot`. With a
   real LC that fallback is the head's own slot time, so plans are still right,
   but the primary path is dead.
4. **Deploy gas: fixed by splitting the deploy.** The one-shot deploy used 17.2 M gas,
   because Sepolia runs Amsterdam (active 6 Oct 2026 14:53 BST, `eth_config`) and its
   state-creation pricing (EIP-8037) charges about 1,541 gas per deployed byte and
   about 111 k per new storage slot (measured read-only on public Sepolia). Read-only
   cap check: EIP-7825's 2^24 cap is enforced on *regular* gas (an infinite loop sent
   with a 33.5 M limit stops at exactly 16,777,216), while state gas comes from a
   reservoir above the cap. A simulated 17.3 M-limit send of the old deploy succeeded
   with validation on. So the old deploy was not strictly blocked, but it is now two
   transactions (`FalconQcBridgeV3Checks`, then `FalconQcBridgeV3`), each under 15 M.
   See `qc-bridge-v3.md` §5.
5. **V2/V3 double-pay (Copilot r4226045086 on #38): fixed in the contract.** V3 copied V2's
   anchor root and only checked V2's `claims` map, so a note committed by V2 header 353130
   could be paid on V3 and later on V2 (V2 records it only once paid there). V3 now takes
   `anchorHeight` (353130, must equal V2 `fplTip()` at deploy) and `anchorSet`, a data-only
   `FalconQcBridgeV3AnchorSet` holding the 9 notes V2's anchor root commits, as withdrawal
   preimages. The constructor rebuilds their leaves and FPL tree root on-chain and requires it
   to equal V2's `claimRoot` (`set`); `openClaim` refuses any of those noteIds at any index, and
   any header `<= 353130` (`anchor`). Preimages, not raw leaf hashes: this tree format has no
   leaf/inner-node domain separation, so a raw-hash set of inner nodes (the root's two
   children) would rebuild the same root and protect nothing (Copilot on #45). A first version used an index cutoff (`index < 9`); Copilot on #45 showed it
   fails because FPL orders a header's leaves per rail (all ETH, then all USDC), so one new
   ETH withdrawal moves every V2-anchor USDC leaf up one index (leaf 8 → 9). S7b checks that
   case on real data. Also: FPL pads an odd layer by repeating its last node, so a leaf has a
   second (padded) index; `merkleRoot` refuses a padded copy (`pad`), so each leaf has one index.
   Residual V2-side risk and why V2 is unchanged: ETH_V3_DEPOSITS.md "V2 notes on V3".
   Otherwise mint, refund, replay, double-resolve, refund-dest and pool accounting all
   behaved as specified.

6. **F4, walletd openClaim gas limit (script gap; fix in walletd).** walletd sends
   `openClaim` with V2's fixed 400,000 gas. On Amsterdam-priced Sepolia, V3 `openClaim`
   uses 650–670 k (the new `Claim` record is 5 storage slots at ~111 k each), and
   V2 is the same today: a read-only `eth_estimateGas` of V2 `openClaim` for the same real
   note against V2 header 353130 returned 601,309. Proposed: walletd's V3 limit
   840,000 (worst measured 670,341 × 1.25, rounded up), or better, `eth_estimateGas` × 1.25 as the
   prover now does. The V2 path needs the same change.

Observations (not bugs; the first is #42): after E, `--v3 --txid` on an already-minted deposit
plans a `refund` (the job is already `done`, so nothing is sent, and the node
would refuse it anyway). A refund to an EOA with an EIP-7702 delegation runs the
delegate's code on receipt; a sweeper delegate forwards it away (this is what
the anvil dev accounts do on public Sepolia).

## Real Sepolia deploy, once a funded new prover key exists (operator / Falcon Manager)

1. F2/F3 are fixed in the prover PR (#41): the prover's default gas limit is
   `eth_estimateGas` × 1.25. Fix F1 (#36 / #38) before telling users refunds are claimable.
2. Re-run this dry run on a fresh fork the same day; check S0 (V2 still at
   353130, same anchor, 9 leaves) and S1/S2 pass. If V2's `fplTip` has moved, stop:
   the constructor reverts `tip`, and the anchor and its leaf set must be re-derived.
3. Build all three contracts in **one** build (solc 0.8.20, via_ir, optimizer 200, as
   `build_v3()` does). V3's constructor checks the Checks runtime code hash, so
   Checks must come from the same build as V3.
   Read-only, from the new key's address: `cast estimate --create <Checks initcode>`,
   then the AnchorSet with the 9 notes, then `cast estimate --create <V3 initcode + ctor>`
   with the predicted Checks and AnchorSet addresses (`cast compute-address <key> --nonce N`,
   `N+1`). Each must be under 15,000,000 gas
   (finding 4; fork figures under "Last run").
4. Three deployments from the new key, **in this order**:
   1. `FalconQcBridgeV3Checks` (no constructor arguments). Record its address.
   2. `FalconQcBridgeV3AnchorSet(noteIds, dests, amounts, isUsdc)` with the 9 notes S0 derived
      (#33 fixture withdrawals matched to `lc.claim_leaves[0..9]`, in that order; their FPL root
      is V2 `headers(353130).claimRoot`). Check its code is `0x00 ‖ entries` and
      `Checks.anchorSetRoot(set)` equals V2's anchor root. Record its address.
   3. `FalconQcBridgeV3` with constructor `(USDC 0x1c7D…7238, Groth16 0x2Cb7…4F80,
      V2 0x8118…B23e, V1 0xf8F1…bc48, DestLock 0xdBF6…1C05, maxJump 50000, checks <step 1>,
      anchorHeight 353130, anchorSet <step 2>)`. `anchorHeight` must equal V2 `fplTip()`
      when the tx runs (else `tip`); the set's root must equal V2's anchor root (else `set`).
   Set each transaction's gas limit from its own estimate (about +10%); all stay
   under the 2^24 cap. Check `checks()`, `anchorHeight()` 353130, `anchorSet()`, `anchorLeafCount()` 9, tip / committee root /
   leaves against V2 as in S1, and that `owner()/guardian()/pause()/rescue()` revert. Record the V3 address
   (the only address users, the prover and walletd need).
5. `fundPool` with the blocked notes' totals plus a buffer (§5 of qc-bridge-v3.md).
6. Prover: `FPL_V3_PROVER_KEY` via systemd `LoadCredential` (ETH_V3_DEPOSITS.md),
   `FALCON_QC_V3_BRIDGE=<V3>`, first run without `--send` (dry-run calldata), then
   `--send` with the fixed gas limit. Jump 353130 → recent H; check `fplTip` and
   the rotated committee.
7. Claims: `pl-eth-claim-proof.py --note … --fpl-height H --header-claim-root
   <headers(H).claimRoot>` then `openClaim` / `take` for the blocked notes. Only notes
   outside V2's anchor set (note 9 onward) open on V3; the 9 V2-committed notes revert
   `anchor` at any index and stay with V2. `openClaim`
   uses about 650–670 k gas on Sepolia now (F4): give it a limit from `eth_estimateGas`
   (+25%), not walletd's old 400,000.
8. Release B activation (ETH_V3_DEPOSITS.md "Rollout"), in this order:
   1. In the 2.9.61 rollout commit, set `TESTNET_2300_ETH_V3_FROM_HEIGHT = H2` and
      `DEFAULT_SEPOLIA_QC_V3_BRIDGE` = the V3 address from step 4.
   2. Roll every seat to that build, so all seats report `eth_v3_from_height == H2`
      and the same V3 address before the tip reaches H2.
   3. Start `pl-eth-auto-mint.py --v3` (it holds V3 deposits until activation).
   4. Switch the wallet's peg-in to V3 (and stop V1 deposits) **no more than ~2 days
      before H2** (an earlier V3 deposit can pass E before H2 and only refund) and
      **no later than H2 minus the LC lag** (a V1 deposit mined after the LC head at
      H2 can never mint and V1 has no refund). Same rule as ETH_V3_DEPOSITS.md.
9. End-to-end on the site: small ETH and USDC peg-in to V3 (mint), one to an
   unknown name left past E (refund), claim the refund, one peg-out.
