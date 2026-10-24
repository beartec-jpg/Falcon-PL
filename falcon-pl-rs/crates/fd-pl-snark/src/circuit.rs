//! Groth16 R1CS: k-of-n BabyJubJub Schnorr over an FPL header statement.
//!
//! Public: height, parent_hi/lo, header_hi/lo, claim_hi/lo, keyRoot, threshold, network_id.
//! Private: 8 committee points + up to 8 signatures (enabled flags).
//! This is NOT Falcon-512-in-circuit. Prover is permissionless given k signatures.

use crate::codec::N_PUBLIC;
use crate::mimc::{mask128_var, merkle8_var, mimc2_var};
use crate::schnorr::{committee_key_root, statement_digest, LcPublic, LcSig, MAX_LEAVES};
use ark_bn254::Fr;
use ark_ed_on_bn254::constraints::EdwardsVar;
use ark_ed_on_bn254::{EdwardsAffine, Fr as BabyFr};
use ark_ec::AffineRepr;
use ark_ff::{BigInteger, PrimeField, Zero};
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::groups::CurveVar;
use ark_r1cs_std::prelude::*;
use ark_r1cs_std::select::CondSelectGadget;
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

pub const BABY_BITS: usize = BabyFr::MODULUS_BIT_SIZE as usize;

#[derive(Clone)]
pub struct FplLcCircuit {
    pub height: Fr,
    pub parent_hi: Fr,
    pub parent_lo: Fr,
    pub header_hi: Fr,
    pub header_lo: Fr,
    pub claim_hi: Fr,
    pub claim_lo: Fr,
    pub key_root: Fr,
    pub threshold: Fr,
    pub network_id: Fr,
    pub pks: [EdwardsAffine; MAX_LEAVES],
    pub enabled: [bool; MAX_LEAVES],
    pub r: [EdwardsAffine; MAX_LEAVES],
    pub s_bits: [Vec<bool>; MAX_LEAVES],
}

impl Default for FplLcCircuit {
    fn default() -> Self {
        Self {
            height: Fr::zero(),
            parent_hi: Fr::zero(),
            parent_lo: Fr::zero(),
            header_hi: Fr::zero(),
            header_lo: Fr::zero(),
            claim_hi: Fr::zero(),
            claim_lo: Fr::zero(),
            key_root: Fr::zero(),
            threshold: Fr::from(4u64),
            network_id: Fr::from(2300u64),
            pks: [EdwardsAffine::zero(); MAX_LEAVES],
            enabled: [false; MAX_LEAVES],
            r: [EdwardsAffine::zero(); MAX_LEAVES],
            s_bits: std::array::from_fn(|_| vec![false; BABY_BITS]),
        }
    }
}

fn baby_bits(s: BabyFr) -> Vec<bool> {
    let mut bits = s.into_bigint().to_bits_le();
    bits.resize(BABY_BITS, false);
    bits.truncate(BABY_BITS);
    bits
}

