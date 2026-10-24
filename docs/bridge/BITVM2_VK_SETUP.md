# Producing and publishing `BITVM2_WITHDRAW_VK_HEX` (public Groth16 setup)

> **Testnet 2300 keys exist (2026-10-19)**: Falcon's logged single-operator setup for both
> circuits (Hermez 2^18, beacon = mainnet block 972174). See `docs/bridge/ceremony/testnet-vk-2026-10-19/`.
> v1 VK sha256 `4a1cabbc…4763` is now `BITVM2_WITHDRAW_VK_HEX` (inert until the bridge height);
> v3 `vk.bin` sha256 `c6d3509b…7843`. Mainnet still needs the multi-party ceremony below.

Status 2026-10-19: tooling implemented and the full flow dry-run on the box (below).
The real setup has NOT been run. Falcon holds no secret at any step. The setup is
sound if at least ONE contributor destroyed their secret (1-of-N).

## Two keys, one procedure
| circuit | used by | public inputs | constraints | phase-1 size |
|---|---|---|---|---|
| **v1** `FplWithdrawValid` | ledger `Bitvm2AttestV2` (M1): `bitvm2_exit::BITVM2_WITHDRAW_VK_HEX` | 13 | 5,483 | 2^13 (any larger ptau works) |
| **v3** `FplWithdrawValidV3` | BitVM connector C split verifier (Disprove / Take2), `FPL_PROOF_DIR/vk.bin` | 1 | 168,518 | **2^18** |

Both are sound only with a public setup. Today v1 has no key at all, so attests are
refused. v3 uses a seeded single-party key (`fpl_v3_proof --seed`): testnet only, and
labelled as such. Run both phase-2 ceremonies in the same round, on the same phase 1.
The r1cs sizes come from `falcon-pl-bitvm2-setup r1cs`. `snarkjs r1cs info` gives the
same counts.

## 0. Freeze first (any change after this needs a new phase 2)
- The circuit code of both circuits, at a tagged commit.
- Public-input conventions baked into the statement: the network constant `2300` and
  `vk_hash` (today `abi::test_vk_hash()`, a public-input value, not part of the VK).
  Decide whether it stays a fixed domain constant or becomes a circuit-version hash.
  Either is fine; it must be fixed before the r1cs is published.
- Record the r1cs sha256 for both circuits (`falcon-pl-bitvm2-setup r1cs` prints it).
  Anyone can rebuild it from the tag and compare.

## 1. Phase 1 (reuse a public ceremony, no work of our own)
Use the Hermez / Perpetual Powers of Tau final file for BN254:
`powersOfTau28_hez_final_18.ptau` (2^18, covers both circuits). Pin its published hash and
check it with `snarkjs powersoftau verify` (slow, run once; anyone may repeat it).

## 2. Phase 2 (ours, sequential, open contribution)
Roles:
- **Coordinator** (the box or any public machine). Holds no secret.
  - Runs `snarkjs zkey new circuit.r1cs pot18.ptau c_0000.zkey`.
  - After each contribution, runs `snarkjs zkey verify circuit.r1cs pot18.ptau c_NNNN.zkey`
    and publishes the zkey plus the contribution hash. Can censor, but cannot weaken
    soundness. Mirrors and a second coordinator mitigate censorship.
- **Contributors**: Scott, each bridge operator (own machine), outside reviewers, anyone.
  - Each downloads the latest zkey (v3 is ~100 MB) and runs
    `snarkjs zkey contribute in.zkey out.zkey --name=<who> -e=<own entropy>` on their own
    machine. Then they upload out.zkey and publish the printed contribution hash (signed
    post / commit).
  - The secret exists only in that snarkjs process and is gone when it exits. Recommended:
    run offline, then reboot or wipe.
  - **Falcon / the box never contributes the only contribution and never stores
    anyone's entropy.**
- **Beacon**: announce in advance a future Bitcoin **mainnet** block height H. After it
  is mined: `snarkjs zkey beacon last.zkey final.zkey <blockhash(H)> 10`.
- **Window**: e.g. 2 weeks, with a minimum of 3 independent outside contributors before
  the beacon.

## 3. Publish
For each circuit:
- `snarkjs zkey verify circuit.r1cs pot18.ptau final.zkey` gives the full transcript.
- `snarkjs zkey export verificationkey final.zkey vk.json`.
- `falcon-pl-bitvm2-setup vk-import --vk vk.json --out-bin vk.bin` prints `vk_hex` and
  its sha256.
  - Do the curve and subgroup checks on every point; reject an off-curve point.
  - Check `n_public` is 13 for v1 and 1 for v3.
- Publish in one place: `final.zkey` (the proving key; public, operators need it),
  `vk.json`, `vk_hex`, the sha256 values, the contribution list with hashes, H and its
  block hash.
- **v1**: a reviewed PR sets `BITVM2_WITHDRAW_VK_HEX = "<vk_hex>"` in
  `crates/fd-pl/src/bitvm2_exit.rs`. It ships in the same combined node release as the
  bridge height.
- **v3**: `vk.bin` goes into the operators' `FPL_PROOF_DIR`, and the connector C roots
  for that VK are published. Depositors accept only graphs whose connector C script is
  in the published set. Graphs presigned under the seeded testnet key stay "single-party
  setup" and are not migrated.

