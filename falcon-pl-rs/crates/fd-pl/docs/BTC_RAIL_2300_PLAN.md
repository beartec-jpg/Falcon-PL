# Falcon PL 2300 — fungible protocol-sealed BTC rail (e2e)

> **HISTORICAL PLAN (pre-e2e).** Operator dest-lock mint + Kickoff + take **passed 2026-08-21**. Wallet `BTC_RAIL_LIVE = true` as of 2026-08-25. **Do not use the “Not built” line below as live status.** Current status: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md). Kickoff is still t-of-n FROST.

| | |
|--|--|
| **Network** | Falcon PL `network_id` **2300** only |
| **Not** | Committee Bitcoin keys · business / EOA wallet · COMPLETE in a json file · one-way notes · “only your deposit UTXO out” |
| **Status (this draft)** | Written before e2e. **Superseded.** |
| **Invariant** | FBTC is **fungible**. Sally 1 in / Dave 2 in → AMM to 1.5 / 1.5 → both bridge out. |

No committee. No operator key ring. Same idea as the old “keyless hold”: **the protocol** can spend the vault; **no person** is given a key.

I will **not** say this is finished until PR8 is green **and I have run the e2e**. Do not test. Do not send BTC until I give a **new** address after that.

---

## FBTC is the withdraw right

If you hold FBTC, you can burn it and take that much BTC. **You do not get a second “bridge key.”** Your Falcon account key + the balance is the authorization. After AMM, Sally’s 1.5 FBTC is as good as Dave’s original 2.0 for exit.

That is the *who may leave* rule. Bitcoin still cannot see an FPL burn, so the vault spend is automatic inside `apply(RailWithdraw)` once that burn is committed — FROST shares on the **nodes**, not a key we hand to Sally or a committee that votes.

---

## The Sally / Dave rule

| | BTC in | After AMM / lend | BTC out |
|--|--------|------------------|---------|
| Sally | 1.0 | 1.5 FBTC | **1.5** |
| Dave | 2.0 | 1.5 FBTC | **1.5** |
| Vault | 3.0 | 3.0 FBTC | 0 |

FBTC is a normal balance. Lend and AMM do not talk to Bitcoin. Bridge Out burns **whatever FBTC you hold** and pays that many sats from the **vault UTXO set**.

A lock that only the original depositor can spend **cannot** do this. Committee 4-of-6 Bitcoin keys are also **out** — that is still people.

---

## The disk-secret problem (this is real)

A **full** Bitcoin spend secret on **every** node is not non-custodial. It is worse than a 4-of-6:

| Who has what | Disks needed to steal the vault |
|--------------|----------------------------------|
| One protocol key copied in every snapshot (last draft) | **1** — any validator, any backup |
| COMPLETE in `protocol-reserve.json` (old bug) | **1** — anyone with the file |
| Human committee “please sign” | People, and they can refuse |
| **FROST shares: one share per seat, never the whole key on disk** | **t of n seats** (same *t* as consensus, e.g. 4-of-6). Automatic in `apply`. No meeting. |
| BitVM / CTV (no spend key exists) | 0 keys; steal is a fraud you can challenge. Not this cut. |

Bitcoin cannot see an FPL burn. For Sally to take **Dave’s** coins after the AMM, *something* must be allowed to spend the vault to an arbitrary dest. That something is either:

1. a key (or secret), or  
2. a Bitcoin covenant / BitVM (no production-ready path I will ship as “done” in one cut — that is how you got stuck).

Hiding the same key “inside FPL” does **not** fix (1). `grep` on a snapshot still finds it. We will **not** ship that.

**What we will ship instead (still not a business, still not a human committee):**

- At 2300 genesis, create a vault pubkey with **FROST**. Each bonded seat writes **only its share** to a local `600` file **outside** the published snapshot (`keys/vault-share-<id>`, never RPC, never git).
- The full secret is **never** written to disk. It exists only in RAM for milliseconds inside `apply(RailWithdraw)` after **t** shares are combined, then zeroed.
- Signing is automatic when the ledger already committed dest + amount + change-to-same-vault. No operator clicks. No “committee vote.”
- One stolen laptop **cannot** drain the vault. *t* stolen seats can — that is the same collusion that can already fork 2300. We are not adding a new class of people.
- User/watcher only broadcasts the finished Bitcoin tx.

