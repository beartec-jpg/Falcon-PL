//! Groth16 setup / prove / verify over BN254, plus Solidity calldata export.

use crate::circuit::FplLcCircuit;
use crate::falcon::circuit::{FplFalconQcCircuit, MAX_OPEN};
use ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine};
use ark_ec::AffineRepr;
use ark_ff::{BigInteger, PrimeField};
use ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_snark::SNARK;
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
pub struct SolidityProof {
    pub a: [String; 2],
    pub b: [[String; 2]; 2],
    pub c: [String; 2],
    pub input: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SolidityVk {
    pub alpha: [String; 2],
    pub beta: [[String; 2]; 2],
    pub gamma: [[String; 2]; 2],
    pub delta: [[String; 2]; 2],
    pub ic: Vec<[String; 2]>,
}

fn fq_be_hex(f: Fq) -> String {
    let mut le = f.into_bigint().to_bytes_le();
    le.resize(32, 0);
    le.reverse();
    format!("0x{}", hex::encode(le))
}

fn g1_xy(p: &G1Affine) -> [String; 2] {
    let x = *p.x().expect("g1 infinity");
    let y = *p.y().expect("g1 infinity");
    [fq_be_hex(x), fq_be_hex(y)]
}

/// Ethereum pairing precompile wants Fq2 as (c1, c0).
fn g2_xy(p: &G2Affine) -> [[String; 2]; 2] {
    let x: Fq2 = *p.x().expect("g2 infinity");
    let y: Fq2 = *p.y().expect("g2 infinity");
    [
        [fq_be_hex(x.c1), fq_be_hex(x.c0)],
        [fq_be_hex(y.c1), fq_be_hex(y.c0)],
    ]
}

fn fr_be_hex(f: Fr) -> String {
    let mut le = f.into_bigint().to_bytes_le();
    le.resize(32, 0);
    le.reverse();
    format!("0x{}", hex::encode(le))
}

pub fn export_proof(proof: &Proof<Bn254>, public: &[Fr]) -> SolidityProof {
    SolidityProof {
        a: g1_xy(&proof.a),
        b: g2_xy(&proof.b),
        c: g1_xy(&proof.c),
        input: public.iter().copied().map(fr_be_hex).collect(),
    }
}

pub fn export_vk(vk: &VerifyingKey<Bn254>) -> SolidityVk {
    SolidityVk {
        alpha: g1_xy(&vk.alpha_g1),
        beta: g2_xy(&vk.beta_g2),
        gamma: g2_xy(&vk.gamma_g2),
        delta: g2_xy(&vk.delta_g2),
        ic: vk.gamma_abc_g1.iter().map(g1_xy).collect(),
    }
}

pub fn setup<R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>), String> {
    Groth16::<Bn254>::circuit_specific_setup(FplLcCircuit::default(), rng)
        .map_err(|e| format!("setup: {e}"))
}

pub fn prove<R: RngCore + CryptoRng>(
    pk: &ProvingKey<Bn254>,
    circuit: FplLcCircuit,
    rng: &mut R,
) -> Result<Proof<Bn254>, String> {
    Groth16::<Bn254>::prove(pk, circuit, rng).map_err(|e| format!("prove: {e}"))
}

pub fn setup_falcon_open<const OPEN: usize, R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>), String> {
    Groth16::<Bn254>::circuit_specific_setup(FplFalconQcCircuit::<OPEN>::default(), rng)
        .map_err(|e| format!("falcon setup: {e}"))
}

/// Groth16 setup for the Falcon-512 QC circuit (OPEN=4). New pk/vk; not the live LC keys.
pub fn setup_falcon<R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>), String> {
    setup_falcon_open::<MAX_OPEN, R>(rng)
}

