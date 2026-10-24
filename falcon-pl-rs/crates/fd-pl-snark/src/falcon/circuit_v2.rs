//! OPEN=1 Falcon-512 QC circuit v2: statement is outer hash (no free claim).
//! Public IO length 16. Next-6 pk leaves are public.

use super::circuit::{mux8, one_hot, FalconOpen};
use super::gadget::{
    alloc_bytes, alloc_h, enforce_num_bits, falcon_verify_var, pk_leaf_var, s2_as_q_and_sq,
    statement_bytes_v2_var,
};
use super::native::{
    committee_key_root, dummy_leaf, extract_witness, pad_pks, pk_leaf, statement_bytes_v2,
    FalconPublic, FalconSig, N, Q,
};
use crate::codec::{public_inputs_v2, N_PUBLIC_V2};
use crate::mimc::merkle8_var;
use crate::schnorr::MAX_LEAVES;
use ark_bn254::Fr;
use ark_ff::Zero;
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::prelude::*;
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

#[derive(Clone)]
pub struct FplFalconQcCircuitV2 {
    pub height: Fr,
    pub parent_hi: Fr,
    pub parent_lo: Fr,
    pub header_hi: Fr,
    pub header_lo: Fr,
    pub current_root: Fr,
    pub next_root: Fr,
    pub threshold: Fr,
    pub network_id: Fr,
    pub signer_index: Fr,
    pub next_leaves: [Fr; 6],
    pub leaves: [Fr; MAX_LEAVES],
    pub open: FalconOpen,
}

impl Default for FplFalconQcCircuitV2 {
    fn default() -> Self {
        Self {
            height: Fr::zero(),
            parent_hi: Fr::zero(),
            parent_lo: Fr::zero(),
            header_hi: Fr::zero(),
            header_lo: Fr::zero(),
            current_root: Fr::zero(),
            next_root: Fr::zero(),
            threshold: Fr::from(1u64),
            network_id: Fr::from(2300u64),
            signer_index: Fr::zero(),
            next_leaves: [Fr::zero(); 6],
            leaves: [Fr::zero(); MAX_LEAVES],
            open: FalconOpen::default(),
        }
    }
}

impl FplFalconQcCircuitV2 {
    pub fn from_committee(
        height: u64,
        parent: &[u8; 32],
        header: &[u8; 32],
        network_id: u32,
        current: &[FalconPublic],
        next: &[FalconPublic],
        sig: &FalconSig,
    ) -> Result<Self, String> {
        if current.is_empty() || current.len() > 6 {
            return Err("current committee 1..=6".into());
        }
        if next.len() != 6 {
            return Err("next committee must be 6".into());
        }
        let msg = statement_bytes_v2(height, parent, header, network_id);
        let mut hs = Vec::new();
        for pk in current {
            let raw = hex::decode(pk.pk_hex.trim().trim_start_matches("0x"))
                .map_err(|e| e.to_string())?;
            hs.push(super::native::decode_pk(&raw)?);
        }
        let current_root = committee_key_root(&hs)?;
        let padded = pad_pks(hs.clone());
        let mut leaves = [dummy_leaf(); MAX_LEAVES];
        for (i, h) in padded.iter().enumerate().take(MAX_LEAVES) {
            leaves[i] = pk_leaf(h);
        }
        let mut next_hs = Vec::new();
        for pk in next {
            let raw = hex::decode(pk.pk_hex.trim().trim_start_matches("0x"))
                .map_err(|e| e.to_string())?;
            next_hs.push(super::native::decode_pk(&raw)?);
        }
        let next_root = committee_key_root(&next_hs)?;
        let mut next_leaves = [dummy_leaf(); 6];
        for (i, h) in next_hs.iter().enumerate().take(6) {
            next_leaves[i] = pk_leaf(h);
        }
        let idx = current
            .iter()
            .position(|p| p.id == sig.id)
            .ok_or_else(|| format!("sig id {} not in current committee", sig.id))?;
        if idx >= 6 {
            return Err(format!("signerIndex {idx} >= 6"));
        }
        let pk = hex::decode(current[idx].pk_hex.trim().trim_start_matches("0x"))
            .map_err(|e| e.to_string())?;
        let sg = hex::decode(sig.sig_hex.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
        let w = extract_witness(&pk, &sg, &msg)?;
        let pubs = public_inputs_v2(
            height,
            parent,
            header,
            current_root,
            next_root,
            1,
            network_id,
            idx as u64,
            &next_leaves,
        );
        Ok(Self {
            height: pubs[0],
            parent_hi: pubs[1],
            parent_lo: pubs[2],
            header_hi: pubs[3],
            header_lo: pubs[4],
            current_root: pubs[5],
            next_root: pubs[6],
            threshold: pubs[7],
            network_id: pubs[8],
            signer_index: pubs[9],
            next_leaves,
            leaves,
            open: FalconOpen {
                index: idx,
                h: w.h,
                s2: w.s2,
                nonce: w.nonce,
            },
        })
    }

    pub fn public_values(&self) -> Vec<Fr> {
        let mut v = vec![
            self.height,
            self.parent_hi,
            self.parent_lo,
            self.header_hi,
            self.header_lo,
            self.current_root,
            self.next_root,
            self.threshold,
            self.network_id,
            self.signer_index,
        ];
        v.extend_from_slice(&self.next_leaves);
        debug_assert_eq!(v.len(), N_PUBLIC_V2);
        v
    }
}

impl ConstraintSynthesizer<Fr> for FplFalconQcCircuitV2 {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let height = FpVar::new_input(cs.clone(), || Ok(self.height))?;
        let parent_hi = FpVar::new_input(cs.clone(), || Ok(self.parent_hi))?;
        let parent_lo = FpVar::new_input(cs.clone(), || Ok(self.parent_lo))?;
        let header_hi = FpVar::new_input(cs.clone(), || Ok(self.header_hi))?;
        let header_lo = FpVar::new_input(cs.clone(), || Ok(self.header_lo))?;
        let current_root = FpVar::new_input(cs.clone(), || Ok(self.current_root))?;
        let next_root = FpVar::new_input(cs.clone(), || Ok(self.next_root))?;
        let threshold = FpVar::new_input(cs.clone(), || Ok(self.threshold))?;
        let network_id = FpVar::new_input(cs.clone(), || Ok(self.network_id))?;
        let signer_index = FpVar::new_input(cs.clone(), || Ok(self.signer_index))?;
        let mut next_pub = Vec::with_capacity(6);
        for i in 0..6 {
            next_pub.push(FpVar::new_input(cs.clone(), || Ok(self.next_leaves[i]))?);
        }
        debug_assert_eq!(N_PUBLIC_V2, 16);

