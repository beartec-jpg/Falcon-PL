# Falcon PL — pooled BTC, transferable FBTC

**This is the product.** Not 1:1 vaults. Not operator fronting. Not n-of-n on exit.

Dave pegs in. FPL mints **fungible FBTC**. Dave pays Sally on Falcon. Sally burns FBTC and **signs the Bitcoin peg-out**. If dest/amount do not match a burn, **anyone** broadcasts a setup-once abort that can only return coins to the pool.

## Auditor table

| Question | Answer |
|----------|--------|
| Transferable / fungible FBTC? | **Yes.** Peg-in credits an account balance. Dave → Sally is a normal FPL transfer. Sally unwraps from the **shared** pool, not from Dave’s original UTXO. |
| Who signs the peg-out? | **Sally** (withdrawer’s BIP340). Pubkey is in the witness, not baked into the pool at setup — otherwise only Dave could unwrap. |
| Can watch / n-of-n / dave / operator take the pool? | **No.** Key-path is NUMS (dead). Peg-out leaf has no protocol key. Abort leaf’s key can only satisfy a **presigned return-to-pool** tx. |
| Operator fronting? | **No.** |
| n-of-n per exit? | **No.** |
| Who challenges fraud? | **Anyone.** FPL `RailChallenge` publishes the abort preimage. Anyone broadcasts the presigned abort. |
| Can the abort key steal? | **No.** `presign_abort` only emits SIGHASH_ALL with the single output = the same pool script. A leaked abort key can freeze/re-anchor, not pay itself. |

## Scripts (NUMS P2TR)

```
pegout:  <csv> OP_CSV OP_DROP OP_CHECKSIG
abort:   OP_SHA256 <abort_commit> OP_EQUALVERIFY <abort_xonly> OP_CHECKSIG
```

- After CSV, Sally’s witness is `[sig, sally_xonly, script, control]`. Dest-lock is the **output** (her address + change to pool).
- Abort does not wait for CSV. Witness is `[abort_sig, preimage, script, control]`. Outputs are already signed: 100% back to the pool.

## Flows

1. **Ceremony (once per script):** abort key + abort preimage. Address is public. Preimage stays 0600 until a challenge.
2. **Peg-in:** BTC → pool + `OP_RETURN FALC‖dest20`. FPL mints FBTC to that account.
3. **Transfer:** Dave pays Sally FBTC on Falcon.
4. **Peg-out:** Sally burns FBTC (`RailWithdraw` with her BTC dest). After CSV she broadcasts peg-out: pool → dest + change-to-pool.
5. **Challenge:** a pool spend whose vout0 dest/amount is not an open burn → `submit-rail-challenge` → publish preimage → broadcast presigned abort (double-spend of the pool input).
6. **Refresh:** every new pool outpoint (honest change or abort re-anchor) needs a **new** presigned abort. Same abort key, new signature. CSV clock restarts on the new UTXO.

## Bitcoin limits (not marketing)

- Scripts cannot name “the current FBTC holder.” Transferability **requires** the peg-out leaf to accept the withdrawer’s key from the witness. Theft after CSV is a **mempool race against abort**, so at least one challenger must be online in the window.
- CSV is relative to the UTXO’s confirmation age. A pool UTXO that sits past CSV is immediately spendable on ELSE; abort must land in the next block. Re-anchor (abort to self) before the window dies.
- After an honest peg-out, the change UTXO is a **new outpoint**. The old abort tx cannot spend it. Re-presign immediately.

## Commands

```
falcon-pl-ctl btc-fbtc-pool-ceremony --keys-dir DIR [--csv 6]
falcon-pl-ctl btc-fbtc-pool-address --keys-dir DIR
falcon-pl-ctl btc-fbtc-pool-deposit --privhex-file … --prev-txid … --sats … --account dave --keys-dir DIR
falcon-pl-ctl btc-fbtc-pool-pegout --privhex-file sally --utxo txid:vout:sats --dest tb1q… --amount N --keys-dir DIR
falcon-pl-ctl btc-fbtc-pool-presign-abort --keys-dir DIR --utxo txid:vout:sats
falcon-pl-ctl btc-fbtc-pool-abort --keys-dir DIR
```

Crate tests: `cargo test -p fd-pl --lib fbtc_pool`

Live 2300 mint still follows packed `vault_pub` unless `BITVM2_INSTANCE_SPK` is set on the node (extra mint dest). Do not spend the legacy NUMS Kickoff (~369k) into this script as a “migration” without an explicit go.

The BitVM2 path that replaces the CSV-anyone pool: **claimer Kickoff dest-lock + setup-once Challenge**. See [BITVM2_ENGINE.md](BITVM2_ENGINE.md). `btc-bitvm2-user-kickoff` does not use setup keys. `btc-bitvm2-presign --wipe` deletes them after the ceremony.
