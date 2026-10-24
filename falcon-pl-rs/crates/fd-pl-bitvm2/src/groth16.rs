//! Groth16 setup / prove / verify for `FplWithdrawValid`.

use crate::circuit::FplWithdrawValid;
use ark_bn254::{Bn254, Fr};
use ark_ff::{BigInteger, PrimeField};
use ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_snark::SNARK;
use rand::{CryptoRng, RngCore};

pub fn setup<R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>), String> {
    Groth16::<Bn254>::circuit_specific_setup(FplWithdrawValid::default(), rng)
        .map_err(|e| format!("bitvm2 setup: {e}"))
}

pub fn prove<R: RngCore + CryptoRng>(
    pk: &ProvingKey<Bn254>,
    circuit: FplWithdrawValid,
    rng: &mut R,
) -> Result<Proof<Bn254>, String> {
    Groth16::<Bn254>::prove(pk, circuit, rng).map_err(|e| format!("bitvm2 prove: {e}"))
}

pub fn verify(
    vk: &VerifyingKey<Bn254>,
    public: &[Fr],
    proof: &Proof<Bn254>,
) -> Result<bool, String> {
    Groth16::<Bn254>::verify(vk, public, proof).map_err(|e| format!("bitvm2 verify: {e}"))
}

pub fn proof_to_bytes(proof: &Proof<Bn254>) -> Result<Vec<u8>, String> {
    let mut v = Vec::new();
    proof
        .serialize_compressed(&mut v)
        .map_err(|e| format!("proof ser: {e}"))?;
    Ok(v)
}

/// M1: deterministic setup from a 32-byte seed. **Dev/test only.** Anyone who
/// knows the seed holds the toxic waste and can forge proofs. Production uses a
/// fixed verifying key from a published multi-party ceremony (see BITVM2_ENGINE.md §5.4).
pub fn setup_from_seed(
    seed: [u8; 32],
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>), String> {
    use rand::SeedableRng;
    let mut rng = rand::rngs::StdRng::from_seed(seed);
    setup(&mut rng)
}

pub fn vk_to_bytes(vk: &VerifyingKey<Bn254>) -> Result<Vec<u8>, String> {
    let mut v = Vec::new();
    vk.serialize_compressed(&mut v)
        .map_err(|e| format!("vk ser: {e}"))?;
    Ok(v)
}

pub fn vk_from_bytes(bytes: &[u8]) -> Result<VerifyingKey<Bn254>, String> {
    VerifyingKey::<Bn254>::deserialize_compressed(bytes).map_err(|e| format!("vk de: {e}"))
}

pub fn proof_from_bytes(bytes: &[u8]) -> Result<Proof<Bn254>, String> {
    Proof::<Bn254>::deserialize_compressed(bytes).map_err(|e| format!("proof de: {e}"))
}

pub fn fr_to_be32(f: Fr) -> [u8; 32] {
    let mut le = f.into_bigint().to_bytes_le();
    le.resize(32, 0);
    le.reverse();
    let mut out = [0u8; 32];
    out.copy_from_slice(&le);
    out
}

pub fn public_from_be_hex(hexes: &[String]) -> Result<Vec<Fr>, String> {
    hexes
        .iter()
        .map(|h| {
            let raw = hex::decode(h.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
            if raw.len() != 32 {
                return Err(format!("public fr must be 32 bytes, got {}", raw.len()));
            }
            Ok(Fr::from_be_bytes_mod_order(&raw))
        })
        .collect()
}
