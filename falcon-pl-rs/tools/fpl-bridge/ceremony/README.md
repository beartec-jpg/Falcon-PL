# Public Groth16 ceremony (BitVM2 withdraw VKs)

Do **not** start the real ceremony until the circuits are frozen (tagged commit),
the contributor list and beacon block height H are announced, and Scott OKs it.
These scripts prepare the dry-run and the eventual public run.

See also `docs/bridge/BITVM2_VK_SETUP.md`.

## Who does what

| role | machine | holds a secret? |
|---|---|---|
| **Coordinator** | any public box (e.g. Falcon) | **no** — only verifies and publishes |
| **Contributor** | their own machine | yes, for one snarkjs process, then destroyed |
| **Verifier** | anyone | no |

Falcon / the coordinator never contributes the only contribution and never
stores anyone's entropy.

## Numbered steps

### 0. Freeze (once)
1. Tag the commit whose `FplWithdrawValid` / `FplWithdrawValidV3` you want.
2. Decide the `vk_hash` / network-2300 convention (already baked into the circuits).
3. Announce: circuits tag, contributor window, Bitcoin **mainnet** beacon height H,
   and the pinned phase-1 URL + hash in `ceremony.env`.

### 1. Coordinator: `init`
```bash
export FPL_SETUP=/path/to/falcon-pl-bitvm2-setup   # built from the tagged commit
export CER_DIR=/path/to/ceremony-work
./coordinator.sh init
```
Publishes: `v1_0000.zkey`, `v3_0000.zkey`, both `.r1cs`, `transcript.md`.

### 2. Contributor N (repeat, sequential, both circuits)
On your own machine (v3 zkey is ~100 MB):
```bash
# install snarkjs 0.7.5 once: npm i -g snarkjs@0.7.5
./contribute.sh v1 v1_NNNN.zkey v1_out.zkey "Alice (github:alice)"
./contribute.sh v3 v3_NNNN.zkey v3_out.zkey "Alice (github:alice)"
```
Upload both `*_out.zkey` and publish the printed **Contribution Hash** (signed
post or commit). Reboot or close the shell so the entropy is gone.

### 3. Coordinator: `accept` each upload
```bash
./coordinator.sh accept v1 /incoming/alice_v1.zkey
./coordinator.sh accept v3 /incoming/alice_v3.zkey
```
Rejects anything that fails `snarkjs zkey verify`. Publish the new `*_NNNN.zkey`
and the updated transcript so the next contributor can start.

### 4. Beacon (after H is mined, and ≥3 outside contributors)
```bash
./coordinator.sh beacon <blockhash_of_H> H
./coordinator.sh publish
```
`manifest.json` has `vk_hex` / sha256 for both. Open a reviewed PR setting
`BITVM2_WITHDRAW_VK_HEX` (v1). Operators put `v3_vk.bin` into `FPL_PROOF_DIR`.

### 5. Anyone: `verify_ceremony.sh <published-dir>`
Rebuilds the r1cs from the tag, re-verifies both final zkeys against the ptau,
and checks the VKs match the manifest.

## Dry run (already done for v1; v3 below)
A local dry run uses the same scripts with throwaway entropy on one machine. Its
keys are **not** usable: every contribution ran on the same box.
