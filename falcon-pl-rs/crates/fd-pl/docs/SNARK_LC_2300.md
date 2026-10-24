# FPL Groth16 Light Client (ETH / USDC — `FalconQcBridgeV3`)

Ethereum has no native Falcon-512 precompile. Falcon PL peg-out headers are proven on Sepolia using Groth16 over BN254 (`crates/fd-pl-snark`).

**Live on Sepolia (`v2.9.81`):**
- **Bridge (`FalconQcBridgeV3`):** `0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4` (active since height `414,000`; header jumps `≤ 50,000` with 4-of-6 Falcon-512 QC proofs)
- **Groth16 Verifier (current V3):** `0xA03005921E1807587B942ab00cC4212650670aA6` (earlier V2/V3 verifier: `0x2Cb70e9f082F2DF91E9A5e6E7C9DF6b8b11B4F80`; legacy V1 bridge `0xf8F1471643792eb1cD5d0C31061629777E55bc48` used verifier `0x9992cD8e45A2b7983E2b7f8fC725308a0E4845EC`)
- **Peg-in Light Client:** Sepolia Gloas beacon sync-committee light client active since height `409,134`.

BabyJub `FplLcCircuit` / `FalconBridge` (`0x7eB72974…`) is **retired**.

## Public Inputs

In `_lcInputs` order:

0. `height`
1. `parent` high 128 bits
2. `parent` low 128 bits
3. `headerHash` high 128 bits
4. `headerHash` low 128 bits
5. `claimRoot` high 128 bits
6. `claimRoot` low 128 bits
7. `keyRoot`
8. `threshold` (`4`)
9. `networkId` (`2300`)

`openClaim` verifies the SHA-256 Merkle path of `(dest, amount)` against `claimRoot`, and `take()` is strictly destination-locked.

---

## Live Circuit: `FplFalconQcCircuit` (`fd-pl-snark/src/falcon/`)

`keyRoot` is the MiMC Merkle root of packed Falcon-512 public keys (512 coefficients, 16 per BN254 `Fr` word, MiMC-folded).

The circuit verifies **PQClean Falcon-512** (the exact signature scheme Testnet 2300 validators sign consensus QCs with):

1. SHAKE256 `HashToPoint-CT` (`nonce || statement`) — full FIPS 202 sponge.
2. `s1 = c − s2·h` in `Z_q[x]/(x^n+1)` with `n=512`, `q=12289`.
3. Short-vector norm bound `||(s1,s2)||² ≤ 34034726`.
4. Opened public keys Merkle-prove to `keyRoot` with distinct signer indices (`OPEN=1` per proof, 4 distinct signer proofs submitted together in `submitHeaderWithProofs`).

Statement bytes:

```
b"fpl-falcon-qc-v1" || height_be8 || parent32 || header32 || claim32 || network_id_be4
```

| Circuit Slice | R1CS Constraints |
|-------|-----------------------------------|
| SHAKE256(`"abc"`) → 32 bytes | 145,123 |
| `HashToPoint-CT` (nonce 40 + 124-byte statement) | 1,992,920 |
| One Falcon-512 signature (`OPEN=1`) | 2,423,413 |
| Four signatures in one circuit (`MAX_OPEN=4`) | 9,675,553 |

Using four `OPEN=1` proofs under one verifying key (`groth16_falcon_four_one_sigs`) allows proving within 15 GiB RAM (~140s per proof in release mode) while verifying all 4 distinct committee signers on-chain in `FalconQcBridgeV3.submitHeaderWithProofs`.

### CLI & Prover Usage

```bash
# Compute committee key root
falcon-pl-snark falcon-committee-root --keys-dir keys/falcon-qc

# Setup OPEN=1 from Hermez Powers of Tau
falcon-pl-snark falcon-setup-one-from-ptau --ptau powersOfTau28_hez_final_23.ptau --out-dir snark/falcon-qc-one/

# Run V3 header prover (reads key via systemd LoadCredential)
python3 scripts/pl-falcon-qc-v3-prover.py --send
```

## Contracts (`crates/fd-pl/scripts/`)

- `FalconQcBridgeV3.sol` & `FalconQcBridgeV3Checks.sol` — Live V3 Sepolia bridge (`submitHeaderWithProofs`, `depositEth`, `depositUsdc`, `openClaim`, `take`)
- `Groth16Verifier.sol` — BN254 Groth16 verifier contract
- `FalconQcBridgeV2.sol` / `FalconQcBridge.sol` — Historical V2 / V1 contracts
