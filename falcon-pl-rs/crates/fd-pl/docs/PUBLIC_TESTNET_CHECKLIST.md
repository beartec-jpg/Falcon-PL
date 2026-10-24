# Falcon PL — Public testnet checklist

| | |
|--|--|
| **Purpose** | Numbered work that must be **done** before a public testnet cut |
| **Current private line** | v2.7.0 on host1+host2 (Phase 1 safety + fresh-genesis soak) |
| **Docs plan name** | Historical docs still say “v2.3.0 public”. Use **one product version** at cut (likely 2.7+ after this list). Do not ship mixed 2.3.6 crate / 2.6.0 product. |
| **Private network_id** | 2200 |
| **Public network_id** | New (plan: **2300**) — never reuse 2200 or Falcon Ledger 1001 |
| **Source review** | [REVIEW_v2.6.0.md](REVIEW_v2.6.0.md) |
| **Date** | 2026-08-13 (gates updated **2026-08-15**) |
| **Private proof cut** | **2.9.26** on 2200 — 8h soak, live join-snap, v12 bond-in, one-val/2-core |
| **Ceremony pack** | [CEREMONY_2300.md](CEREMONY_2300.md) · [PUBLIC_PARAMS_2300.md](PUBLIC_PARAMS_2300.md) · [HARDWARE.md](HARDWARE.md) · `scripts/pl-start-public-2300.sh` |

Status key: **OPEN** = blocks public · **PRIVATE-OK** = fine on Tailscale-only fleet · **AFTER-CUT** = can wait until public is running · **DONE** = proven on 2200 2.9.26 (2026-08-15).

This is **not** a mainnet list. Mainnet is out of scope (EXPANSION_PLAN §9).

---

## A. Safety — nobody untrusted can break the chain

These are review issues 1–3, 6–8. Public RPC or an extra joiner without them is unsafe.

