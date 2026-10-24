//! Groth16 R1CS: `FplWithdrawValid` — withdraw leaf ∈ claim merkle root + bindings.

use crate::abi::{domain_fr, note_id_fr, test_vk_hash, MERKLE_DEPTH};
use crate::leaf::{dest20_to_bytes32, leaf_to_bytes32, withdraw_leaf_fr};
use ark_bn254::Fr;
use ark_ff::{Field, Zero};
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::prelude::*;
use ark_r1cs_std::select::CondSelectGadget;
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use fd_pl_snark::codec::split_hash;
use fd_pl_snark::mimc::mimc2_var;

#[derive(Clone)]
pub struct FplWithdrawValid {
    pub network_id: Fr,
    pub amount: Fr,
    pub dest_hi: Fr,
    pub dest_lo: Fr,
    pub claim_root_hi: Fr,
    pub claim_root_lo: Fr,
    pub height: Fr,
    pub header_hi: Fr,
    pub header_lo: Fr,
    pub note_id: Fr,
    pub pegout_hi: Fr,
    pub pegout_lo: Fr,
    pub vk_hash: Fr,
    pub path: [Fr; MERKLE_DEPTH],
    pub path_bits: [bool; MERKLE_DEPTH],
}

impl Default for FplWithdrawValid {
    fn default() -> Self {
        Self {
            network_id: Fr::from(2300u64),
            amount: Fr::zero(),
            dest_hi: Fr::zero(),
            dest_lo: Fr::zero(),
            claim_root_hi: Fr::zero(),
            claim_root_lo: Fr::zero(),
            height: Fr::zero(),
            header_hi: Fr::zero(),
            header_lo: Fr::zero(),
            note_id: Fr::zero(),
            pegout_hi: Fr::zero(),
            pegout_lo: Fr::zero(),
            vk_hash: test_vk_hash(),
            path: [Fr::zero(); MERKLE_DEPTH],
            path_bits: [false; MERKLE_DEPTH],
        }
    }
}

impl FplWithdrawValid {
    pub fn from_withdraw(
        network_id: u32,
        amount: u64,
        dest_h160: &[u8; 20],
        claim_root: Fr,
        height: u64,
        header_hash: &[u8; 32],
        note_id: &str,
        pegout_txid: &[u8; 32],
        path: &[Fr],
        path_bits: &[bool],
        vk_hash: Fr,
    ) -> Result<Self, String> {
        if path.len() != MERKLE_DEPTH || path_bits.len() != MERKLE_DEPTH {
            return Err(format!("path must be length {MERKLE_DEPTH}"));
        }
        let dest = dest20_to_bytes32(dest_h160);
        let root_bytes = leaf_to_bytes32(claim_root);
        let (c_hi, c_lo) = split_hash(&root_bytes);
        let (d_hi, d_lo) = split_hash(&dest);
        let (h_hi, h_lo) = split_hash(header_hash);
        let (p_hi, p_lo) = split_hash(pegout_txid);
        let mut path_arr = [Fr::zero(); MERKLE_DEPTH];
        let mut bits_arr = [false; MERKLE_DEPTH];
        path_arr.copy_from_slice(path);
        bits_arr.copy_from_slice(path_bits);
        Ok(Self {
            network_id: Fr::from(network_id),
            amount: Fr::from(amount),
            dest_hi: d_hi,
            dest_lo: d_lo,
            claim_root_hi: c_hi,
            claim_root_lo: c_lo,
            height: Fr::from(height),
            header_hi: h_hi,
            header_lo: h_lo,
            note_id: note_id_fr(note_id),
            pegout_hi: p_hi,
            pegout_lo: p_lo,
            vk_hash,
            path: path_arr,
            path_bits: bits_arr,
        })
    }

    pub fn public_values(&self) -> Vec<Fr> {
        vec![
            self.network_id,
            self.amount,
            self.dest_hi,
            self.dest_lo,
            self.claim_root_hi,
            self.claim_root_lo,
            self.height,
            self.header_hi,
            self.header_lo,
            self.note_id,
            self.pegout_hi,
            self.pegout_lo,
            self.vk_hash,
        ]
    }

