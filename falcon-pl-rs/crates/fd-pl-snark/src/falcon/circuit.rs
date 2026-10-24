//! Groth16 R1CS: k Falcon-512 signatures over the dest-lock header statement.
//!
//! Public IO matches `FplLcCircuit` (same Solidity `_lcInputs` order).
//! `key_root` is the MiMC merkle of 8 packed Falcon-512 public keys.

use super::gadget::{
    alloc_bytes, alloc_h, enforce_num_bits, falcon_verify_var, pk_leaf_var, s2_as_q_and_sq,
    statement_bytes_var,
};
use super::native::{
    committee_key_root, dummy_leaf, extract_witness, pad_pks, pk_leaf, statement_bytes,
    FalconPublic, FalconSig, N, NONCE_LEN, Q,
};
use crate::codec::{public_inputs, N_PUBLIC};
use crate::mimc::merkle8_var;
use crate::schnorr::MAX_LEAVES;
use ark_bn254::Fr;
use ark_ff::Zero;
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::prelude::*;
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

/// Opened Falcon signatures in the QC circuit (live dest-lock threshold).
pub const MAX_OPEN: usize = 4;

#[derive(Clone)]
pub struct FalconOpen {
    pub index: usize,
    pub h: [u16; N],
    pub s2: [i16; N],
    pub nonce: [u8; NONCE_LEN],
}

impl Default for FalconOpen {
    fn default() -> Self {
        Self {
            index: 0,
            h: [0u16; N],
            s2: [0i16; N],
            nonce: [0u8; NONCE_LEN],
        }
    }
}

#[derive(Clone)]
pub struct FplFalconQcCircuit<const OPEN: usize = MAX_OPEN> {
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
    pub leaves: [Fr; MAX_LEAVES],
    pub opens: [FalconOpen; OPEN],
}

impl<const OPEN: usize> Default for FplFalconQcCircuit<OPEN> {
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
            threshold: Fr::from(OPEN as u64),
            network_id: Fr::from(2300u64),
            leaves: [Fr::zero(); MAX_LEAVES],
            opens: std::array::from_fn(|_| FalconOpen::default()),
        }
    }
}

impl<const OPEN: usize> FplFalconQcCircuit<OPEN> {
    pub fn from_committee(
        height: u64,
        parent: &[u8; 32],
        header: &[u8; 32],
        claim_root: &[u8; 32],
        network_id: u32,
        threshold: u64,
        committee: &[FalconPublic],
        sigs: &[FalconSig],
    ) -> Result<Self, String> {
        if OPEN == 0 || OPEN > MAX_LEAVES {
            return Err("OPEN".into());
        }
        if threshold as usize > OPEN {
            return Err(format!("threshold {threshold} > OPEN {OPEN}"));
        }
        if sigs.len() < OPEN {
            return Err(format!("need {OPEN} Falcon sigs, got {}", sigs.len()));
        }
        let msg = statement_bytes(height, parent, header, claim_root, network_id);
        let mut hs = Vec::new();
        for pk in committee {
            let raw = hex::decode(pk.pk_hex.trim().trim_start_matches("0x"))
                .map_err(|e| e.to_string())?;
            hs.push(super::native::decode_pk(&raw)?);
        }
        if hs.is_empty() {
            return Err("empty committee".into());
        }
        let padded = pad_pks(hs.clone());
        let key_root = committee_key_root(&hs)?;
        let pubs = public_inputs(
            height,
            parent,
            header,
            claim_root,
            key_root,
            threshold,
            network_id,
        );
        let mut leaves = [dummy_leaf(); MAX_LEAVES];
        for (i, h) in padded.iter().enumerate().take(MAX_LEAVES) {
            leaves[i] = pk_leaf(h);
        }
        let mut opens: Vec<FalconOpen> = Vec::new();
        let mut used = vec![false; MAX_LEAVES];
        for sig in sigs.iter().take(OPEN) {
            let Some(idx) = committee.iter().position(|p| p.id == sig.id) else {
                return Err(format!("sig id {} not in committee", sig.id));
            };
            if idx >= MAX_LEAVES || used[idx] {
                return Err(format!("bad/duplicate index {idx}"));
            }
            used[idx] = true;
            let pk = hex::decode(committee[idx].pk_hex.trim().trim_start_matches("0x"))
                .map_err(|e| e.to_string())?;
            let sg = hex::decode(sig.sig_hex.trim().trim_start_matches("0x"))
                .map_err(|e| e.to_string())?;
            let w = extract_witness(&pk, &sg, &msg)?;
            opens.push(FalconOpen {
                index: idx,
                h: w.h,
                s2: w.s2,
                nonce: w.nonce,
            });
        }
        if opens.len() < OPEN {
            return Err(format!("need {OPEN} opens"));
        }
        let opens: [FalconOpen; OPEN] = opens
            .try_into()
            .map_err(|_| "open array".to_string())?;
        Ok(Self {
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
            leaves,
            opens,
        })
    }