        let stmt = statement_bytes_v2_var(
            &height,
            &parent_hi,
            &parent_lo,
            &header_hi,
            &header_lo,
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
        root.enforce_equal(&current_root)?;

        let dummy = FpVar::constant(dummy_leaf());
        let sel = one_hot(cs.clone(), self.open.index)?;
        let mut recon = FpVar::<Fr>::zero();
        for i in 0..MAX_LEAVES {
            recon += FpVar::from(sel[i].clone()) * FpVar::constant(Fr::from(i as u64));
        }
        recon.enforce_equal(&signer_index)?;
        // signerIndex < 6
        let mut lt6 = Boolean::FALSE;
        for i in 0..6u64 {
            lt6 = lt6.or(&signer_index.is_eq(&FpVar::constant(Fr::from(i)))?)?;
        }
        lt6.enforce_equal(&Boolean::TRUE)?;

        let h = alloc_h(cs.clone(), &self.open.h)?;
        for coeff in &h {
            super::gadget::enforce_mod_q(coeff)?;
        }
        let leaf = pk_leaf_var(&h)?;
        leaf.enforce_equal(&mux8(&leaf_arr, &sel))?;
        leaf.is_eq(&dummy)?.enforce_equal(&Boolean::FALSE)?;

        let mut s2q = Vec::with_capacity(N);
        let mut s2sq = Vec::with_capacity(N);
        for z in &self.open.s2 {
            let (qel, sq, _) = s2_as_q_and_sq(cs.clone(), *z)?;
            s2q.push(qel);
            s2sq.push(sq);
        }
        let s2q: [FpVar<Fr>; N] = s2q.try_into().map_err(|_| SynthesisError::Unsatisfiable)?;
        let s2sq: [FpVar<Fr>; N] = s2sq.try_into().map_err(|_| SynthesisError::Unsatisfiable)?;
        let nonce = alloc_bytes(cs.clone(), &self.open.nonce)?;
        for b in &nonce {
            enforce_num_bits(b, 8)?;
        }
        falcon_verify_var(cs.clone(), &h, &s2q, &s2sq, &nonce, &stmt)?;

        threshold.enforce_equal(&FpVar::constant(Fr::from(1u64)))?;

        let mut next_arr = [(); MAX_LEAVES].map(|_| FpVar::<Fr>::zero());
        for i in 0..6 {
            next_pub[i].is_eq(&dummy)?.enforce_equal(&Boolean::FALSE)?;
            next_arr[i] = next_pub[i].clone();
        }
        next_arr[6] = dummy.clone();
        next_arr[7] = dummy;
        let nroot = merkle8_var(&next_arr)?;
        nroot.enforce_equal(&next_root)?;
        let _ = Q;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_relations::r1cs::ConstraintSystem;
    use pqcrypto_falcon::falcon512;
    use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _};

    fn key_sig(id: &str, msg: &[u8]) -> (FalconPublic, FalconSig) {
        let (pk, sk) = falcon512::keypair();
        let sig = falcon512::detached_sign(msg, &sk);
        (
            FalconPublic {
                id: id.into(),
                pk_hex: hex::encode(pk.as_bytes()),
            },
            FalconSig {
                id: id.into(),
                sig_hex: hex::encode(sig.as_bytes()),
            },
        )
    }

    fn six_keys(msg: &[u8]) -> (Vec<FalconPublic>, FalconSig) {
        let mut pubs = Vec::new();
        let mut first_sig = None;
        for i in 1..=6 {
            let (pk, sig) = key_sig(&format!("v{i}"), msg);
            if i == 1 {
                first_sig = Some(sig);
            }
            pubs.push(pk);
        }
        (pubs, first_sig.unwrap())
    }

    #[test]
    fn v2_one_sig_satisfies_r1cs() {
        let parent = [4u8; 32];
        let header = [5u8; 32];
        let msg = statement_bytes_v2(7, &parent, &header, 2300);
        let (pubs, sig) = six_keys(&msg);
        let circuit =
            FplFalconQcCircuitV2::from_committee(7, &parent, &header, 2300, &pubs, &pubs, &sig)
                .expect("v2 circuit");
        assert_eq!(circuit.public_values().len(), 16);
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap(), "v2 R1CS unsatisfied");
    }
}
