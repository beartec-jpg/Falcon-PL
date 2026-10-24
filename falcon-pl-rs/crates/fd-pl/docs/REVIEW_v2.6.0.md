> **HISTORICAL.** This file is not the live Falcon PL **2300** dest-lock specification.
> Current: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md) and [README.md](README.md).
> Falcon Ledger **1001** lock-mint is shut down.

# Falcon PL v2.6.0 — Full Review

| | |
|--|--|
| **Date** | 2026-08-13 |
| **Target** | Live working tree on `host1:~/falcon-dag` (`fix/pl-multihost-state-determinism` + uncommitted 2.6 liveness work) |
| **Product version in code** | `2.6.0` (`fd_pl::PRODUCT_VERSION`) |
| **Cargo workspace version** | `2.3.6` (stale) |
| **Network** | private v22, `network_id=2200`, scheme `falcon-512` |
| **Live at review** | 3 hubs (host1 v1/v2, host2 v3) agreed `tip=49` hash `3ef4adca…`; load-net running; mempool empty; lottery seat `v3`; no pending proposal |
| **Issue counts** | 8 bugs, 4 suggestions, 2 nits |

Companion: **[PUBLIC_TESTNET_CHECKLIST.md](PUBLIC_TESTNET_CHECKLIST.md)** — numbered work before a public testnet cut.

---

## Summary

Falcon PL is a real, running ordered-ledger prototype: Falcon-512 signs txs and packer seals, lottery election is now tip-deterministic, tip only advances after soft QC, and the last two weeks of AMDs are honest about multi-host forks and mempool freezes. That is a successful private-testnet engine, not vapor.

It is **not** ready to treat consensus votes, RPC, bridges, or emission as adversarial. Wire votes are an unauthenticated `voter: String`. `SubmitTx` overwrites the account public key *before* verify. Admin `WireMsg`s (mint, claim, bond, snapshot load, hard-reset, rail mint) mutate state without a signature and without going through a ledger. Those are safety bugs on any host that is reachable, including the current Tailscale mesh.

v2.6.0 is the right place to keep testing liveness (open-height poison, skip cursor, view-change). Do not freeze a public parameter sheet, and do not call the bridge or watcher economy “custodialess / proven,” until the auth holes below are closed.

## What is in good shape (do not thrash)

- **Tx and packer crypto (Falcon-512 path):** detached sign/verify via `pqcrypto-falcon`, payload domain `pl-tx:v2|…|network_id|body`, `tx_id` binds the payload, packer sig checked in `accept_ledger_ex`.
- **Lottery (v2.3.4–v2.5):** seed is tip + bonded set + frozen ONLINE tag + view round — not local pongs. Skip advances a preselected seat; it does not reseed.
- **Commit rule:** live proposals stage; tip moves only after soft QC. Empty ledgers rejected. Equivocation jails.
- **Catch-up:** residual `from=tip+1`; no auto genesis wipe (P2). Operator hard-reset is explicit (but unauthenticated — see issues).
- **v2.6 mempool sanitize:** always drop `seq < need`; drop `seq > need` only when poisoned. That matches the AMD-20260813-2 write-up and the `sanitize_keeps_sequential_pipeline` test intent.
- **Epoch pack/check settlement:** rebuilt from sealed chain, not gossip vote counts — correct response to the dual-host claimable-divergence bug.
- **Docs honesty:** PoC explicitly says not a BFT proof, not production gossip, in-memory history can OOM.

## Live test note (this session)

Monitor 17:37–17:39 UTC: all three hubs stuck at **tip=49 / same hash**, `mempool=0`, `pending=None`, `lottery_winner_next=v3`, `i_may_pack=false` on v1. Load-net still alive. That is either a drained inject window or the current liveness bug (winner v3 not sealing while inject is deferred/dropped). Not a fork. Worth a dedicated dig: *why mempool stays 0 while load-net is up*.

## Issues

### Issue 1 -- Severity: bug
- File: crates/fd-pl/src/bin/falcon_pl_node.rs:1311
- Description: `SubmitTx` calls `keys.insert_public(&tx.account, &tx.public_key)` **before** `submit_tx` / `tx.verify`. `insert_public` unconditionally overwrites the KeyRing entry. `verify_message` then treats the attacker key as the “known” key and only checks sig-under-that-key. On the Falcon-512 path this is account takeover of any funded id (w00…, faucet, alice, a validator id if used as a tx account). HMAC-dev is separately forgeable (`pl-secret:{id}`).
- Suggestion: Never insert a wire public key before verify. Verify against the **existing** registry key if present; reject if missing or mismatched. First-seen keys only via an authenticated join/bond tx, not SubmitTx/Hello.
- Status: open