This is “no keys” in the product sense: **you** never hold a bridge key, **ops** never get a COMPLETE file, **relays** cannot steal. It is not “zero secret exists on the fleet.” That sentence is false for any fungible wrap on today’s Bitcoin.

BitVM as `AMD-BTC-2300-later` can delete the shares. Not required for first e2e; first e2e **must** still pass Sally/Dave and must **fail** if a single share can sign alone.

---

## Network expansion (new vals)

The Bitcoin vault **address does not change** when you add a validator. All UTXOs stay where they are. A new key or a new taproot would force a sweep and that is how coins get stuck.

The rotating **4-of-6 pack committee** is the wrong set for FROST. That committee changes every height. You cannot reshare every ledger.

Shares follow the **bonded, non-jailed** set — the same set as **hard QC** (`ceil(n × 0.6)` on 2300).

| Event | What happens | Bitcoin address | Old shares |
|-------|----------------|-----------------|------------|
| Genesis | DKG among genesis bonded seats. Each writes one local share. | Vault P2TR created | — |
| New val **Bond** completes | **Reshare** (same secret, new polynomial). Newcomer gets a share. Everyone else gets a *new* share. `t = ceil(n × 0.6)` with the new *n*. | **Same** | **Dead.** Stolen disk of an old share cannot combine with the new epoch. |
| **CompleteUnbond** | Reshare *without* the leaver. Their file is useless after the epoch bump. Do this at the end of the 14-day cooldown, in the same ledger as unbond complete. | Same | Leaver’s share dead |
| **Jail** | Same as kick: reshare excluding them, or they keep a live share on a box you no longer trust. | Same | Jailed share dead |
| Joiner from **snapshot** | Snapshot has **no** share. They run DKG refresh with live peers. Until that finishes they pack/check as usual but **do not** contribute a vault share. Peg-out still works if `t` of the *current* share-holders are online. | Same | Unchanged |

Never:

- Copy a share onto the new machine (that is cloning, not joining).
- Put shares in the published snapshot (every joiner would get one; 1-disk steal returns).
- Generate a **new** vault pubkey on expansion (old UTXOs would need a sweep).
- Wait for a human to “approve” the new val’s share. Bond on-ledger **is** the trigger; DKG is automatic.

Liveness after growth: more bonded vals ⇒ higher `t` ⇒ more machines must be up to peg-out. That matches hard QC. If too many are down, Bridge Out waits; BTC stays in the vault; FBTC stays burned only after a successful sign (order: build tx → collect `t` shares in apply → then burn, or: burn and leave a “needs signature” note that can be completed when `t` are back — **v1 = do not burn until `t` shares signed**, so a down fleet cannot eat FBTC).

PR8 must include: start with 5 bonded, Sally/Dave in, add a 6th val, reshare, **old-share file fails to sign**, 6th val’s share participates, one withdraw still pays.

---

## Why coins got stuck last time

1. Peg-out was a note, or the script changed under deposits.  
2. Headers pruned.  
3. Secret lived in a public file.  
4. Must not add: per-user UTXO out (breaks Sally 1.5) or committee keys.

Rules:

- Fungible FBTC. No per-account `lock_id` on withdraw.  
- Vault script + pubkey frozen in `AMD-BTC-2300-1` before first deposit.  
- Header archive for every height that still has a vault UTXO.  
- No script change while `vault_sats > 0` unless the binary still spends the old version.  
- Phase-1 park: user refunds themselves. Unminted phase-2: FROST-sign a refund to the sender (same apply-only path, no full key on disk).  
- Old `tb1q7dnl…` / 1001 UTXOs stay lost. New vault address.

---

## Flows

### Peg-in

**Phase 1 — park**  
`Script_A`: user’s Bitcoin key + `CSV_PARK`. Abort without minting = they take it back. No protocol secret involved.

**Phase 2 — vault** (only if `spv=bitcoin` and lag < 20)

