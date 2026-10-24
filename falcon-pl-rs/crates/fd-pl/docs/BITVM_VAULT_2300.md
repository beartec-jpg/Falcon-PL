> **LEGACY exit path (live on 2300).** Name is historical.  
> This is **watch + dest Kickoff**, not BitVM2. Bitcoin does **not** verify FPL `claim_root`.  
> Live status: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md).  
> Next engine: [BITVM2_ENGINE.md](BITVM2_ENGINE.md) (design — operator fronting A1).

# Falcon PL 2300 — legacy BTC exit vault (watch co-sign)

This document describes the **shipped** pool → dest-lock Kickoff used on testnet today.

Not BitVM2. Not custodialess: Kickoff requires the operator **watch** BIP340 (+ dest). FPL-side `kickoff_ok` / `RailChallenge` are protocol aids only.

Not: operator liquidity · COMPLETE in json · dest-free `OP_TRUE` after a public preimage · “reserved challenge”.

## What the old chain was supposed to be

Shared pool. Peg-out pays **from that pool** to the user. A **challenge leaf** stops a bad payout. No federation CHECKSIG on the hold.

What shipped instead (earlier cuts): a COMPLETE preimage in a file, and `kBTC_WITHDRAW_CHALLENGED` left reserved. Current cut: NUMS + watch co-sign (see STATUS).

## Kickoff (watch co-sign — not BitVM2)

Kickoff is **not** a founder / t-of-n Bitcoin signature on the live NUMS leaf (see STATUS for watch x-only).

FPL checks a **program** `kickoff_valid` any watcher can recompute:

1. `leaf = SHA256("fpl-bitvm-wd-v1|" || note_id || dest_h160 || amount)`
2. `leaf` is in FPL `claim_root` (merkle). That root is updated on `RailWithdraw` — a public ledger fact, not a key.
3. Bitcoin `vout0` scriptPubKey **is** the dest-locked claim script for that dest (CSV + dest CHECKSIG / abort-on-challenge).
4. A **single SHA256 step** of that program can be challenged (`challenge_sha_step`). A lie fails on Bitcoin-script-sized work (`OP_SHA256`).

No operator inventory. The pool pays the dest-lock; the user takes with **their** key after CSV. A permissionless `RailChallenge` aborts a Kickoff whose dest/amount ≠ the burn.

The pool address is **NUMS** (`FalconPL-BitVM-Kickoff-NUMS-2300`): no secp private key exists for it. Founder signatures are not part of the spend path.

**Honest label:** the committed **watch key must sign** every Kickoff. A compromised watch key + attacker dest can move the pool without an FPL burn (Bitcoin never sees `claim_root`). FPL `RailChallenge` helps after a bad Kickoff is observed; it does not replace watch custody. True 1-of-n optimistic verification is [BITVM2_ENGINE.md](BITVM2_ENGINE.md), not this vault.

## What this cut implements

```
Pool UTXO (fungible)
    │  Kickoff (only move: pool → dest-locked claim + change-to-pool)
    ▼
Claim UTXO  (per withdraw, script below)
    ├─ IF   abort_preimage     → back to pool     (no CSV; only after FPL accepts Challenge)
    └─ ELSE csv + dest CHECKSIG → user takes BTC  (no protocol secret, no LP)
```

Anyone can watch. A valid FPL `RailChallenge` **publishes** that claim’s abort preimage. Watchers broadcast the abort. The thief’s CSV has not matured yet, so dest CHECKSIG cannot race if CSV ≥ watcher reaction.

Honest path: no challenge → user waits CSV → user signs with **their** Bitcoin key. The pool never needs a COMPLETE password.

## Scripts

Claim witness script (`bitvm.rs`), same shape as `btcParseBitvmVaultScript`:

```
OP_IF
  OP_SHA256 <abort_commit 32> OP_EQUALVERIFY OP_TRUE
OP_ELSE
  <csv> OP_CSV OP_DROP
  OP_DUP OP_HASH160 <dest_h160 20> OP_EQUALVERIFY OP_CHECKSIG
OP_ENDIF
```

- `abort_commit = SHA256(abort_preimage)`.
- `abort_preimage` is random at claim open. **Hash only** on the ledger / status / snapshot.
- Preimage is written to a local `600` file (`keys/bitvm-abort/<note_id>`), never RPC.
- On accepted `RailChallenge`, apply reconstructs/reads it into `published_abort` so any watcher can spend the IF leaf **back to the pool**.
- ELSE leaf is the user. Protocol cannot take that UTXO after an honest Kickoff.

Pool address does **not** change when dests change. Only the Kickoff output script does.

## FPL state

| Status | Meaning |
|--------|---------|
| `pending` | FBTC burned, claim open, Kickoff may be signed |
| `challenged` | Watcher proved Kickoff ≠ claim (or unexpected spend). FBTC returned. Abort preimage published. |
| `final` | Challenge window on FPL elapsed, no valid challenge. User may take ELSE after Bitcoin CSV. |
| `paid` | Dest spend seen (optional mark). |

`RailChallenge { asset, note_id, observed_dest, observed_amount }` is a normal signed tx. Anyone. Protocol accepts iff observed dest/amount ≠ committed claim (theft Kickoff) or observed spend of the claim UTXO that is not the dest script.

Invalid challenges (`observed == committed`) are rejected. That is the anti-grief rule: you cannot abort an honest claim.

## What still signs Kickoff

Bitcoin cannot see an FPL burn. Creating the claim UTXO still needs a pool spend. That Kickoff is the **only** remaining coordinated signature (t-of-n on bonded seats, dealerless DKG, same address on reshare).

It is not COMPLETE. It cannot pay an arbitrary dest unless `t` ignore the committed claim (same collusion as forging 2300). After Kickoff, `t` **cannot** take the claim UTXO: only dest (after CSV) or a published abort (after a real Challenge).

Public testnet peg-in is **open** (`BTC_RAIL_LIVE`). Live Kickoff is **dest + watch** on NUMS (STATUS). Older prose about t-of-n FROST Kickoff is **superseded** — do not cite it as live. BitVM2 engine is the planned replacement for watch co-sign.

## Explicitly out

- Operator inventory / fronting BTC
- Global COMPLETE preimage
- Per-user-only vault (breaks Sally 1.5)
- Calling this finished without e2e: challenge abort on a mutated dest, honest dest-spend after CSV, grief rejected, status dump has no abort preimage
