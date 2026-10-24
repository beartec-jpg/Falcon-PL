# Falcon PL — BitVM2 Engine (`fd-pl-bitvm2` & `BTC4` Live Integration)

**Status:** **LIVE on Testnet 2300 (`BTC4` / Bitcoin Testnet4) since height `692,000`** (`TESTNET_2300_BRIDGE_FROM_HEIGHT = 692_000`, `TESTNET_2300_BITVM2_M1_FROM_HEIGHT = 692_000`, `5,000 FPL` operator bond, `Bitvm2AttestV2`).  
**Companion Docs:** [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md), [docs/bridge/BRIDGE_INTEGRATION.md](../../../../docs/bridge/BRIDGE_INTEGRATION.md), [docs/bridge/OPERATOR_GUIDE.md](../../../../docs/bridge/OPERATOR_GUIDE.md), and [docs/bridge/OPERATOR_REWARDS.md](../../../../docs/bridge/OPERATOR_REWARDS.md).  
**Legacy Testnet3 (`BTC`):** Frozen since 16 Oct 2026 ([BTC_POOL_FREEZE_2026-10-16.md](BTC_POOL_FREEZE_2026-10-16.md)).

This document freezes roles, the program `f`, timeouts, slash conditions, and the v1 operator model so Phase B+ can implement without re-litigating product intent.

---

## 1. Goal

Ship a **BitVM2 engine** so BTC peg-out authorization on Bitcoin does **not** depend on a single operator watch key (`keys/bitvm-watch.secret`).

| Property | Target |
|----------|--------|
| Safety | Existential honesty at **setup** (1-of-n setup signers) + ≥1 **rational challenger** |
| Liveness | ≥1 **rational operator** who fronts and claims |
| Peg-in | Unchanged: BTC header SPV + FALC (already strong) |
| Marketing | “Auditable BitVM2 testnet” — **not** “custodialess” until e2e Disprove is live and reviewed |

Reference design: BitVM2 / BitVM Bridge (Linus et al.) — optimistic SNARK verification on Bitcoin via Claim → Challenge → Assert → Disprove, **no consensus change**.

---

## 2. Non-claims (explicit)

Do **not** describe the following as BitVM2:

- Current NUMS Kickoff requiring **dest BIP340 + watch BIP340**
- FPL-side `kickoff_ok` / `RailChallenge` alone
- SHA256-step “disprove” leaf that does not verify a Groth16 trace
- Shared `bitvm-abort-seed` abort path

Those remain **legacy watch co-sign + dest-lock claim vault** ([BITVM_VAULT_2300.md](BITVM_VAULT_2300.md) — historical name; treat as legacy).

---

## 3. Operator model (locked for v1): **A1 — operator fronting**

**Decision (locked):** **Kickoff dest-lock, setup-once Challenge.** No operator fronting. No live n-of-n on the user’s unwrap. Setup n-of-n **presigns Challenge/abort once** (Assert connector or return-to-instance), then secrets are deleted. Claimer Kickoff dest-locks to the user; dest-lock CSV **starts the clock**. User takes with their key. A1 dave-fronting is retired. Per-exit n-of-n is retired.

```
User burns FBTC on FPL
        │
        ▼
Operator pays user on Bitcoin (fronting / inventory)
        │
        ▼
Operator posts BitVM2 Claim:
  “this payout matches a valid FPL withdraw”
        │
        ├── no successful challenge in Δ → PayoutOptimistic (reimburse operator from instance)
        └── challenge → Assert → Disprove (slash / forfeit) OR claim stands
```

**Why A1 first**

- Matches published BitVM Bridge flow (operator inventory + reimbursement).
- Avoids redesigning the shared NUMS pool spend graph in the first cut.
- Legacy pool + watch Kickoff can keep serving until cutover flag flips.
- A2 (direct pool under BitVM2) can reuse the same circuit `f` later.

**Operator inventory:** testnet BTC held by operator(s); fee model TBD in Phase D (not blocking Phase A).

---

## 4. Roles

| Role | Who | Duty |
|------|-----|------|
| **User** | FBTC holder | Burn on FPL; receive BTC from operator; may Challenge |
| **Operator** | Permissioned set (start: ops + bonded seats) | Front BTC; post Claim; respond to Assert if challenged |
| **Challenger** | **Anyone** (permissionless) | Recompute `f`; Challenge bad Claims; Disprove faulty Assert |
| **Setup signers** | Committee of `n` (start: bonded FPL seats + ops) | One-time presign of dispute tx graph for an instance; **1-of-n honest** assumed (no covenants on Bitcoin yet) |
| **FPL** | Network 2300 | Source of burns / `claim_root` / finalized headers — **witness for the SNARK**, not the Bitcoin spend key |

