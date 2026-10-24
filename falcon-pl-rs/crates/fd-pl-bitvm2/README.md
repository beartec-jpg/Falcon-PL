# fd-pl-bitvm2

BitVM2 engine for Falcon PL BTC exits.

| Phase | Contents |
|-------|----------|
| **B** | `FplWithdrawValid` Groth16 circuit |
| **C** | Optimistic Claim/Challenge/Assert/Disprove emulator |
| **C.1a** | Verifier-split trace + SHA256 Disprove taptree |
| **C.1b** | BitVM BN254/Groth16 pack bridge (`bn254_pack` + `vendor/BitVM`) |
| **D** | Sidecars + FPL hooks |
| **D.1** | Instance spend graph: NUMS P2TR, CSV+operator payout, n-of-n Challenge (no watch key) |

```bash
cargo test -p fd-pl-bitvm2
cargo run -p fd-pl-bitvm2 --bin falcon-pl-bitvm2-emu -- skeleton

# Optional: fetch BitVM upstream + export script digests
../../scripts/fetch-bitvm.sh
cd ../../tools/bitvm-c1b-export && cargo run --release
```

Spec: [`../fd-pl/docs/BITVM2_ENGINE.md`](../fd-pl/docs/BITVM2_ENGINE.md).
