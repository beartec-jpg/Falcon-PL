# Falcon PL — 1:1 user BTC vault (audit)

**Claim an auditor can check on Bitcoin, without trusting this repo’s marketing.**

BTC locked for wrapping is spent **only** with the depositor’s secp256k1 key.

| Question | Answer |
|----------|--------|
| Can the protocol / watch key / n-of-n / dave spend the vault? | **No.** Both branches end in `OP_CHECKSIG` against the user’s HASH160. |
| Can the user take BTC without a burn? | **Take path:** only with `take_preimage`, which FPL publishes **after** `RailWithdraw` burn. **Refund path:** CSV + user sig (if they never meant to wrap, or mint failed). A refund of a minted vault is unbacked FBTC until SPV sees the spend — treat FBTC as a **receipt of that UTXO**. |
| Operator fronting? | **No.** |
| n-of-n per exit? | **No.** |
| Watch co-sign? | **No.** |

## Script (P2WSH)

```
OP_IF
  OP_SHA256 <take_commit 32> OP_EQUALVERIFY
  OP_DUP OP_HASH160 <user_h160 20> OP_EQUALVERIFY OP_CHECKSIG
OP_ELSE
  <csv> OP_CSV OP_DROP
  OP_DUP OP_HASH160 <user_h160 20> OP_EQUALVERIFY OP_CHECKSIG
OP_ENDIF
```

`take_commit = SHA256(take_preimage)`  
`take_preimage = SHA256("fpl-user-take-v1|" || seed || dest20 || h160 || nonce)`  
Seed is `keys/bitvm-abort-seed` on validators (mode 600). **Not** a Bitcoin spend key.

## Flows

1. **Peg-in:** user funds this P2WSH + `OP_RETURN FALC‖dest20‖h160‖nonce` (76 bytes). FPL mints FBTC after Bitcoin SPV of that tx.
2. **Peg-out:** user burns FBTC. FPL publishes `take_preimage` on `published_takes`. User broadcasts take (preimage + their sig) to their dest. No protocol signature.
3. **Abort wrap:** user refunds after CSV with their sig only.

## What this is not

- Not a **pooled** wrapped-BTC reserve (old NUMS Kickoff / BitVM2 instance). Those still exist on 2300 as legacy.
- Not Groth16-in-Bitcoin-script. Theft of **user** BTC does not require it: the script never gives the protocol a key.
- If a validator leaks `take_preimage` before burn, the user (only) can unwrap; FBTC of that UTXO must be treated as invalid once the vault is spent.

## Commands

`falcon-pl-ctl btc-user-vault-address | btc-user-vault-deposit | btc-user-vault-take | btc-user-vault-refund`

Crate tests: `cargo test -p fd-pl --lib user_vault`