---

## 5. Program `f` — `FplWithdrawValid`

Circuit proves (informally):

> There exists a valid Falcon PL withdraw note such that `amount` and Bitcoin `dest` match the operator’s peg-out payment, the note is in a finalized `claim_root` at height `H` on network 2300, and the note is not double-spent under FPL rules.

### 5.1 Public inputs (ABI — freeze for Phase B)

Version tag: `fpl-bitvm2-wd-v1`

| Field | Meaning |
|-------|---------|
| `network_id` | `2300` |
| `amount` | sats paid / burned |
| `dest_commitment` | commitment to user Bitcoin payment identity (exact encoding Phase B) |
| `claim_root` | FPL claim merkle root at `height` |
| `height` | FPL ledger height |
| `header_hash` | FPL header hash at `height` (or LC anchor — Phase B picks one policy) |
| `note_id` | withdraw note id |
| `pegout_txid` | Bitcoin txid of operator fronting payment (or hash thereof) |
| `vk_hash` | hash of Groth16 verifying key / circuit version |


### 5.1a M1 decisions (activation-gated; `bitvm2_m1_from_height`)

**Claim root: MiMC (decided).** The withdraw circuit's `claim_root` public input is the
MiMC-BN254 claim tree (`bitvm2_claim_leaves` / `bitvm2_claim_root`). Reasons: MiMC is
circuit-native (a few hundred constraints per hash vs ~27k per SHA-256 compression), it is
already the tree the rail maintains and the prover uses, and moving to SHA-256 in-circuit
would multiply proving cost and size for no security gain inside a BN254 SNARK. The legacy
SHA-256 `claim_root` stays only for the vault/watch path and is **not** a circuit input.

**FPL header binding.** `fpl_header_hash` is no longer `header_hash_placeholder`. From M1 it is
`checkpoint_header_hash(h, H) = SHA256("fpl-bitvm2-ckpt-v2|" || h_le || lowercase(H))`, where
`(h, H)` is a consensus checkpoint stored in BTC rail state (`fpl_checkpoints`): every ledger
whose height is a multiple of `BITVM2_CHECKPOINT_INTERVAL` (64) records its parent `(h-1, hash)`;
the last `BITVM2_CHECKPOINT_KEEP` (1024) are kept. A checkpoint is usable only once
`BITVM2_CHECKPOINT_FINALITY` (2) further ledgers have been applied, and only if
`h >= burn open height`. Validators check against rail state, never their own tip or disk.

**BTC payout inclusion.** `TxBody::Bitvm2AttestV2` carries the raw peg-out tx, its BTC block
height and a merkle path. Consensus requires: header present on the BTC header rail with raw
bytes, ≥ `BITVM2_PEGOUT_MIN_CONFS` (6) confirmations, merkle proof to that header's root, an
output paying exactly the burned amount to the destination spk (or the dest-lock), and the
peg-out txid unused by any other claim. The Groth16 proof must verify under the fixed
`BITVM2_WITHDRAW_VK_HEX`.

**VK.** Pre-M1 verification ran a fresh random setup per call, so no proof could ever verify
and Disprove fired on any failing submission. M1 verifies with a single fixed VK and Disprove
re-verifies the *stored* proof. The VK must come from a multi-party ceremony; a single-party
setup lets its holder forge attestations (i.e. it would be custody). On Testnet 2300, `BITVM2_WITHDRAW_VK_HEX` is pinned to the ceremony verifying key (`sha256 = 4a1cabbc…4763`) and activated at height `692,000` (`TESTNET_2300_BITVM2_M1_FROM_HEIGHT = 692_000`). `setup_from_seed` is restricted to dev/test.

This is not a non-custodial claim: the peg-in pool remains custodial and frozen (see
`BTC_POOL_FREEZE_2026-10-16.md`).

### 5.2 Private witness (sketch)

- Withdraw leaf `SHA256("fpl-bitvm-wd-v1|" \|\| note_id \|\| dest_h160 \|\| amount)` (align with existing leaf domain where possible)
- Merkle path leaf → `claim_root`
- Evidence that `height` is finalized under FPL rules (checkpoint + header/OK policy — **must be explicit in Phase B**; FPL is not a Bitcoin rollup)
- Binding between `pegout_txid` outputs and `dest`/`amount`