    pub fn expected_leaf(note_id: &str, dest_h160: &[u8; 20], amount: u64) -> Fr {
        withdraw_leaf_fr(note_id, dest_h160, amount)
    }
}

fn leaf_var(
    note_id: &FpVar<Fr>,
    dest_hi: &FpVar<Fr>,
    dest_lo: &FpVar<Fr>,
    amount: &FpVar<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let domain = FpVar::constant(domain_fr());
    let inner = mimc2_var(&domain, note_id)?;
    let dest_c = mimc2_var(dest_hi, dest_lo)?;
    let mid = mimc2_var(&inner, &dest_c)?;
    mimc2_var(&mid, amount)
}

impl ConstraintSynthesizer<Fr> for FplWithdrawValid {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        self.synth(cs, false)
    }
}

/// MiMC fold of the 13 V1 public values into one field element (BitVM pack
/// verifies Groth16 with exactly one public input).
pub fn public_digest(values: &[Fr]) -> Fr {
    let mut h = Fr::from(0x46504c32u64); // "FPL2"
    for v in values {
        h = fd_pl_snark::mimc::mimc2(h, *v);
    }
    h
}

/// V2: same statement as V1, but the only public input is `public_digest`
/// of the 13 V1 values (all other values become witnesses). Used for the
/// BitVM split-verifier Disprove graph.
#[derive(Clone)]
pub struct FplWithdrawValidV2(pub FplWithdrawValid);

impl FplWithdrawValidV2 {
    pub fn public_values(&self) -> Vec<Fr> {
        vec![public_digest(&self.0.public_values())]
    }
}

impl ConstraintSynthesizer<Fr> for FplWithdrawValidV2 {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        self.0.synth(cs, true)
    }
}