1. **Stop overwriting account keys on SubmitTx / Hello.** Verify against the existing KeyRing / bonded registry. First-seen keys only via an authenticated join or bond tx. (Review #1)
2. **Sign every consensus vote** (`LedgerOk`, `SkipPacker`, `ViewChange`, `OnlineSetVote`) with Falcon-512 over a domain-separated payload. Reject empty, unknown, jailed, or unbonded voters. Voter id must match the key. (Review #2)
3. **Close side-channel state mutation.** Public listen port may only take Hello / Pong / Need* / SubmitTx / ProposeLedger / signed votes / Status / AccountQuery. Vault, claim, pool, swap, bond, mint, watcher credit, tick, settle, rails, snapshot load, hard-reset, SealNow must not apply from a raw WireMsg. (Review #3)
4. **Operator-only admin channel.** `OperatorHardReset`, `SnapshotLoad`/`Save` on a unix socket or a pre-shared operator key — not the mesh listen port. (Review #3)
5. **Soft / hard QC from tip-committed state only.** Drop the local pong-fresh cap in `soft_votes_needed`. Pong is pack permission, never QC math. (Review #6)
6. **Ignore packer `sealed_at_ms` for the local clock.** Do not settle epochs or complete unbonds because a proposal stamped a future time. (Review #7)
7. **`ProposeLedger.catchup=true` only on a pull you opened** (`LedgersBatch`) or after an armed operator hard-reset. Live gossip always stages and waits for soft QC. (Review #8)
8. **Refuse `hmac-dev` on public binaries** unless `--allow-dev-crypto`. Public fleet is Falcon-512 only. (Review #12)
9. **Integer lottery pick** (`seed % total_bond` over sorted bonds). No `f64` winner. (Review #11)
10. **Version handshake.** Hello carries `product_version` + `network_id`; reject mixed crates (today 2.3.6 vs 2.6.0 is already a fork class). (Review #9)

## B. Money — public cannot inflate or steal FPL

11. **Remove or admin-gate `CreditAsset`.** **DONE 2.9.30:** SubmitTx + apply + native `credit_asset_admin` require `allow_admin_wire` (off with `--lock-public-wire`). Public mint path = epoch settlement only. (Review #4)
12. **Watcher credit only from verified work.** **DONE 2.9.30:** `TxBody::WatcherCredit` and WireMsg always `disabled`. Presence = `WatcherHeartbeat`. Work = rail header/deposit. Cap unchanged. (Review #4)
13. **All economy ops as sequenced signed txs** that every honest node applies in the same ledger: bond, unbond, claim, vault, AMM, lend. No local-only apply. (Review #3)
14. **Treasury / faucet / builder keys** generated for public genesis; never reuse private-fleet or Falcon Ledger keys. Faucet pot is the 1B genesis bucket only.
15. **Unbond cooldown = 14 days** on public (private 1-day flag off). Min bond 1,000 FPL.
16. **Fee burn 50% + escalation table** frozen on the public parameter sheet (`params_version` bumped). No silent retune after go-live without AMD + notice.
17. **Do not expose rails as “custodialess SPV”** until a real BTC header + real merkle path is verified. Until then: disable public mint/burn, or ship rails behind `synthetic_rails=false` default. (Review #5)
18. **If any rail ships:** proof must match the **stored header** merkle root only; deposits unique by external txid; withdraw notes not replayable.

## C. Liveness — private soak must actually pass

Do not cut public while v2.6 is still “just testing.”

19. **Close the tip=49 / empty-mempool stall class** (or prove it was only a drained inject window) with a written dig.
20. **Multi-host tip agree under load** on host1 + host2 + droplet: same tip **and** same hash for a soak (hours, not minutes). No sustained fork.
21. **Join / exit / bond** a fourth validator under load; tip continues; joiner catch-up does not wipe genesis.
22. **Snapshot restart:** kill a hub, restore snapshot, rejoin, same tip/hash, no hard-resync storm.
23. **Progress timeout / skip QC:** kill the lottery winner; next preselected seat seals; no lottery desync.
24. **View-change** recovers a stuck pending / poison height without operator hard-reset.
25. **Steady 20–80 tx/s** keep-up on ≥3 hubs; document mean tip interval and mempool.
26. **Spike / soft wall** (150–500) does not OOM (`max_chain_ledgers` + snapshots on). Resource wall is acceptable; consensus lie is not.
27. **Epoch boundary payday** on multi-host: identical `claimable` / state root on every validator after settle (the dual-host divergence class stays dead).
28. **Adversarial local:** double-sign → slash + jail; unsigned / wrong-key vote rejected (after #2).

## D. Freeze the public product (ceremony)

29. **One version number** on crate, `PRODUCT_VERSION`, AMD log, and binaries. Backfill AMD rows for 2.4 ONLINE + 2.5 view-change + 2.6 liveness.
30. **Publish a public parameter sheet** (copy of EXPANSION_PLAN §3–§5 + surviving AMDs). Soft=3 static, hard=0.6, pack 1000 ms, progress timeout, mempool 50_000, emission 30 bps, 7d epoch, bootstrap epochs 1–7.
31. **New `network_id` (2300)** and a **clean genesis**. Do not copy private 2200 history or Falcon Ledger 1001 balances.
32. **Genesis buckets:** 196B keyless treasury, 2B community, 1B faucet, 1B builder — as locked. Record genesis hashes and validator publics.
33. **≥3 independent hubs**, ≥3 bonded genesis validators (prefer 5), ≥1 RPC, ≥1 snapshot/archival node. Public listen not bound to `0.0.0.0` without a firewall.
34. **Bootstrap peer list + status/metrics** (tip, peers, epoch, faucet balance, `params_version`).
35. **Public RPC / faucet / wallet** pointed at FPL 2300 only. Falcon-512 keys. Pay + claim at minimum.
36. **Docs for outsiders:** IDENTITY, this checklist, parameter sheet, endpoints, “not mainnet / not a BFT proof.”
37. **Retire Falcon Ledger v1 public testnet** (announce, stop FL faucet/relays, archive endpoints). No auto-migrate of FL balances.
38. **Announce window** with a go-live date. Safety patches may be immediate; Class B money-rule changes get ≥1 epoch notice.

## E. Explicitly not required for first public testnet

Leave these off the cut, or ship as off-by-default:

- Real ETH / BNB / XRP / USDC rails
- Dynamic fee burn, RBF, perf-weighted lottery
- On-chain governance / token vote
- Production DHT gossip (JSON-TCP + persistent peers is acceptable if #1–#8 hold)
- Splitting `lib.rs` (do after soak, not as a cut blocker)
- Mainnet branding

---

## Suggested sequence

1. Finish **C** on the current private v2.6 soak (you are here).
2. Land **A** then **B** on private; re-run C (join/exit, payday, adversarial).
3. Do **D** as a new genesis (2300). Never “flip 2200 public.”

A public testnet that skips A is a public griefing target. A public testnet that skips C will freeze on day one. A public testnet that skips D will mix histories and versions.