### Issue 2 -- Severity: bug
- File: crates/fd-pl/src/lib.rs:4737
- Description: `LedgerOk`, `SkipPacker`, `ViewChange`, and `OnlineSetVote` carry a bare `voter: String` with **no signature**. `vote_ok_ex` (lib.rs:3144) records any non-empty voter and does not check that the voter is a bonded, non-jailed checker. Soft QC is `min(3, n)` (floor 2 when n≥2). Anyone who can open the JSON-TCP port can mint 2–3 fake voters and commit an arbitrary staged ledger, skip the real packer, or view-change the height.
- Suggestion: Sign every vote with the checker’s Falcon-512 key over a domain-separated payload (`pl-ok:v1|network|height|hash|voter`, same for skip/VC/online). Reject unknown / jailed / unbonded voters. Require the voter id to match the key id.
- Status: open

### Issue 3 -- Severity: bug
- File: crates/fd-pl/src/bin/falcon_pl_node.rs:1668
- Description: Side-channel `WireMsg` still **mutates committed state** without a sequenced, signed tx: `VaultOpen`/`VaultLock`, `ClaimRewards`, `CreatePool`, `AddLiquidity`, `Swap`, `Bond`/`Unbond`, `CreditAsset`, `WatcherCredit`, `Tick`, `ForceSettleEpoch`, `RailHeader`/`RailDeposit`/`RailWithdraw`, `SnapshotSave`/`SnapshotLoad`, `OperatorHardReset`, `SealNow`. `lib.rs:89` says these must enqueue as signed txs; the node does not. Effects: steal claimable, mint FPL (`CreditAsset` with asset=FPL calls `credit_native`), self-award watcher emission, rewrite vault allowlists, load an attacker snapshot (including `cfg`), arm genesis resync, invent a rail header chain and mint wrapped BTC. `fan_state` only stops *amplification*, not the local apply.
- Suggestion: On the node, accept only `Hello`/`Pong`/`Need*`/`SubmitTx`/`ProposeLedger`/`LedgerOk`/`Skip`/`ViewChange`/`OnlineSetVote`/`Status*`/`AccountQuery`. Everything that touches balances or config must be a signed `TxBody` included in a ledger. Gate `OperatorHardReset` / snapshot load on a pre-shared operator key or a unix-admin socket, not the public listen port.
- Status: open

### Issue 4 -- Severity: bug
- File: crates/fd-pl/src/lib.rs:3453
- Description: `TxBody::CreditAsset` is a user-signable protocol op that calls `credit_asset_admin`. That function mints **native FPL** when `asset` is `FPL`/`NATIVE`. Any key that can pay a fee can inflate supply. Same class: `TxBody::WatcherCredit` lets an account write its own watcher work and 168 presence slots (`watcher_credit_ex`), which feeds `epoch_work_from_chain`’s watcher weights (cloned from local `epoch_work`, not from a constrained proof).
- Suggestion: Remove `CreditAsset` from the public tx set (or restrict to a genesis-admin key). Watcher credit only from verified rail/header work, with per-epoch caps already in economy.rs. Never mint native except epoch settlement.
- Status: open

### Issue 5 -- Severity: bug
- File: crates/fd-pl/src/bridge.rs:107
- Description: Rails are not SPV. `submit_header` accepts any parent-linked hash with no PoW, difficulty, or light-client check. `verify_and_mint` uses a protocol `sha256(left|right)` merkle, not Bitcoin/Ethereum merkle, and will accept a proof whose `merkle_root` field matches the computed root even if it disagrees with the stored header (`root != expected && root != hdr.merkle_root`). Combined with unsigned `RailHeader`/`RailDeposit` WireMsgs (and with signed `TxBody::RailHeader` that any account can submit), wrapped BTC/ETH/BNB can be minted from fiction.
- Suggestion: Keep this as a **fixture** behind a `synthetic_rails` flag. Do not document as custodialess SPV until a real BTC header chain + real merkle is verified. Fix the proof check to require `root == hdr.merkle_root` only.
- Status: open

### Issue 6 -- Severity: bug
- File: crates/fd-pl/src/lib.rs:2066
- Description: `soft_votes_needed` still caps by **local pong-fresh** checker count. That is the same class of per-node view that P0 removed from the lottery seed. Two honest nodes on the same tip can compute soft=2 vs soft=3, so one commits while the other still stages. v2.6’s floor `need.max(2)` when `c>=2` avoids the solo-commit case but does not restore agreement. Live fleet is n=3 with soft=3, so this is latent until a pong blip.
- Suggestion: Soft/hard thresholds must be functions of tip-committed state only (bonded n, certified ONLINE size). Pong stays pack-permission, never QC math.
- Status: open

### Issue 7 -- Severity: bug
- File: crates/fd-pl/src/lib.rs:4049
- Description: `verify_ledger_state_root` (and `commit_ledger_body`) set `self.now_ms = ledger.sealed_at_ms` if the packer stamped a future time, then run `maybe_settle_epochs` / unbonds. A packer can jump the clock across epoch boundaries and settle emission early, or complete unbonds before cooldown. Catch-up with `check_lottery=false` (`ProposeLedger{catchup:true}` or `LedgersBatch`) will apply that clock.
- Suggestion: Ignore `sealed_at_ms` for local clocks; use local `now_ms` (or median peer time later). Reject ledgers whose `sealed_at_ms` is outside a small skew of local time. Never settle epochs as a side effect of verifying a proposal.
- Status: open

