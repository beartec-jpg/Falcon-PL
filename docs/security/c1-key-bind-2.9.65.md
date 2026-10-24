# C-1: first-seen key binding (fixed in 2.9.65)

## The bug
Before 2.9.65 the first tx that carries a Falcon key binds that key to an
account that has no key yet (`enroll_name_key` from `Tx::is_name_enroll`,
which includes Pay, AssetPay, VaultOpen, NameReserve and, on main, Claim;
and `note_sender_key` for account meta). Nothing ties the key to the account,
so anyone can sign a Pay for a funded account that has never sent a tx and
drain it.

## The rule from the activation height
`TESTNET_2300_KEY_BIND_FROM_HEIGHT = 571000` (open height `tip + 1 >= H`,
public testnet 2300, Falcon512 only; other networks unchanged). Before H the
behaviour is byte-for-byte the same as 2.9.64.

From H a tx may bind the **first** key of an account only if one of these holds:
1. **Funder commitment.** The account was funded with the new `PayToKey`
   body (`{"kind":"pay_to_key","key_hash":<hex>}`), which is a Pay that also
   records `key_hash = sha256(raw 897-byte Falcon public key)` on the
   recipient (`AccountMeta.key_commit`). Only the key with that hash can bind.
2. **Key-derived id.** The account id is the classic address of the key
   (`base58check(0x00 || ripemd160(sha256(0xFB || raw_pub)))`, as the site
   wallet derives `wallet.address`). The id already commits to the key.

Accounts that already have a key (ring or account meta) are unchanged. The
admission bodies (ApproveValidator, RevokeApproval, RevokeArchive) keep their
own release-key check. Every other body (Pay, AssetPay, Claim, NameReserve,
VaultOpen, rail/watcher txs) fails with `key bind: ...` and binds nothing:
the check runs before the mempool or apply path inserts the key.

`PayToKey` rules: refused (mempool and apply) before H, since 2.9.64 cannot
decode it; `key_hash` is 64 lowercase hex; destination must differ from the
sender and must not have a key yet; an existing commitment can only be
repeated (top-up), not replaced. Otherwise it applies like Pay.

Replay protection is unchanged (sequence).

## Operational consequences
- New named wallets: the funding tx must be `PayToKey` with the wallet's key
  hash (faucet: `falcon-pl-ctl faucet --key-hash <sha256>`; walletd takes
  the key from its name reservation). Then the wallet's first Pay binds the
  key exactly as today.
- A name funded with a plain Pay after H can receive and hold funds but cannot
  be spent until a funder commits a key with `PayToKey` (only while no key is
  bound).
- Funded accounts with no key at H (4 on live at 10:57 UTC 14 Oct, 14 200 FPL)
  are frozen rather than stealable until someone commits their key.