1. `RailLockPrepare` → `{ deposit_id, vault_address }` (address only, **no secret**).  
2. Wallet pays **frozen vault** + `FALC‖dest20`.  
3. 6 confs + headers.  
4. `RailDeposit` SPV-verifies, mints **fungible** FBTC, appends UTXO to `vault_utxos`.

No mint: `RailLockRefund` → node signs that UTXO back to the sender → user broadcasts.

### AMM / lend

Unchanged. FBTC is `defi` BTC.

### Peg-out (any amount ≤ balance)

1. Wallet asks for a withdraw quote (coin-select + fee).  
2. Builds the **unsigned** Bitcoin tx: inputs = vault UTXOs, out1 = user dest, out2 = change to **same** vault (if ≥ dust).  
3. User signs `RailWithdraw { amount, external_to, raw_unsigned_btc_tx }`.  
4. Apply: burn FBTC; check dest/amount/change/inputs; **protocol secret signs SIGHASH_ALL**; signed tx stored on the withdraw note; secret **not** stored on the note.  
5. Wallet (or watcher) broadcasts. Retry does not burn again.  
6. Bitcoin conf → mark inputs spent, add change UTXO.

**Sally 1.5 / Dave 1.5:** spend Dave’s 2.0 → 1.5 Sally + 0.5 change; then spend 1.0+0.5 → 1.5 Dave. Vault empty.

Fee: from vault, min withdraw 10k sats, fee cap in AMD. FBTC burned = amount received, not the fee.

---

## Headers

Checkpointed testnet hash. Real bits. **No reanchor.** Keep headers for every vault UTXO height. Submitter systemd + lag alert. Deposit refused if header missing.

---

## PR plan (no live BTC until PR8)

### PR1 — FROST vault shares + UTXO set

Genesis: FROST keygen among 2300 seats. **Share per bonded seat**, local file only, **not** in snapshot JSON. Bond/unbond/jail **reshare** same pubkey (`t = ceil(n × 0.6)`). Test: `< t` cannot sign; old share epoch fails after reshare; snapshot/RPC have no full secret; Sally/Dave 1.5/1.5 math.

### PR2 — Park script + vault P2TR/P2WPKH template

Rust + TS same bytes. AMD lists **pubkey and address only**.

### PR3 — Header archive + checkpoint

### PR4 — Peg-in apply

SPV to vault address, fungible mint, UTXO list. Refund apply uses FROST sign (`t` shares, in RAM only).

### PR5 — Peg-out apply + sealed sign

Burn any amount. Sign only the committed tx. Tests: wrong dest refused; secret absent from withdraw JSON; second withdraw cannot reuse spent outpoints.

### PR6 — Wallet

Phase 1/2 in. AMM/lend as now. Bridge Out: unsigned tx → withdraw → broadcast signed tx from node. **Disabled** until PR8.

### PR7 — systemd header submitter + optional broadcaster

Broadcasts the signed tx from the ledger. Holds no key.

### PR8 — My e2e

1. Phase-1 CSV refund.  
2. Sally 1.0 + Dave 2.0 in (testnet sats, same ratio).  
3. Swap to 1.5 / 1.5 FBTC.  
4. Both bridge out. Vault 0. FBTC 0.  
5. Submitter down → new phase 2 refused; in-flight still mints after restart.  
6. Snapshot restart between the two outs.  
7. Upgrade without script change; leftover change UTXO still spends.  
8. Status/RPC dump contains **zero** vault secret bytes.  
9. Mutated dest does not get a signature.  
10. Broadcast retry, no double-burn.  
11. Add a 6th val, reshare: leftover pre-reshare share file cannot sign; withdraw still works with the new set.

Only then the BTC tab turns on. Then you may send to the **wallet-shown** address I specify.

---

## Your BTC

Not now. After PR8: address, amount, version. Until then, send nothing.

---

## Definition of done

- [ ] Sally/Dave 1.5/1.5 e2e I ran myself  
- [ ] No EOA, no COMPLETE file, no **full** vault secret on any one disk; `< t` shares cannot sign  
- [ ] Phase-1 self-refund + unminted phase-2 protocol refund  
- [ ] Headers for vault UTXOs kept  
- [ ] I wrote “e2e passed” with logs  

Until then the BTC tab stays **disabled**.
