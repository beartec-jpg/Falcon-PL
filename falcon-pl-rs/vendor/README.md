# BitVM upstream (Phase C.1b)

Shallow checkout of [BitVM/BitVM](https://github.com/BitVM/BitVM) for Falcon PL’s
Groth16/BN254 Disprove pack. **Not** a Falcon workspace member (ark-bn254 0.5 +
heavy script codegen).

Pinned tip used by `fd-pl-bitvm2::verifier_split::bn254_pack`: see
`UPSTREAM_COMMIT` in that module / `data/bitvm_bn254_pack_manifest.json`.

## Fetch

From `falcon-pl-rs/`:

```bash
./scripts/fetch-bitvm.sh
```

## Export script digests (optional, heavy)

```bash
cd tools/bitvm-c1b-export
cargo run --release -- --vendor ../../vendor/BitVM --out ../../crates/fd-pl-bitvm2/data/bitvm_scripts_cache.json
```

Full ScriptBuf blobs are huge (~1GB class); the export tool writes **per-tap
SHA256 digests** by default. Point `FPL_BITVM_SCRIPTS_CACHE` at that JSON to
upgrade the Disprove pack commitment.
