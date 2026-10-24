//! MiMC-7 over BN254 Fr. Domain-separated constants (not circomlib).
//! Used as the SNARK-friendly hash for LC keys, merkle, and Schnorr Fiat-Shamir.

use ark_bn254::Fr;
use ark_ff::{BigInteger, Field, PrimeField, Zero};
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::prelude::*;
use ark_relations::r1cs::SynthesisError;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// 91 rounds, 128-bit security class for a 254-bit field (MiMC paper).
pub const MIMC_ROUNDS: usize = 91;

static CTS: OnceLock<Vec<Fr>> = OnceLock::new();

pub fn constants() -> &'static [Fr] {
    CTS.get_or_init(|| {
        let mut out = Vec::with_capacity(MIMC_ROUNDS);
        for i in 0..MIMC_ROUNDS {
            let h = Sha256::digest(format!("falcon-pl-mimc7-bn254-v1|{i}").as_bytes());
            out.push(Fr::from_le_bytes_mod_order(&h));
        }
        out[0] = Fr::zero();
        out[MIMC_ROUNDS - 1] = Fr::zero();
        out
    })
}

fn pow7(x: Fr) -> Fr {
    let x2 = x.square();
    let x4 = x2.square();
    x4 * x2 * x
}

/// MiMC-7(x, k) with the domain constants.
pub fn mimc7(x: Fr, k: Fr) -> Fr {
    let cts = constants();
    let mut s = x;
    for c in cts {
        let t = s + k + c;
        s = pow7(t);
    }
    s + k
}

/// 2-to-1 compression: MiMC7(MiMC7(a)+b).
pub fn mimc2(a: Fr, b: Fr) -> Fr {
    mimc7(mimc7(a, Fr::zero()) + b, Fr::zero())
}

pub fn merkle8(leaves: &[Fr; 8]) -> Fr {
    let mut layer: Vec<Fr> = leaves.to_vec();
    while layer.len() > 1 {
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            next.push(mimc2(pair[0], pair[1]));
        }
        layer = next;
    }
    layer[0]
}

fn pow7_var(t: &FpVar<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    let t2 = t * t;
    let t4 = &t2 * &t2;
    Ok(&t4 * &t2 * t)
}

pub fn mimc7_var(x: &FpVar<Fr>, k: &FpVar<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    let mut s = x.clone();
    for c in constants() {
        let t = &s + k + FpVar::constant(*c);
        s = pow7_var(&t)?;
    }
    Ok(s + k)
}

pub fn mimc2_var(a: &FpVar<Fr>, b: &FpVar<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    let inner = mimc7_var(a, &FpVar::zero())?;
    mimc7_var(&(&inner + b), &FpVar::zero())
}

pub fn merkle8_var(leaves: &[FpVar<Fr>; 8]) -> Result<FpVar<Fr>, SynthesisError> {
    let mut layer: Vec<FpVar<Fr>> = leaves.to_vec();
    while layer.len() > 1 {
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            next.push(mimc2_var(&pair[0], &pair[1])?);
        }
        layer = next;
    }
    Ok(layer.pop().unwrap())
}

/// Low 128 bits of a field element (LE limb). Fits in BabyJubJub scalar field.
pub fn mask128(x: Fr) -> Fr {
    let bytes = x.into_bigint().to_bytes_le();
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&bytes[..16]);
    Fr::from_le_bytes_mod_order(&out)
}

pub fn mask128_var(x: &FpVar<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    let bits = x.to_bits_le()?;
    Boolean::le_bits_to_fp_var(&bits[..128])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ff::One;

    #[test]
    fn mimc_not_zero_on_zero() {
        assert_ne!(mimc7(Fr::zero(), Fr::zero()), Fr::zero());
        assert_ne!(mimc2(Fr::zero(), Fr::one()), Fr::zero());
    }

    #[test]
    fn merkle8_changes_with_leaf() {
        let a = [Fr::zero(); 8];
        let mut b = [Fr::zero(); 8];
        b[0] = Fr::from(7u64);
        assert_ne!(merkle8(&a), merkle8(&b));
    }

    #[test]
    fn mask128_clears_high() {
        let x = Fr::from_le_bytes_mod_order(&[0xff; 32]);
        let m = mask128(x);
        let bytes = m.into_bigint().to_bytes_le();
        assert!(bytes[16..].iter().all(|b| *b == 0));
    }
}