### Issue 8 -- Severity: bug
- File: crates/fd-pl/src/lib.rs:3810
- Description: `ingest_ledger_catchup` / `accept_ledger_ex(..., check_lottery=false)` commits immediately with no lottery check and no soft QC. The node honors `ProposeLedger.catchup` from **any** peer (falcon_pl_node.rs:1416). A reachable attacker can push a parallel height+1 ledger as catch-up if they also win parent/sig/root checks — easier once Issue 1 lets them swap keys, or if they replay a real packer-signed ledger on a lagged node.
- Suggestion: `catchup:true` only from a pull `LedgersBatch` on a connection you opened, or after `need_hard_resync` armed by operator. Live `ProposeLedger` must always take the stage+soft-QC path.
- Status: open

### Issue 9 -- Severity: suggestion
- File: falcon-dag-rs/Cargo.toml:9
- Description: Versioning is inconsistent: workspace crate `2.3.6`, `PRODUCT_VERSION` `2.6.0`, AMD log jumps 2.3.5 → 2.6.0, code comments refer to 2.3.7 / 2.4.0 / 2.5.0 / 2.6.0, docs still describe “2.2.x private → 2.3.0 public”. `params_version` is still `2`. Operators cannot tell which binary is on which host; mixing 2.2.7 and 2.6 on one mesh is an already-documented fork class.
- Suggestion: One number: bump workspace to 2.6.0, print it in status and logs, add AMD rows for the undocumented 2.4/2.5 ONLINE + view-change work, and refuse Hello from a different `product_version`.
- Status: open

### Issue 10 -- Severity: suggestion
- File: crates/fd-pl/src/lib.rs:1
- Description: Consensus, execution, mempool, lottery, wire types, and ~65 tests live in a 7400-line `lib.rs`. The last 13 AMDs are all patches to this file. That is why freezes keep regressing (seq-poison, tip_lag, skip reseeds, Hello size, catch-up from=1). Hard to review, hard to test one rule without pulling the world.
- Suggestion: After the current soak, split `lottery`, `qc`, `mempool`, `wire`, `execute` into modules with the AMD rules as unit tests next to each. Do not do this mid-liveness dig.
- Status: open

### Issue 11 -- Severity: suggestion
- File: crates/fd-pl/src/lib.rs:2816
- Description: Lottery pick uses `f64` weights and `r <= 0.0`. Same binary/arch it is deterministic; mixed hosts (or a later integer bond that does not convert cleanly) can disagree on the winner. Bond is `u64` already.
- Suggestion: Integer pick: `seed % total_bond` over sorted `(id, bond)` with checked u128. Drop f64.
- Status: open

### Issue 12 -- Severity: suggestion
- File: crates/fd-pl/src/crypto.rs:279
- Description: Secrets are plaintext hex JSON in `--keys-dir` (`{id}.json` mode 0600). Filename is the raw id — `../` in an id writes outside the dir. Fine for a private fleet with 1024 pre-shared wallets; not a wallet product. HMAC-dev secrets are `pl-secret:{id}` and must never be a fleet default (live fleet is correctly on falcon-512).
- Suggestion: Refuse `hmac-dev` unless `--allow-dev-crypto`. Sanitize id for path characters. Do not check secrets into images; the current `keys/` dir on host1 is a live secret store.
- Status: open

### Issue 13 -- Severity: nit
- File: crates/fd-pl/src/crypto.rs:217
- Description: `verify_with_public` for HMAC always returns `false` (`use verify_hmac_id`). Dead path; confusing if a future caller uses it for packer HMAC verify.
- Suggestion: Implement HMAC verify here or `compile_error` / `unreachable` with a comment pointing at `verify_hmac_id`.
- Status: open

### Issue 14 -- Severity: nit
- File: crates/fd-pl/src/bin/falcon_pl_node.rs:1544
- Description: Live logs spam `votes on … soft=true hard=true tip 49→49` many times per hash (self + peer re-fan). Matches the “tip 42→42” pattern seen during this run. Makes freeze forensics noisy and likely contributes to queue pressure (they already rate-limited skip re-fan for this reason).
- Suggestion: Log only on `fresh && (soft||hard) && tip changed`, or first transition to soft/hard.
- Status: open

## Suggested order while you are “just testing” v2.6

1. Keep the current liveness soak (tip agree, no freeze under 25–80 tx/s, join/exit, snapshot restart). That is the right test for AMD-20260813-2.
2. Treat Issues 1–3 as **P0 before any public port or extra untrusted joiner**. They are independent of TPS.
3. Do not run epoch payday / rail / AMM soaks as “economic proof” until 3–5 are closed — those paths still lie locally.
4. Align versions (Issue 9) on the next fleet restart so host1/host2/droplet cannot mix 2.3.x and 2.6.0.

*Review of host1 live tree · 2026-08-13*