impl FplWithdrawValid {
    fn synth(self, cs: ConstraintSystemRef<Fr>, single_public: bool) -> Result<(), SynthesisError> {
        let digest_native = if single_public { Some(public_digest(&self.public_values())) } else { None };
        let alloc = |v: Fr| -> Result<FpVar<Fr>, SynthesisError> {
            if single_public { FpVar::new_witness(cs.clone(), || Ok(v)) } else { FpVar::new_input(cs.clone(), || Ok(v)) }
        };
        let digest_var = match digest_native {
            Some(d) => Some(FpVar::new_input(cs.clone(), || Ok(d))?),
            None => None,
        };
        let network = alloc(self.network_id)?;
        let amount = alloc(self.amount)?;
        let dest_hi = alloc(self.dest_hi)?;
        let dest_lo = alloc(self.dest_lo)?;
        let root_hi = alloc(self.claim_root_hi)?;
        let root_lo = alloc(self.claim_root_lo)?;
        let _height = alloc(self.height)?;
        let _header_hi = alloc(self.header_hi)?;
        let _header_lo = alloc(self.header_lo)?;
        let note_id = alloc(self.note_id)?;
        let pegout_hi = alloc(self.pegout_hi)?;
        let pegout_lo = alloc(self.pegout_lo)?;
        let vk = alloc(self.vk_hash)?;

        if let Some(d) = &digest_var {
            let mut h = FpVar::constant(Fr::from(0x46504c32u64));
            for v in [&network, &amount, &dest_hi, &dest_lo, &root_hi, &root_lo, &_height, &_header_hi, &_header_lo, &note_id, &pegout_hi, &pegout_lo, &vk] {
                h = mimc2_var(&h, v)?;
            }
            h.enforce_equal(d)?;
        }
        network.enforce_equal(&FpVar::constant(Fr::from(2300u64)))?;
        vk.enforce_equal(&FpVar::constant(test_vk_hash()))?;

        let leaf = leaf_var(&note_id, &dest_hi, &dest_lo, &amount)?;

        let mut h = leaf;
        for i in 0..MERKLE_DEPTH {
            let sib = FpVar::new_witness(cs.clone(), || Ok(self.path[i]))?;
            let bit = Boolean::new_witness(cs.clone(), || Ok(self.path_bits[i]))?;
            let left = CondSelectGadget::conditionally_select(&bit, &sib, &h)?;
            let right = CondSelectGadget::conditionally_select(&bit, &h, &sib)?;
            h = mimc2_var(&left, &right)?;
        }

        let two128 = Fr::from(2u64).pow([128]);
        let expected = &root_lo + &root_hi * FpVar::constant(two128);
        h.enforce_equal(&expected)?;

        // amount > 0 ⇒ pegout commitment non-zero (fronting txid bound in public IO)
        let amt_zero = amount.is_eq(&FpVar::constant(Fr::zero()))?;
        let peg_sum = &pegout_hi + &pegout_lo;
        let peg_zero = peg_sum.is_eq(&FpVar::constant(Fr::zero()))?;
        let ok = Boolean::kary_or(&[amt_zero, peg_zero.not()])?;
        ok.enforce_equal(&Boolean::TRUE)?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// V3: SHA256 binding (decision 2026-10-18, final layout approved 07:25).
//
// Single public input
//   x = SHA256("FPL3" || txid_display[32] || binding_commit[32]), top 3 bits
//       cleared, read big-endian;
//   binding_commit = SHA256(withdraw_commit[32] || checkpoint[32]);
//   withdraw_commit = big-endian bytes of public_digest(V1 values);
//   txid_display = reverse(pegout_txid internal bytes).
// On Bitcoin the operator Winternitz-commits pegout_txid (internal order) and
// reverse(binding_commit) at kick-off 1; the BitVM binding leaf recomputes x
// with the u4 SHA256 script and disproves on mismatch.
// ---------------------------------------------------------------------------

pub const V3_DOMAIN: &[u8; 4] = b"FPL3";

pub fn fr_to_be32(f: Fr) -> [u8; 32] {
    use ark_ff::{BigInteger, PrimeField};
    let v = f.into_bigint().to_bytes_be();
    let mut o = [0u8; 32];
    o[32 - v.len()..].copy_from_slice(&v);
    o
}

pub fn v3_binding_commit(withdraw_commit: &[u8; 32], checkpoint: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(withdraw_commit);
    h.update(checkpoint);
    h.finalize().into()
}

pub fn v3_message(pegout_txid: &[u8; 32], binding_commit: &[u8; 32]) -> Vec<u8> {
    let mut m = V3_DOMAIN.to_vec();
    let mut d = *pegout_txid;
    d.reverse();
    m.extend_from_slice(&d);
    m.extend_from_slice(binding_commit);
    m
}

/// Native x.
pub fn v3_public_input(pegout_txid: &[u8; 32], withdraw_commit: &[u8; 32], checkpoint: &[u8; 32]) -> Fr {
    use ark_ff::PrimeField;
    use sha2::{Digest, Sha256};
    let b = v3_binding_commit(withdraw_commit, checkpoint);
    let mut d: [u8; 32] = Sha256::digest(v3_message(pegout_txid, &b)).into();
    d[0] &= 0x1f;
    Fr::from_be_bytes_mod_order(&d)
}

#[derive(Clone)]
pub struct FplWithdrawValidV3 {
    pub inner: FplWithdrawValid,
    /// internal byte order (as `Txid::as_byte_array`)
    pub pegout_txid: [u8; 32],
    pub checkpoint: [u8; 32],
}

impl FplWithdrawValidV3 {
    pub fn new(inner: FplWithdrawValid, pegout_txid: [u8; 32], checkpoint: [u8; 32]) -> Result<Self, String> {
        let (p_hi, p_lo) = split_hash(&pegout_txid);
        let (h_hi, h_lo) = split_hash(&checkpoint);
        if (p_hi, p_lo) != (inner.pegout_hi, inner.pegout_lo) || (h_hi, h_lo) != (inner.header_hi, inner.header_lo) {
            return Err("pegout/checkpoint bytes do not match the V1 values".into());
        }
        Ok(Self { inner, pegout_txid, checkpoint })
    }
    pub fn withdraw_commit(&self) -> [u8; 32] {
        fr_to_be32(public_digest(&self.inner.public_values()))
    }
    pub fn binding_commit(&self) -> [u8; 32] {
        v3_binding_commit(&self.withdraw_commit(), &self.checkpoint)
    }
    pub fn public_values(&self) -> Vec<Fr> {
        vec![v3_public_input(&self.pegout_txid, &self.withdraw_commit(), &self.checkpoint)]
    }
}

fn be_bytes_to_fp(bytes: &[UInt8<Fr>]) -> Result<FpVar<Fr>, SynthesisError> {
    let mut bits = vec![];
    for b in bytes.iter().rev() {
        bits.extend(b.to_bits_le()?);
    }
    Boolean::le_bits_to_fp_var(&bits)
}

impl ConstraintSynthesizer<Fr> for FplWithdrawValidV3 {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        use ark_crypto_primitives::crh::sha256::constraints::Sha256Gadget;
        let x_native = self.public_values()[0];
        let x = FpVar::new_input(cs.clone(), || Ok(x_native))?;
        let v = &self.inner;
        let w = |f: Fr| FpVar::new_witness(cs.clone(), || Ok(f));
        let vals: Vec<FpVar<Fr>> = v.public_values().into_iter().map(w).collect::<Result<_, _>>()?;
        let [network, amount, dest_hi, dest_lo, root_hi, root_lo, _height, header_hi, header_lo, note_id, pegout_hi, pegout_lo, vk]: [FpVar<Fr>; 13] =
            vals.clone().try_into().map_err(|_| SynthesisError::Unsatisfiable)?;

        network.enforce_equal(&FpVar::constant(Fr::from(2300u64)))?;
        vk.enforce_equal(&FpVar::constant(test_vk_hash()))?;
        let mut h = leaf_var(&note_id, &dest_hi, &dest_lo, &amount)?;
        for i in 0..MERKLE_DEPTH {
            let sib = FpVar::new_witness(cs.clone(), || Ok(v.path[i]))?;
            let bit = Boolean::new_witness(cs.clone(), || Ok(v.path_bits[i]))?;
            let left = CondSelectGadget::conditionally_select(&bit, &sib, &h)?;
            let right = CondSelectGadget::conditionally_select(&bit, &h, &sib)?;
            h = mimc2_var(&left, &right)?;
        }
        let two128 = Fr::from(2u64).pow([128]);
        h.enforce_equal(&(&root_lo + &root_hi * FpVar::constant(two128)))?;
        let amt_zero = amount.is_eq(&FpVar::constant(Fr::zero()))?;
        let peg_zero = (&pegout_hi + &pegout_lo).is_eq(&FpVar::constant(Fr::zero()))?;
        Boolean::kary_or(&[amt_zero, peg_zero.not()])?.enforce_equal(&Boolean::TRUE)?;

        let mut d = FpVar::constant(Fr::from(0x46504c32u64));
        for x in &vals {
            d = mimc2_var(&d, x)?;
        }
        let mut wc_le = d.to_bytes()?;
        wc_le.truncate(32);
        let wc_be: Vec<UInt8<Fr>> = wc_le.into_iter().rev().collect();

        let peg = UInt8::new_witness_vec(cs.clone(), &self.pegout_txid)?;
        let chk = UInt8::new_witness_vec(cs.clone(), &self.checkpoint)?;
        be_bytes_to_fp(&peg[..16])?.enforce_equal(&pegout_hi)?;
        be_bytes_to_fp(&peg[16..])?.enforce_equal(&pegout_lo)?;
        be_bytes_to_fp(&chk[..16])?.enforce_equal(&header_hi)?;
        be_bytes_to_fp(&chk[16..])?.enforce_equal(&header_lo)?;

        let mut inner = wc_be;
        inner.extend(chk);
        let binding = Sha256Gadget::digest(&inner)?.0;

        let mut msg: Vec<UInt8<Fr>> = V3_DOMAIN.iter().map(|b| UInt8::constant(*b)).collect();
        msg.extend(peg.into_iter().rev());
        msg.extend(binding);
        let digest = Sha256Gadget::digest(&msg)?.0;
        let mut bits = vec![];
        for (i, b) in digest.iter().enumerate().rev() {
            let mut bl = b.to_bits_le()?;
            if i == 0 {
                for k in 5..8 {
                    bl[k] = Boolean::FALSE;
                }
            }
            bits.extend(bl);
        }
        Boolean::le_bits_to_fp_var(&bits)?.enforce_equal(&x)?;
        Ok(())
    }
}