### 5.3 Reject

Wrong dest, wrong amount, forged root, replayed note, non-final height, mismatched peg-out payment.

### 5.4 SNARK stack

Reuse Falcon Groth16 tooling (`fd-pl-snark` MiMC + BN254 Groth16). **Testnet** may use test-seed setup; mainnet requires a real ceremony (same class of honesty as FalconQc today).

**Phase B note:** Membership is proven against a **MiMC** claim tree over the withdraw statement (`fpl-bitvm2-wd-v1`), not the live SHA256 `claim_root` in `bitvm.rs`. Same semantic fields (note, dest20, amount). Phase D either publishes the MiMC root on FPL or adds SHA256 gadgets to match the legacy root.

---

## 6. BitVM2 on-Bitcoin surface (engine)

Per BitVM2 optimistic flow (names may match implementation crates):

| Tx / step | Purpose |
|-----------|---------|
| **Setup** | `n` signers presign Claim / Challenge / Assert / Disprove / PayoutOptimistic graph for instance `I` |
| **Fund** | Lock testnet BTC into instance `I` (reimbursement pool) |
| **Claim** | Operator asserts knowledge of valid `(z, o)` for `f` (SNARK + public inputs) |
| **Challenge** | Anyone disputes within window Δ |
| **Assert** | Operator reveals verifier intermediate states (`VerifyTrace` chunks) |
| **Disprove** | Challenger executes one failing chunk on-chain → Claim fails / bond forfeit |
| **PayoutOptimistic** | If no successful dispute in Δ, operator reimbursed |

**Engine crate:** `falcon-pl-rs/crates/fd-pl-bitvm2/`

**C.1a (shipped):** scriptable chunks use `OP_SHA256` inequality leaves in a BIP341 taptree (`build_disprove_taptree`).  
**C.1b (shipped):** Groth16/BN254 chunk wired to BitVM pack bridge (`verifier_split::bn254_pack`) — pinned API constants + Disprove pack-bind leaf; upstream tree at `vendor/BitVM` (fetch via `scripts/fetch-bitvm.sh`). Full ~1GB ScriptBuf blobs are **not** committed; optional digest cache via `tools/bitvm-c1b-export`.  
**D.1 (shipped, then tightened):** instance is **payout + setup n-of-n only** (C.1a SHA256-inequality leaves removed — they were anyone-spend). Assert connector = operator CSV recover (no `OP_TRUE`) + setup n-of-n + 256 Lamport equivocation leaves. `setup-1` is root-only (`/root/falcon-bitvm2-setup/setup-1.privhex`). Kickoff frozen via `FPL_KICKOFF_FROZEN=1`. Groth16-in-script Disprove is still BitVM pack (not live).

Legacy `fd-pl/src/bitvm.rs` remains the **CSV dest-lock claim helper** for the watch path until cutover.

---

## 7. Timeouts (testnet defaults — tunable)

| Parameter | Testnet v1 (demo) | Mainnet-class (later) |
|-----------|-------------------|------------------------|
| Challenge window Δ | **24h** (may shorten to 2–6h for demos with flag) | ~7 days (BitVM2-class) |
| Assert response | **12h** | multi-day |
| Disprove grace | **12h** | multi-day |
| FPL finality for witness | Existing 4-of-6 tip policy + explicit circuit checkpoint | Same, hardened |

Exact block counts are implementation details; **wall-clock Δ above is the product default** for Phase C scripts.

---

## 8. Bonds / slash (v1)

| Event | Effect |
|-------|--------|
| Honest Claim, no Challenge | Operator reimbursed from instance after Δ |
| Successful Disprove | Operator Claim fails; bonded/locked value for that Claim path **forfeited** to disprove economics (exact split Phase D) |
| Operator no-shows Assert | Challenger wins by timeout rules (Phase C) |
| Setup signer malice | Mitigated only by **1-of-n honest** setup — publish this on the website |

User BTC from fronting is already received before Claim; user is not waiting on BitVM for funds under A1 (user waits on operator fronting latency).

---

## 9. Cutover flags (Phase D)

| Flag / mode | Behavior |
|-------------|----------|
| `BTC_EXIT_MODE=watch` | **Default today** — dest + watch Kickoff |
| `BTC_EXIT_MODE=bitvm2` | Operator fronting + BitVM2 Claim; watch Kickoff disabled for **new** exits. Reimbursement from the D.1 instance (CSV + operator), not the watch pool. |
| Dual-run | Allowed on testnet: read-only Challenger against bitvm2 while watch still serves users |