impl FplLcCircuit {
    pub fn from_committee(
        height: u64,
        parent: &[u8; 32],
        header: &[u8; 32],
        claim_root: &[u8; 32],
        network_id: u32,
        threshold: u64,
        committee: &[LcPublic],
        sigs: &[LcSig],
    ) -> Result<Self, String> {
        if threshold == 0 || threshold as usize > MAX_LEAVES {
            return Err("threshold".into());
        }
        let padded = crate::schnorr::pad_committee(committee.to_vec());
        let key_root = committee_key_root(&padded)?;
        let pubs = crate::codec::public_inputs(
            height,
            parent,
            header,
            claim_root,
            key_root,
            threshold,
            network_id,
        );
        let stmt = statement_digest(height, parent, header, claim_root, network_id);
        let mut c = Self {
            height: pubs[0],
            parent_hi: pubs[1],
            parent_lo: pubs[2],
            header_hi: pubs[3],
            header_lo: pubs[4],
            claim_hi: pubs[5],
            claim_lo: pubs[6],
            key_root: pubs[7],
            threshold: pubs[8],
            network_id: pubs[9],
            ..Self::default()
        };
        for (i, pk) in padded.iter().enumerate() {
            c.pks[i] = pk.pk()?;
        }
        let mut used = 0usize;
        for sig in sigs {
            if !sig.verify(stmt) {
                return Err(format!("bad LC sig for {}", sig.id));
            }
            let Some(idx) = padded.iter().position(|p| p.id == sig.id) else {
                return Err(format!("sig id {} not in committee", sig.id));
            };
            if c.enabled[idx] {
                return Err(format!("duplicate sig {}", sig.id));
            }
            c.enabled[idx] = true;
            c.r[idx] = sig.r()?;
            c.s_bits[idx] = baby_bits(sig.s_fr()?);
            used += 1;
        }
        if used < threshold as usize {
            return Err(format!("need {threshold} LC sigs, got {used}"));
        }
        Ok(c)
    }

    pub fn public_values(&self) -> Vec<Fr> {
        vec![
            self.height,
            self.parent_hi,
            self.parent_lo,
            self.header_hi,
            self.header_lo,
            self.claim_hi,
            self.claim_lo,
            self.key_root,
            self.threshold,
            self.network_id,
        ]
    }
}

fn geq_small(sum: &FpVar<Fr>, thr: &FpVar<Fr>) -> Result<Boolean<Fr>, SynthesisError> {
    let mut acc = Boolean::FALSE;
    for v in 0..=MAX_LEAVES as u64 {
        let is_sum = sum.is_eq(&FpVar::constant(Fr::from(v)))?;
        let mut t_le = Boolean::FALSE;
        for u in 0..=v {
            t_le = t_le.or(&thr.is_eq(&FpVar::constant(Fr::from(u)))?)?;
        }
        acc = acc.or(&is_sum.and(&t_le)?)?;
    }
    Ok(acc)
}