    pub fn public_values(&self) -> Vec<Fr> {
        let mut v = vec![
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
        ];
        // OPEN=1: 11th public is the signer leaf index so Ethereum can require
        // four distinct Falcon keys (one proof each).
        if OPEN == 1 {
            v.push(Fr::from(self.opens[0].index as u64));
        }
        v
    }
}

pub(crate) fn one_hot(
    cs: ConstraintSystemRef<Fr>,
    index: usize,
) -> Result<[Boolean<Fr>; MAX_LEAVES], SynthesisError> {
    let mut sel = Vec::with_capacity(MAX_LEAVES);
    let mut sum = FpVar::<Fr>::zero();
    for i in 0..MAX_LEAVES {
        let b = Boolean::new_witness(cs.clone(), || Ok(i == index))?;
        sum += FpVar::from(b.clone());
        sel.push(b);
    }
    sum.enforce_equal(&FpVar::one())?;
    sel.try_into().map_err(|_| SynthesisError::Unsatisfiable)
}

pub(crate) fn mux8(leaves: &[FpVar<Fr>; MAX_LEAVES], sel: &[Boolean<Fr>; MAX_LEAVES]) -> FpVar<Fr> {
    let mut acc = FpVar::<Fr>::zero();
    for i in 0..MAX_LEAVES {
        acc += FpVar::from(sel[i].clone()) * &leaves[i];
    }
    acc
}

impl<const OPEN: usize> ConstraintSynthesizer<Fr> for FplFalconQcCircuit<OPEN> {
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
        let open_index = if OPEN == 1 {
            Some(FpVar::new_input(cs.clone(), || {
                Ok(Fr::from(self.opens[0].index as u64))
            })?)
        } else {
            None
        };
        debug_assert_eq!(N_PUBLIC, 10);

        let stmt = statement_bytes_var(
            &height,
            &parent_hi,
            &parent_lo,
            &header_hi,
            &header_lo,
            &claim_hi,
            &claim_lo,
            &network_id,
        )?;

        let mut leaves_v = Vec::with_capacity(MAX_LEAVES);
        for i in 0..MAX_LEAVES {
            leaves_v.push(FpVar::new_witness(cs.clone(), || Ok(self.leaves[i]))?);
        }
        let leaf_arr: [FpVar<Fr>; MAX_LEAVES] = leaves_v
            .try_into()
            .map_err(|_| SynthesisError::Unsatisfiable)?;
        let root = merkle8_var(&leaf_arr)?;
        root.enforce_equal(&key_root)?;

        let dummy = FpVar::constant(dummy_leaf());
        let mut col_sum: [FpVar<Fr>; MAX_LEAVES] = std::array::from_fn(|_| FpVar::zero());
        for open in &self.opens {
            let sel = one_hot(cs.clone(), open.index)?;
            if let Some(idx) = open_index.as_ref() {
                let mut recon = FpVar::<Fr>::zero();
                for i in 0..MAX_LEAVES {
                    recon += FpVar::from(sel[i].clone()) * FpVar::constant(Fr::from(i as u64));
                }
                recon.enforce_equal(idx)?;
            }
            for i in 0..MAX_LEAVES {
                col_sum[i] += FpVar::from(sel[i].clone());
            }
            let h = alloc_h(cs.clone(), &open.h)?;
            for coeff in &h {
                super::gadget::enforce_mod_q(coeff)?;
            }
            let leaf = pk_leaf_var(&h)?;
            leaf.enforce_equal(&mux8(&leaf_arr, &sel))?;
            leaf.is_eq(&dummy)?.enforce_equal(&Boolean::FALSE)?;

            let mut s2q = Vec::with_capacity(N);
            let mut s2sq = Vec::with_capacity(N);
            for z in &open.s2 {
                let (qel, sq, _) = s2_as_q_and_sq(cs.clone(), *z)?;
                s2q.push(qel);
                s2sq.push(sq);
            }
            let s2q: [FpVar<Fr>; N] = s2q.try_into().map_err(|_| SynthesisError::Unsatisfiable)?;
            let s2sq: [FpVar<Fr>; N] = s2sq.try_into().map_err(|_| SynthesisError::Unsatisfiable)?;
            let nonce = alloc_bytes(cs.clone(), &open.nonce)?;
            for b in &nonce {
                enforce_num_bits(b, 8)?;
            }
            falcon_verify_var(cs.clone(), &h, &s2q, &s2sq, &nonce, &stmt)?;
        }
        for s in &col_sum {
            // each leaf used at most once: s ∈ {0,1}
            let is0 = s.is_eq(&FpVar::zero())?;
            let is1 = s.is_eq(&FpVar::one())?;
            is0.or(&is1)?.enforce_equal(&Boolean::TRUE)?;
        }

