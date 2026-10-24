# Catch-up per-tx Falcon verify (v2.9.66 / audit R-2)

## Default (v2.9.66+)

When applying a **certified** catch-up ledger whose body is the published
full-signature form (`batch_sig` empty), each transaction’s Falcon signature
is verified — same as the live accept path.

Merkle-batch bodies (`batch_sig` set) already verify sender signatures in
`verify_merkle_batch_known` from height `TESTNET_2300_MERKLE_SENDER_SIG_FROM_HEIGHT`
(520000 on public testnet 2300).

## Escape hatch (weak boxes)

If catch-up cannot keep up with the live head on a constrained host:

```
Environment=FPL_CATCHUP_SKIP_TX_SIG=1
```

Restores the pre-2.9.66 skip when `parent_cert` is present. Prefer upgrading
the host; use the flag only as a temporary operator override.

## Bench (developer workstation, portable `x86-64-v2`, 2026-10-14)

| Metric | Value |
|--------|-------|
| Falcon-512 verify | ~0.06 ms/tx (~16k verify/s) |
| Extra cost @ 64 txs/ledger | ~4 ms |

Even at ~10× slower hardware, a full block is on the order of tens of ms —
usually acceptable versus snap-treadmill risk from skipping verify.
