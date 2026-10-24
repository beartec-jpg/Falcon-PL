# Plan: replace the single-party Groth16 setup with a public open-contribution setup

Status: PLAN (not implemented). Today `fpl_v3_proof` runs
`circuit_specific_setup` from a seed = single-party trust ("testnet single-party
setup"). Whoever knows the toxic waste can forge a proof that passes every
chunk and the binding leaf -> steal via Take2. This must change before any value
that matters.

## Target trust model
1-of-N: the setup is sound if ANY single contributor destroyed their secret.
Anyone may contribute; no allow-list; the transcript is public and verifiable by
anyone.

## Two phases
1. **Phase 1 (universal powers of tau, BN254).** Reuse an existing public
   ceremony with hundreds of contributors (Perpetual Powers of Tau / the Hermez
   `powersOfTau28_hez_final_NN.ptau` series). Needed size: 2^17 if the V3 final
   circuit is under 131k constraints, else 2^18 (to be measured: last V3 prototype ~95.7k
   before final gadgets). No ceremony of our own needed for phase 1.
2. **Phase 2 (circuit-specific, ours).** Sequential MPC on delta (δ ← δ·s,
   L/H queries ÷ s) with a proof of knowledge per contribution, ending in a
   public random beacon (hash of a pre-announced future Bitcoin block).

## Tooling
- Prefer audited snarkjs `zkey new / contribute / beacon / verify` over writing
  our own phase-2 code.
- Need an **arkworks → .r1cs exporter** for `FplWithdrawValidV3` (iterate the
  ConstraintSystem's A/B/C matrices; .r1cs is a simple documented binary format),
  plus a **zkey/vkey → ark `VerifyingKey<Bn254>` converter** and a **prover
  that loads the final zkey proving key** (ark-circom's zkey reader works, or
  convert to ark `ProvingKey`).
- Cross-check: proof from the converted PK verifies with snarkjs AND with the
  ark verifier AND with the BitVM chunker on regtest (honest Take2 + replay
  disprove tests above).
- Alternative if the exporter is a problem: our own arkworks phase-2 (~500 LOC)
  modelled on `phase2-bn254` (used by Tornado/Semaphore setups); more audit work.

## Ceremony operation
- Public repo/issue queue; contributor downloads the latest zkey (~tens of MB at
  2^17), runs `snarkjs zkey contribute` (or a browser page with snarkjs in WASM,
  minutes), uploads; coordinator runs `zkey verify` and publishes the chain hash.
- Contribution window, e.g. 2–4 weeks; then announce block height H, apply the
  beacon with block hash at H, publish final zkey + vk + full transcript.
- Anyone verifies end-to-end with `snarkjs zkey verify r1cs ptau final.zkey`.
- The coordinator cannot break soundness (each contribution is verified); it can only
  censor, which open mirrors and multiple coordinators address.

## Consequences for the bridge
- The VK is embedded in the connector C chunk leaves, so the **connector C
  root changes** when the VK changes. Graphs presigned under the single-party VK stay
  under that VK. Policy: testnet deposits made before the public setup are labelled
  "single-party setup" and are not migrated.
- Watchers publish the connector C root(s) for the public VK; the depositor
  signer (option (a)) will then only accept graphs whose connector C spk is in
  that published set, which ties every deposit to the public setup.
- Any circuit change (e.g. V3 → V4) needs a new phase 2 (phase 1 is reused).

## Work items
1. Measure V3 final constraint count; pick ptau size.
2. r1cs exporter + test (snarkjs `r1cs info` matches the constraint count).
3. zkey→ark PK/VK conversion; regtest honest/replay tests with converted keys.
4. Ceremony runbook + contribution page; dry run with 3 internal + 2 external
   contributors on testnet.
5. Public ceremony, beacon, publish, then switch connector C root publication.