## 4. Proving (operators; no arkworks proving key anywhere)
- **M1 attest (v1)**:
  1. `falcon-pl-bitvm2-setup attest-wtns --input attest.json --out w.wtns --public-out pub.json`
     (attest.json = note id, dest h160, amount, checkpoint height + header, peg-out txid
     internal, MiMC leaves).
  2. `snarkjs groth16 prove final_v1.zkey w.wtns proof.json public.json`.
  3. `falcon-pl-bitvm2-setup proof-import --vk vk_v1.json --proof proof.json --public public.json`
     prints `proof_hex` and `public_be` for `Bitvm2AttestV2`, verified with arkworks
     before submit.
  - **Wired into the operator kit (2026-10-19)**: `fpl-operatord` runs
    `bin/fpl-operator-attest.sh` for each of its mined peg-outs whose claim is still
    AwaitingPegout/PegoutPosted (retry every 10 min; the ledger needs 6 confs and a final
    checkpoint). The hook: `falcon-pl-ctl status` → `falcon-pl-bitvm2-attest input`
    (claim, leaves, newest final checkpoint ≥ burn; status now lists `fpl_checkpoints`)
    → `attest-wtns` → `snarkjs groth16 prove $FPL_OP_ATTEST_ZKEY` → `proof-import` →
    `falcon-pl-bitvm2-attest body` (Bitcoin merkle path from `getblock`, statement digest,
    optional exact ledger pre-check with `FPL_OP_ATTEST_VK_HEX`) → `falcon-pl-ctl operator
    --body` (now accepts `bitvm2_attest_v2`). Inert until `FPL_OP_ATTEST_ZKEY` and
    `FPL_OP_ATTEST_VK_JSON` are configured and the bridge height is active.
    Tests: `tools/fpl-operator/tests/attest_snarkjs_stub.sh` (real hook, stub ctl/bitcoin-cli)
    and ledger test `kit_snarkjs_attest_body_accepted_by_ledger` (that body → ClaimPosted).
- **v3 / BitVM**: the same wtns → snarkjs → `proof-import --out-bin proof.bin` path
  replaces `fpl_v3_proof --seed`: `falcon-pl-bitvm2-setup v3-wtns --input attest.json
  --binding-out binding_commit.hex` takes the same real statement (real leaves) as the attest;
  `proof-import --out-bin proof.bin --public-bin public.bin` and `vk-import --out-bin vk.bin`
  fill `FPL_PROOF_DIR`. `tools/fpl-bridge/v3_snarkjs_prove.sh` is a drop-in for
  `falcon-pl-bitvm2-v3-proof` (same flags, or `--input attest.json`).

## What lives where
- **Box (coordinator)**: the r1cs files, the public ptau, the zkey chain, the verify logs,
  the published artifacts. No entropy, no toxic waste.
- **Contributors**: their own snarkjs run and entropy, destroyed after.
- **Operators**: `final.zkey` (public) and snarkjs to prove. No key for the setup.
- **Ledger**: only `BITVM2_WITHDRAW_VK_HEX` (public).

## Tooling in this branch
- `crates/fd-pl-bitvm2/src/snarkjs_io.rs`:
  - r1cs / wtns writers (arkworks column order: wire 0 = 1, then the public inputs, then
    the witness);
  - vk / proof / public JSON importers with on-curve and subgroup checks.
- `falcon-pl-bitvm2-setup` (bin): `r1cs`, `demo-wtns`, `attest-wtns`, `vk-import`, `proof-import`.
- `tools/fpl-bridge/vk_setup_dryrun_v1.sh`: the dry run below.

## Dry run (2026-10-19, box only, NOT a usable key)
- Setup: local ptau 2^13, 1 phase-1 contribution, 2 phase-2 contributions, and a fixed
  beacon `00..01`, all generated on the box.
- `snarkjs zkey verify`: ZKey Ok.
- `wtns check` of our demo witness: OK.
- A snarkjs proof verifies in snarkjs and in arkworks under the imported VK.
- snarkjs public signals == Rust `public_values()`.
- Ledger test `m1_attest_with_snarkjs_proof_and_imported_vk`: the node, armed with the
  imported VK, accepts a `Bitvm2AttestV2` proved by snarkjs (SPV peg-out + Groth16) →
  ClaimPosted; the same proof under another VK is refused.
- Fixtures: `fd-pl-bitvm2/testdata/dryrun_v1_*.json`, `fd-pl/testdata/snarkjs_dryrun_m1_attest.json`.

## Ceremony scripts (ready, not started)
`tools/fpl-bridge/ceremony/`: `coordinator.sh init|accept|beacon|publish`, `contribute.sh`
(contributor's own machine), `verify_ceremony.sh` (anyone), `ceremony.env` (pinned phase 1:
PSE Perpetual Powers of Tau #80, 2^18, sha256 9693220206afab749e3d88d4ab5fdf5d36120ea102e7e587ccea0e7a5208e711),
numbered steps in its `README.md`. The Hermez S3 file now returns 403, so phase 1 is the PSE
PPoT file (also snarkjs-ready).

Full local dry run (`dryrun_local.sh`, 2026-10-19, throwaway): init for v1+v3 → 2 contributions
each through `contribute.sh` → `accept` (zkey verify) → beacon → publish → `verify_ceremony.sh`
re-derives both VKs = manifest. 2 min 43 s on the box. v3 snarkjs proofs take ~5 s.

## Remaining before the real ceremony
1. Freeze the circuits and `vk_hash`; tag the commit.
2. Cross-check the PPoT #80 2^18 hash against PSE's published list.
3. Recruit at least 3 outside contributors; announce the window and beacon height H.
4. Scott's go-ahead; then `coordinator.sh init` on a public machine.