---

## 10. Website / auditability requirements (Phase E preview)

Live test page must show:

1. Bitcoin testnet instance funding address / outpoint  
2. Challenge window Δ and clock  
3. Trust line: **1-of-n setup · permissionless challenge · operator fronting**  
4. Link to this doc + [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md)  
5. Explicit: legacy watch Kickoff ≠ BitVM2  

Forbidden copy until Phase C Disprove e2e: “custodialess,” “permissionless bridge,” “Bitcoin verifies Falcon” without the optimistic caveat.

---

## 11. Phase roadmap (engine)

| Phase | Deliverable | Status |
|-------|-------------|--------|
| **A** | This SoT; deprecate false BitVM labels; lock A1 | **Done** |
| **B** | `FplWithdrawValid` circuit + tests (`fd-pl-bitvm2`) | **Done** (2026-08-27) — MiMC claim tree; see crate README |
| **C** | BitVM2-core optimistic engine + emulator e2e | **Done** |
| **C.1** | Verifier-split + Disprove taptree | **Done** — C.1a SHA256 leaves; C.1b BitVM BN254 pack bridge + vendor |
| **D** | Operator + challenger sidecars + FPL hooks | **In progress** — `BTC_EXIT_MODE=bitvm2`, operator sidecar, `Bitvm2Attest` tx |
| **D.1** | Instance spend graph (no watch key) | **Done** — NUMS P2TR + CSV operator payout + n-of-n Challenge template |
| **E** | Portal auditable live test | |
| **F** | Harden / ceremony / review | |

Optional parallel: replace watch key with **t-of-n FPL seats** on legacy Kickoff for nearer “protocol-controlled” exits — does not replace this engine.

---

## 12. Acceptance

### Phase A
- [x] Operator model **A1** locked
- [x] Program `f` named and public-input ABI sketched
- [x] Roles, timeouts, slash sketch recorded
- [x] Legacy watch path explicitly **not** BitVM2
- [x] STATUS + docs index point here

### Phase B
- [x] Crate `fd-pl-bitvm2` with `FplWithdrawValid`
- [x] Public ABI frozen (`N_PUBLIC = 13`, domain `fpl-bitvm2-wd-v1`)
- [x] `cargo test -p fd-pl-bitvm2` — honest prove/verify; wrong amount/dest/network rejected; nullifier reuse detected
- [x] Test-seed Groth16 setup only (not a ceremony)

### Phase C
- [x] Protocol state machine: Claim → Challenge → Assert → Disprove / PayoutOptimistic
- [x] Emulator verifies `FplWithdrawValid` off-chain (stand-in for on-chain verifier split)
- [x] E2E tests: honest timeout payout; false claim disproved; valid proof survives challenge
- [x] CLI `falcon-pl-bitvm2-emu` (`honest` / `disprove` / `skeleton`)
- [x] `TestnetInstance` scaffold + `FPL2||digest` commit format

### Phase C.1
- [x] **C.1a** `verifier_split`: hash-linked Assert trace; 6 scriptable SHA256 Disprove leaves; BIP341 taptree root
- [x] Tests: honest trace sound; tampered z_out fraudulent
- [x] **C.1b** BitVM BN254/Groth16 pack bridge (`ChunkBackend::BitvmBn254`); vendor `vendor/BitVM`; pack-bind leaf in taptree; export tool for script digests (blobs not reinvented / not committed)

### Phase D.1
- [x] Instance taptree: `payout_optimistic` (CSV + operator) + `setup_nofn` (≥2) + C.1 disprove leaves
- [x] Assert connector taptree: setup n-of-n + CSV timeout
- [x] NUMS P2TR address (`FalconPL-BitVM2-Instance-2300`) — key-path unspendable
- [x] `sign_payout_optimistic` — operator only, no watch signature
- [x] Challenge template: n-of-n signatures; vout0 **must** be Assert connector
- [x] ctl: `btc-bitvm2-instance` / `btc-bitvm2-payout` / `btc-bitvm2-challenge`
- [x] Setup ceremony: persist presigned Challenge txs; fund instance on testnet (`tb1p30ymr…jlcu6`, tx `f36116a9…`)
- [ ] Winternitz / BitVM pack scripts so Disprove is sound on Bitcoin (not emulator)