pub fn prove_falcon_open<const OPEN: usize, R: RngCore + CryptoRng>(
    pk: &ProvingKey<Bn254>,
    circuit: FplFalconQcCircuit<OPEN>,
    rng: &mut R,
) -> Result<Proof<Bn254>, String> {
    Groth16::<Bn254>::prove(pk, circuit, rng).map_err(|e| format!("falcon prove: {e}"))
}

pub fn setup_falcon_v2<R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>), String> {
    Groth16::<Bn254>::circuit_specific_setup(crate::FplFalconQcCircuitV2::default(), rng)
        .map_err(|e| format!("falcon v2 setup: {e}"))
}

pub fn prove_falcon_v2<R: RngCore + CryptoRng>(
    pk: &ProvingKey<Bn254>,
    circuit: crate::FplFalconQcCircuitV2,
    rng: &mut R,
) -> Result<Proof<Bn254>, String> {
    Groth16::<Bn254>::prove(pk, circuit, rng).map_err(|e| format!("falcon v2 prove: {e}"))
}

pub fn prove_falcon<R: RngCore + CryptoRng>(
    pk: &ProvingKey<Bn254>,
    circuit: FplFalconQcCircuit<MAX_OPEN>,
    rng: &mut R,
) -> Result<Proof<Bn254>, String> {
    prove_falcon_open::<MAX_OPEN, R>(pk, circuit, rng)
}

pub fn verify(vk: &VerifyingKey<Bn254>, public: &[Fr], proof: &Proof<Bn254>) -> Result<bool, String> {
    Groth16::<Bn254>::verify(vk, public, proof).map_err(|e| format!("verify: {e}"))
}

pub fn save_pk(pk: &ProvingKey<Bn254>, path: impl AsRef<Path>) -> Result<(), String> {
    let mut buf = Vec::new();
    pk.serialize_uncompressed(&mut buf)
        .map_err(|e| e.to_string())?;
    std::fs::write(path, buf).map_err(|e| e.to_string())
}

pub fn load_pk(path: impl AsRef<Path>) -> Result<ProvingKey<Bn254>, String> {
    let buf = std::fs::read(path).map_err(|e| e.to_string())?;
    ProvingKey::<Bn254>::deserialize_uncompressed(&buf[..]).map_err(|e| e.to_string())
}

pub fn save_vk_json(vk: &VerifyingKey<Bn254>, path: impl AsRef<Path>) -> Result<(), String> {
    let json = serde_json::to_string_pretty(&export_vk(vk)).map_err(|e| e.to_string())?;
    std::fs::write(path, json + "\n").map_err(|e| e.to_string())
}

pub fn save_vk_bin(vk: &VerifyingKey<Bn254>, path: impl AsRef<Path>) -> Result<(), String> {
    let mut buf = Vec::new();
    vk.serialize_uncompressed(&mut buf)
        .map_err(|e| e.to_string())?;
    std::fs::write(path, buf).map_err(|e| e.to_string())
}