        // OPEN >= threshold, threshold in 1..=OPEN.
        let mut ge = Boolean::FALSE;
        for t in 1..=OPEN as u64 {
            let is_t = threshold.is_eq(&FpVar::constant(Fr::from(t)))?;
            if OPEN as u64 >= t {
                ge = ge.or(&is_t)?;
            }
        }
        ge.enforce_equal(&Boolean::TRUE)?;
        let _ = Q;
        Ok(())
    }
}

pub type FplFalconOneCircuit = FplFalconQcCircuit<1>;

#[cfg(test)]
mod tests {
    use super::*;
    use ark_relations::r1cs::ConstraintSystem;
    use pqcrypto_falcon::falcon512;
    use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _};

    fn one_key_sig(msg: &[u8]) -> (FalconPublic, FalconSig) {
        let (pk, sk) = falcon512::keypair();
        let sig = falcon512::detached_sign(msg, &sk);
        (
            FalconPublic {
                id: "v1".into(),
                pk_hex: hex::encode(pk.as_bytes()),
            },
            FalconSig {
                id: "v1".into(),
                sig_hex: hex::encode(sig.as_bytes()),
            },
        )
    }

    #[test]
    fn one_native_falcon_satisfies_r1cs() {
        let parent = [4u8; 32];
        let header = [5u8; 32];
        let claim = [6u8; 32];
        let msg = statement_bytes(7, &parent, &header, &claim, 2300);
        let (pk, sig) = one_key_sig(&msg);
        let circuit = FplFalconOneCircuit::from_committee(
            7, &parent, &header, &claim, 2300, 1, &[pk], &[sig],
        )
        .expect("one-sig circuit");
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(
            cs.is_satisfied().unwrap(),
            "native Falcon-512 signature must satisfy R1CS"
        );
        eprintln!(
            "falcon-512 one-sig constraints={} instance={} witnesses={}",
            cs.num_constraints(),
            cs.num_instance_variables(),
            cs.num_witness_variables()
        );
    }

    #[test]
    fn one_sig_rejects_wrong_message() {
        let parent = [4u8; 32];
        let header = [5u8; 32];
        let claim = [6u8; 32];
        let msg = statement_bytes(7, &parent, &header, &claim, 2300);
        let (pk, sig) = one_key_sig(&msg);
        let err = FplFalconOneCircuit::from_committee(
            8, &parent, &header, &claim, 2300, 1, &[pk], &[sig],
        );
        assert!(err.is_err(), "wrong height must fail witness extract");
    }

    fn keys_sigs(
        n_keys: usize,
        n_sigs: usize,
        msg: &[u8],
    ) -> (Vec<FalconPublic>, Vec<FalconSig>) {
        let mut pubs = Vec::new();
        let mut sigs = Vec::new();
        for i in 1..=n_keys {
            let (pk, sk) = falcon512::keypair();
            let id = format!("v{i}");
            pubs.push(FalconPublic {
                id: id.clone(),
                pk_hex: hex::encode(pk.as_bytes()),
            });
            if i <= n_sigs {
                let sig = falcon512::detached_sign(msg, &sk);
                sigs.push(FalconSig {
                    id,
                    sig_hex: hex::encode(sig.as_bytes()),
                });
            }
        }
        (pubs, sigs)
    }

    #[test]
    fn four_of_six_native_falcon_satisfies_r1cs() {
        let parent = [4u8; 32];
        let header = [5u8; 32];
        let claim = [6u8; 32];
        let msg = statement_bytes(7, &parent, &header, &claim, 2300);
        let (pubs, sigs) = keys_sigs(6, 4, &msg);
        let circuit = FplFalconQcCircuit::<4>::from_committee(
            7, &parent, &header, &claim, 2300, 4, &pubs, &sigs,
        )
        .expect("4-of-6 circuit");
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(
            cs.is_satisfied().unwrap(),
            "4 native Falcon-512 signatures must satisfy R1CS"
        );
        eprintln!(
            "falcon-512 4-of-6 constraints={} instance={} witnesses={}",
            cs.num_constraints(),
            cs.num_instance_variables(),
            cs.num_witness_variables()
        );
    }

    #[test]
    fn four_of_six_rejects_three_sigs() {
        let parent = [4u8; 32];
        let header = [5u8; 32];
        let claim = [6u8; 32];
        let msg = statement_bytes(7, &parent, &header, &claim, 2300);
        let (pubs, sigs) = keys_sigs(6, 3, &msg);
        let err = FplFalconQcCircuit::<4>::from_committee(
            7, &parent, &header, &claim, 2300, 4, &pubs, &sigs,
        );
        assert!(err.is_err(), "3 sigs must fail OPEN=4");
    }
}