impl ConstraintSynthesizer<Fr> for FplLcCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let height = FpVar::new_input(cs.clone(), || Ok(self.height))?;
        let parent_hi = FpVar::new_input(cs.clone(), || Ok(self.parent_hi))?;
        let parent_lo = FpVar::new_input(cs.clone(), || Ok(self.parent_lo))?;
        let header_hi = FpVar::new_input(cs.clone(), || Ok(self.header_hi))?;
        let header_lo = FpVar::new_input(cs.clone(), || Ok(self.header_lo))?;
        let claim_hi = FpVar::new_input(cs.clone(), || Ok(self.claim_hi))?;
        let claim_lo = FpVar::new_input(cs.clone(), || Ok(self.claim_lo))?;
        let key_root = FpVar::new_input(cs.clone(), || Ok(self.key_root))?;
        let threshold = FpVar::new_input(cs.clone(), || Ok(self.threshold))?;
        let network_id = FpVar::new_input(cs.clone(), || Ok(self.network_id))?;
        debug_assert_eq!(N_PUBLIC, 10);

        let mut stmt = height;
        for x in [
            &parent_hi,
            &parent_lo,
            &header_hi,
            &header_lo,
            &claim_hi,
            &claim_lo,
        ] {
            stmt = mimc2_var(&stmt, x)?;
        }
        stmt = mimc2_var(&stmt, &network_id)?;

        let g = EdwardsVar::new_constant(cs.clone(), EdwardsAffine::generator())?;
        let mut leaves: Vec<FpVar<Fr>> = Vec::with_capacity(MAX_LEAVES);
        let mut enabled_sum = FpVar::<Fr>::zero();
        let id_point = EdwardsVar::new_constant(cs.clone(), EdwardsAffine::zero())?;

        for i in 0..MAX_LEAVES {
            let enabled = Boolean::new_witness(cs.clone(), || Ok(self.enabled[i]))?;
            enabled_sum += FpVar::from(enabled.clone());

            let p = EdwardsVar::new_witness(cs.clone(), || Ok(self.pks[i]))?;
            let r = EdwardsVar::new_witness(cs.clone(), || Ok(self.r[i]))?;

            let mut s_bits = Vec::with_capacity(BABY_BITS);
            for b in &self.s_bits[i] {
                s_bits.push(Boolean::new_witness(cs.clone(), || Ok(*b))?);
            }
            let sg = g.scalar_mul_le(s_bits.iter())?;

            let px = p.x.clone();
            let py = p.y.clone();
            let rx = r.x.clone();
            let e = mask128_var(&mimc2_var(&mimc2_var(&rx, &px)?, &stmt)?)?;
            let e_bits = e.to_bits_le()?;
            let ep = p.scalar_mul_le(e_bits.iter().take(128))?;
            let rhs = &r + &ep;
            let expected = EdwardsVar::conditionally_select(&enabled, &rhs, &sg)?;
            sg.enforce_equal(&expected)?;

            let is_id = p.is_eq(&id_point)?;
            enabled.and(&is_id)?.enforce_equal(&Boolean::FALSE)?;

            leaves.push(mimc2_var(&px, &py)?);
        }

        let leaf_arr: [FpVar<Fr>; MAX_LEAVES] =
            leaves.try_into().map_err(|_| SynthesisError::Unsatisfiable)?;
        let root = merkle8_var(&leaf_arr)?;
        root.enforce_equal(&key_root)?;

        geq_small(&enabled_sum, &threshold)?.enforce_equal(&Boolean::TRUE)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schnorr::{generate, statement_digest};
    use ark_relations::r1cs::ConstraintSystem;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn make(n_sigs: usize) -> FplLcCircuit {
        let mut rng = ChaCha20Rng::from_seed([3u8; 32]);
        let secs: Vec<_> = (1..=6).map(|i| generate(&format!("v{i}"), &mut rng)).collect();
        let pubs: Vec<_> = secs.iter().map(|s| s.public()).collect();
        let parent = [4u8; 32];
        let header = [5u8; 32];
        let claim = [6u8; 32];
        let stmt = statement_digest(1, &parent, &header, &claim, 2300);
        let sigs: Vec<_> = secs[..n_sigs]
            .iter()
            .map(|s| s.sign_stmt(stmt, &mut rng).unwrap())
            .collect();
        FplLcCircuit::from_committee(1, &parent, &header, &claim, 2300, 4, &pubs, &sigs)
            .expect("circuit")
    }

    #[test]
    fn r1cs_satisfied_for_4_sigs() {
        let cs = ConstraintSystem::<Fr>::new_ref();
        make(4).generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
        eprintln!(
            "r1cs constraints={} instance={} witnesses={}",
            cs.num_constraints(),
            cs.num_instance_variables(),
            cs.num_witness_variables()
        );
    }

    #[test]
    fn from_committee_rejects_under_threshold() {
        let mut rng = ChaCha20Rng::from_seed([3u8; 32]);
        let secs: Vec<_> = (1..=6).map(|i| generate(&format!("v{i}"), &mut rng)).collect();
        let pubs: Vec<_> = secs.iter().map(|s| s.public()).collect();
        let parent = [4u8; 32];
        let header = [5u8; 32];
        let claim = [6u8; 32];
        let stmt = statement_digest(1, &parent, &header, &claim, 2300);
        let sigs: Vec<_> = secs[..3]
            .iter()
            .map(|s| s.sign_stmt(stmt, &mut rng).unwrap())
            .collect();
        let err = match FplLcCircuit::from_committee(
            1, &parent, &header, &claim, 2300, 4, &pubs, &sigs,
        ) {
            Ok(_) => panic!("expected under-threshold"),
            Err(e) => e,
        };
        assert!(err.contains("need 4"), "{err}");
    }
}