pub fn load_vk_bin(path: impl AsRef<Path>) -> Result<VerifyingKey<Bn254>, String> {
    let buf = std::fs::read(path).map_err(|e| e.to_string())?;
    VerifyingKey::<Bn254>::deserialize_uncompressed(&buf[..]).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schnorr::{generate, statement_digest, LcSecret};
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn committee(rng: &mut ChaCha20Rng, n: usize) -> Vec<LcSecret> {
        (1..=n)
            .map(|i| generate(&format!("v{i}"), rng))
            .collect()
    }

    #[test]
    fn groth16_honest_4_of_6() {
        let mut rng = ChaCha20Rng::from_seed([9u8; 32]);
        let secs = committee(&mut rng, 6);
        let pubs: Vec<_> = secs.iter().map(|s| s.public()).collect();
        let parent = [11u8; 32];
        let header = [22u8; 32];
        let claim = [33u8; 32];
        let stmt = statement_digest(4, &parent, &header, &claim, 2300);
        let sigs: Vec<_> = secs[..4]
            .iter()
            .map(|s| s.sign_stmt(stmt, &mut rng).unwrap())
            .collect();
        let circuit = FplLcCircuit::from_committee(
            4, &parent, &header, &claim, 2300, 4, &pubs, &sigs,
        )
        .unwrap();
        let public = circuit.public_values();
        let (pk, vk) = setup(&mut rng).unwrap();
        let proof = prove(&pk, circuit, &mut rng).unwrap();
        assert!(verify(&vk, &public, &proof).unwrap());
        let mut bad = public.clone();
        bad[0] = Fr::from(5u64);
        assert!(!verify(&vk, &bad, &proof).unwrap());
    }

    fn falcon_pubs_sigs(
        n_keys: usize,
        n_sigs: usize,
        height: u64,
        parent: &[u8; 32],
        header: &[u8; 32],
        claim: &[u8; 32],
    ) -> (
        Vec<crate::falcon::FalconPublic>,
        Vec<crate::falcon::FalconSig>,
    ) {
        use crate::falcon::native::statement_bytes;
        use pqcrypto_falcon::falcon512;
        use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _};
        let msg = statement_bytes(height, parent, header, claim, 2300);
        let mut pubs = Vec::new();
        let mut sigs = Vec::new();
        for i in 1..=n_keys {
            let (pk, sk) = falcon512::keypair();
            let id = format!("v{i}");
            pubs.push(crate::falcon::FalconPublic {
                id: id.clone(),
                pk_hex: hex::encode(pk.as_bytes()),
            });
            if i <= n_sigs {
                let sig = falcon512::detached_sign(&msg, &sk);
                sigs.push(crate::falcon::FalconSig {
                    id,
                    sig_hex: hex::encode(sig.as_bytes()),
                });
            }
        }
        (pubs, sigs)
    }

    #[test]
    fn groth16_falcon_one_sig() {
        let mut rng = ChaCha20Rng::from_seed([11u8; 32]);
        let parent = [11u8; 32];
        let header = [22u8; 32];
        let claim = [33u8; 32];
        let (pubs, sigs) = falcon_pubs_sigs(1, 1, 9, &parent, &header, &claim);
        let circuit = FplFalconQcCircuit::<1>::from_committee(
            9, &parent, &header, &claim, 2300, 1, &pubs, &sigs,
        )
        .unwrap();
        let public = circuit.public_values();
        let (pk, vk) = setup_falcon_open::<1, _>(&mut rng).unwrap();
        let proof = prove_falcon_open::<1, _>(&pk, circuit, &mut rng).unwrap();
        assert!(verify(&vk, &public, &proof).unwrap());
        let mut bad = public.clone();
        bad[0] = Fr::from(10u64);
        assert!(!verify(&vk, &bad, &proof).unwrap());
    }

    /// 4-of-6 as four OPEN=1 Groth16 proofs (fits 15GiB). Distinct private
    /// indices; on-chain distinctness needs the public `opened_mask` IO.
    #[test]
    fn groth16_falcon_four_one_sigs() {
        let mut rng = ChaCha20Rng::from_seed([13u8; 32]);
        let parent = [11u8; 32];
        let header = [22u8; 32];
        let claim = [33u8; 32];
        let (pubs, sigs) = falcon_pubs_sigs(6, 4, 9, &parent, &header, &claim);
        let (pk, vk) = setup_falcon_open::<1, _>(&mut rng).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for sig in &sigs {
            let circuit = FplFalconQcCircuit::<1>::from_committee(
                9, &parent, &header, &claim, 2300, 1, &pubs, std::slice::from_ref(sig),
            )
            .unwrap();
            assert!(seen.insert(circuit.opens[0].index), "duplicate index");
            let public = circuit.public_values();
            let proof = prove_falcon_open::<1, _>(&pk, circuit, &mut rng).unwrap();
            assert!(verify(&vk, &public, &proof).unwrap());
        }
        assert_eq!(seen.len(), 4);
    }
}
