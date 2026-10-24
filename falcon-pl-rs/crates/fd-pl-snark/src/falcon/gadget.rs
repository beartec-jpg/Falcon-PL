//! R1CS gadgets: range, HashToPoint-CT, negacyclic mul, L2 bound, pk leaf.

use super::native::{
    H2P_BYTES, H2P_OVER, H2P_SAMPLES, L2BOUND, N, NONCE_LEN, PACK_COEFFS, PACK_WORDS, Q, REJECT,
};
use super::shake::shake256_var;
use crate::mimc::mimc2_var;
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField, Zero};
use ark_r1cs_std::alloc::AllocVar;
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::prelude::*;
use ark_r1cs_std::select::CondSelectGadget;
use ark_r1cs_std::R1CSVar;
use ark_relations::r1cs::{ConstraintSystemRef, SynthesisError};

/// Witness-n bits and prove `x = Σ b_i 2^i` (no 254-bit full-field decomposition).
pub fn enforce_num_bits(x: &FpVar<Fr>, n: usize) -> Result<Vec<Boolean<Fr>>, SynthesisError> {
    if x.is_constant() {
        let v = x.value().unwrap_or(Fr::zero());
        let bytes = v.into_bigint().to_bytes_le();
        let mut bits = Vec::with_capacity(n);
        for i in 0..n {
            let b = (bytes.get(i / 8).copied().unwrap_or(0) >> (i % 8)) & 1 == 1;
            bits.push(Boolean::constant(b));
        }
        return Ok(bits);
    }
    let cs = x.cs();
    let val = x.value().ok();
    let mut bits = Vec::with_capacity(n);
    for i in 0..n {
        let bit = Boolean::new_witness(cs.clone(), || {
            let v = val.ok_or(SynthesisError::AssignmentMissing)?;
            let bytes = v.into_bigint().to_bytes_le();
            Ok((bytes.get(i / 8).copied().unwrap_or(0) >> (i % 8)) & 1 == 1)
        })?;
        bits.push(bit);
    }
    let recon = Boolean::le_bits_to_fp_var(&bits)?;
    recon.enforce_equal(x)?;
    Ok(bits)
}

/// x ∈ [0, q). q < 2^14.
pub fn enforce_mod_q(x: &FpVar<Fr>) -> Result<(), SynthesisError> {
    let _ = enforce_num_bits(x, 14)?;
    let d = FpVar::constant(Fr::from((Q - 1) as u64)) - x;
    let _ = enforce_num_bits(&d, 14)?;
    Ok(())
}

/// Big-endian `nbytes` of a field element that fits in `nbytes*8` bits.
pub fn fp_to_be_bytes(x: &FpVar<Fr>, nbytes: usize) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    let bits = enforce_num_bits(x, nbytes * 8)?;
    let mut out = Vec::with_capacity(nbytes);
    for i in 0..nbytes {
        let lo = (nbytes - 1 - i) * 8;
        out.push(Boolean::le_bits_to_fp_var(&bits[lo..lo + 8])?);
    }
    Ok(out)
}

pub fn statement_bytes_var(
    height: &FpVar<Fr>,
    parent_hi: &FpVar<Fr>,
    parent_lo: &FpVar<Fr>,
    header_hi: &FpVar<Fr>,
    header_lo: &FpVar<Fr>,
    claim_hi: &FpVar<Fr>,
    claim_lo: &FpVar<Fr>,
    network_id: &FpVar<Fr>,
) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    let mut out = Vec::new();
    for b in super::native::STATEMENT_DOMAIN {
        out.push(FpVar::constant(Fr::from(*b as u64)));
    }
    out.extend(fp_to_be_bytes(height, 8)?);
    out.extend(fp_to_be_bytes(parent_hi, 16)?);
    out.extend(fp_to_be_bytes(parent_lo, 16)?);
    out.extend(fp_to_be_bytes(header_hi, 16)?);
    out.extend(fp_to_be_bytes(header_lo, 16)?);
    out.extend(fp_to_be_bytes(claim_hi, 16)?);
    out.extend(fp_to_be_bytes(claim_lo, 16)?);
    out.extend(fp_to_be_bytes(network_id, 4)?);
    Ok(out)
}

/// v2 statement: domain || height_be8 || parent32 || header32 || nid_be4 (no claim).
pub fn statement_bytes_v2_var(
    height: &FpVar<Fr>,
    parent_hi: &FpVar<Fr>,
    parent_lo: &FpVar<Fr>,
    header_hi: &FpVar<Fr>,
    header_lo: &FpVar<Fr>,
    network_id: &FpVar<Fr>,
) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    let mut out = Vec::new();
    for b in super::native::STATEMENT_DOMAIN_V2 {
        out.push(FpVar::constant(Fr::from(*b as u64)));
    }
    out.extend(fp_to_be_bytes(height, 8)?);
    out.extend(fp_to_be_bytes(parent_hi, 16)?);
    out.extend(fp_to_be_bytes(parent_lo, 16)?);
    out.extend(fp_to_be_bytes(header_hi, 16)?);
    out.extend(fp_to_be_bytes(header_lo, 16)?);
    out.extend(fp_to_be_bytes(network_id, 4)?);
    Ok(out)
}

/// w = k*q + r, r∈[0,q), k∈[0,5]; valid iff k<5 (w < 5q).
fn reduce_sample(
    cs: ConstraintSystemRef<Fr>,
    w: &FpVar<Fr>,
) -> Result<(FpVar<Fr>, Boolean<Fr>), SynthesisError> {
    let _ = enforce_num_bits(w, 16)?;
    let w_val = w.value().unwrap_or(Fr::zero());
    let w_u = bigint_u64(&w_val);
    let k_n = w_u / Q as u64;
    let r_n = w_u % Q as u64;
    let k = FpVar::new_witness(cs.clone(), || Ok(Fr::from(k_n)))?;
    let r = FpVar::new_witness(cs, || Ok(Fr::from(r_n)))?;
    let _ = enforce_num_bits(&k, 3)?;
    enforce_mod_q(&r)?;
    let recon = &k * FpVar::constant(Fr::from(Q as u64)) + &r;
    recon.enforce_equal(w)?;
    let mut k_ok = Boolean::FALSE;
    for i in 0..=5 {
        k_ok = k_ok.or(&k.is_eq(&FpVar::constant(Fr::from(i as u64)))?)?;
    }
    k_ok.enforce_equal(&Boolean::TRUE)?;
    let is_five = k.is_eq(&FpVar::constant(Fr::from(5u64)))?;
    let valid = is_five.not();
    Ok((r, valid))
}

fn bigint_u64(f: &Fr) -> u64 {
    let b = f.into_bigint().to_bytes_le();
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_le_bytes(a)
}

pub fn hash_to_point_var(
    cs: ConstraintSystemRef<Fr>,
    nonce: &[FpVar<Fr>],
    message: &[FpVar<Fr>],
) -> Result<[FpVar<Fr>; N], SynthesisError> {
    assert_eq!(nonce.len(), NONCE_LEN);
    let mut input = Vec::with_capacity(NONCE_LEN + message.len());
    input.extend_from_slice(nonce);
    input.extend_from_slice(message);
    let stream = shake256_var(&input, H2P_BYTES)?;
    let mut samples = Vec::with_capacity(H2P_SAMPLES);
    let mut valids = Vec::with_capacity(H2P_SAMPLES);
    for u in 0..H2P_SAMPLES {
        let hi = &stream[2 * u];
        let lo = &stream[2 * u + 1];
        let w = hi * FpVar::constant(Fr::from(256u64)) + lo;
        let (r, v) = reduce_sample(cs.clone(), &w)?;
        samples.push(r);
        valids.push(v);
    }
    compact_first_n(&mut samples, &mut valids)?;
    for ok in valids.iter().take(N) {
        ok.enforce_equal(&Boolean::TRUE)?;
    }
    let _ = REJECT;
    samples
        .into_iter()
        .take(N)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| SynthesisError::Unsatisfiable)
}

/// PQClean HashToPoint-CT compact: in-place power-of-two hole fills.
fn compact_first_n(
    samples: &mut [FpVar<Fr>],
    valids: &mut [Boolean<Fr>],
) -> Result<(), SynthesisError> {
    let m = H2P_SAMPLES;
    let mut p = 1usize;
    while p <= H2P_OVER {
        let mut v = FpVar::<Fr>::zero();
        let p_bit = p.trailing_zeros() as usize;
        for u in 0..m {
            let sv = samples[u].clone();
            let sv_ok = valids[u].clone();
            let j = FpVar::constant(Fr::from(u as u64)) - &v;
            let j_bits = enforce_num_bits(&j, 10)?;
            if u >= p {
                let do_swap = sv_ok.and(&j_bits[p_bit])?;
                let dv = samples[u - p].clone();
                let dv_ok = valids[u - p].clone();
                samples[u] = FpVar::conditionally_select(&do_swap, &dv, &sv)?;
                samples[u - p] = FpVar::conditionally_select(&do_swap, &sv, &dv)?;
                valids[u] = Boolean::conditionally_select(&do_swap, &dv_ok, &sv_ok)?;
                valids[u - p] = Boolean::conditionally_select(&do_swap, &sv_ok, &dv_ok)?;
            }
            v += FpVar::from(sv_ok);
        }
        p <<= 1;
    }
    Ok(())
}

fn reduce_shifted(
    cs: ConstraintSystemRef<Fr>,
    shifted: &FpVar<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let q = FpVar::constant(Fr::from(Q as u64));
    let s_val = shifted.value().unwrap_or(Fr::zero());
    let s_u = {
        let b = s_val.into_bigint().to_bytes_le();
        // shifted < 2^40; read 8 bytes
        let mut a = [0u8; 8];
        a.copy_from_slice(&b[..8]);
        u64::from_le_bytes(a)
    };
    let quot_n = s_u / Q as u64;
    let rem_n = s_u % Q as u64;
    let quot = FpVar::new_witness(cs.clone(), || Ok(Fr::from(quot_n)))?;
    let rem = FpVar::new_witness(cs, || Ok(Fr::from(rem_n)))?;
    let _ = enforce_num_bits(&quot, 24)?;
    enforce_mod_q(&rem)?;
    (quot * q + &rem).enforce_equal(shifted)?;
    Ok(rem)
}

pub fn poly_mul_negacyclic_var(
    cs: ConstraintSystemRef<Fr>,
    a: &[FpVar<Fr>; N],
    b: &[FpVar<Fr>; N],
) -> Result<[FpVar<Fr>; N], SynthesisError> {
    let bias = FpVar::constant(Fr::from(512u64) * Fr::from(Q as u64) * Fr::from(Q as u64));
    let mut out = Vec::with_capacity(N);
    for i in 0..N {
        let mut pos = FpVar::<Fr>::zero();
        let mut neg = FpVar::<Fr>::zero();
        for j in 0..N {
            let prod = &a[j] * &b[(i + N - j) % N];
            if j <= i {
                pos += prod;
            } else {
                neg += prod;
            }
        }
        let shifted = pos + &bias - neg;
        out.push(reduce_shifted(cs.clone(), &shifted)?);
    }
    out.try_into().map_err(|_| SynthesisError::Unsatisfiable)
}

pub fn poly_sub_mod_q(
    cs: ConstraintSystemRef<Fr>,
    a: &[FpVar<Fr>; N],
    b: &[FpVar<Fr>; N],
) -> Result<[FpVar<Fr>; N], SynthesisError> {
    let q = FpVar::constant(Fr::from(Q as u64));
    let mut out = Vec::with_capacity(N);
    for i in 0..N {
        let shifted = &a[i] + &q - &b[i];
        out.push(reduce_shifted(cs.clone(), &shifted)?);
    }
    out.try_into().map_err(|_| SynthesisError::Unsatisfiable)
}

pub fn to_q_nonneg_var(z: &FpVar<Fr>, negative: &Boolean<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    // z is |coeff|; if negative, q − z (z>0). z=0 and negative is forbidden by codec.
    let qz = FpVar::constant(Fr::from(Q as u64)) - z;
    FpVar::conditionally_select(negative, &qz, z)
}

#[allow(dead_code)]
pub fn center_var(w: &FpVar<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    // if w > q/2 (=6144) then w−q else w. Represent centered as signed via q-shift.
    // We return the centered value as Fr element in [0,q) still, plus a signed square
    // computed separately. See sq_centered.
    Ok(w.clone())
}

/// Square of the centered representative of w ∈ [0,q).
pub fn sq_centered(cs: ConstraintSystemRef<Fr>, w: &FpVar<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    let half = 6144u64;
    let bits = enforce_num_bits(w, 14)?;
    // w > 6144 ⇔ centered is negative. Compare via 14-bit range.
    let w_val = w.value().unwrap_or(Fr::zero());
    let w_u = bigint_u64(&w_val);
    let flag = w_u > half;
    let hi = Boolean::new_witness(cs.clone(), || Ok(flag))?;
    // hi ⇒ w >= 6145, !hi ⇒ w <= 6144.
    let bound_lo = FpVar::constant(Fr::from(half + 1));
    let bound_hi = FpVar::constant(Fr::from(half));
    // If hi: w - (half+1) is 14-bit; if !hi: half - w is 14-bit.
    let a = w - &bound_lo;
    let b = &bound_hi - w;
    let chk_hi = FpVar::conditionally_select(&hi, &a, &FpVar::zero())?;
    let chk_lo = FpVar::conditionally_select(&hi, &FpVar::zero(), &b)?;
    let _ = enforce_num_bits(&chk_hi, 14)?;
    let _ = enforce_num_bits(&chk_lo, 14)?;
    let centered = FpVar::conditionally_select(
        &hi,
        &(w.clone() - FpVar::constant(Fr::from(Q as u64))),
        w,
    )?;
    // centered may be Fr(p - x). Square is still x² because (p-x)² ≠ x² in Fr!
    // Compute square from the positive magnitude instead.
    let mag = FpVar::conditionally_select(&hi, &(FpVar::constant(Fr::from(Q as u64)) - w), w)?;
    let _ = bits;
    let _ = centered;
    Ok(&mag * &mag)
}

/// s2 is signed i16. Witness mag (≥0) and sign bit.
pub fn s2_as_q_and_sq(
    cs: ConstraintSystemRef<Fr>,
    s2: i16,
) -> Result<(FpVar<Fr>, FpVar<Fr>, FpVar<Fr>), SynthesisError> {
    let neg = s2 < 0;
    let mag = s2.unsigned_abs() as u64;
    let mag_v = FpVar::new_witness(cs.clone(), || Ok(Fr::from(mag)))?;
    let neg_v = Boolean::new_witness(cs.clone(), || Ok(neg))?;
    // |s2| <= 6598 theoretically; 13 bits. Enforce 13 bits and mag != 0 if neg.
    let _ = enforce_num_bits(&mag_v, 13)?;
    neg_v
        .and(&mag_v.is_eq(&FpVar::zero())?)?
        .enforce_equal(&Boolean::FALSE)?;
    let s2q = to_q_nonneg_var(&mag_v, &neg_v)?;
    let sq = &mag_v * &mag_v;
    Ok((s2q, sq, mag_v))
}

pub fn pack_h_var(h: &[FpVar<Fr>; N]) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    let base = FpVar::constant(Fr::from(1u64 << 14));
    let mut out = Vec::with_capacity(PACK_WORDS);
    for i in 0..PACK_WORDS {
        let mut acc = FpVar::<Fr>::zero();
        for j in 0..PACK_COEFFS {
            acc = acc * &base + &h[i * PACK_COEFFS + j];
        }
        out.push(acc);
    }
    Ok(out)
}

pub fn pk_leaf_var(h: &[FpVar<Fr>; N]) -> Result<FpVar<Fr>, SynthesisError> {
    let packed = pack_h_var(h)?;
    let mut acc = FpVar::<Fr>::zero();
    for p in packed {
        acc = mimc2_var(&acc, &p)?;
    }
    Ok(acc)
}

pub fn falcon_verify_var(
    cs: ConstraintSystemRef<Fr>,
    h: &[FpVar<Fr>; N],
    s2q: &[FpVar<Fr>; N],
    s2sq: &[FpVar<Fr>; N],
    nonce: &[FpVar<Fr>],
    message: &[FpVar<Fr>],
) -> Result<(), SynthesisError> {
    for coeff in h.iter().chain(s2q.iter()) {
        enforce_mod_q(coeff)?;
    }
    let c = hash_to_point_var(cs.clone(), nonce, message)?;
    let s2h = poly_mul_negacyclic_var(cs.clone(), s2q, h)?;
    let s1q = poly_sub_mod_q(cs.clone(), &c, &s2h)?;
    let mut norm = FpVar::<Fr>::zero();
    for i in 0..N {
        norm += sq_centered(cs.clone(), &s1q[i])?;
        norm += &s2sq[i];
    }
    // norm <= L2BOUND. Both fit in 36 bits.
    let _ = enforce_num_bits(&norm, 36)?;
    let diff = FpVar::constant(Fr::from(L2BOUND)) - &norm;
    let _ = enforce_num_bits(&diff, 36)?;
    Ok(())
}

pub fn alloc_h(
    cs: ConstraintSystemRef<Fr>,
    h: &[u16; N],
) -> Result<[FpVar<Fr>; N], SynthesisError> {
    let mut v = Vec::with_capacity(N);
    for c in h {
        v.push(FpVar::new_witness(cs.clone(), || Ok(Fr::from(*c as u64)))?);
    }
    v.try_into().map_err(|_| SynthesisError::Unsatisfiable)
}

pub fn alloc_bytes(
    cs: ConstraintSystemRef<Fr>,
    b: &[u8],
) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    b.iter()
        .map(|x| FpVar::new_witness(cs.clone(), || Ok(Fr::from(*x as u64))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::falcon::native::{hash_to_point_ct, statement_bytes};
    use ark_relations::r1cs::ConstraintSystem;

    #[test]
    fn fp_to_be_bytes_roundtrip() {
        let cs = ConstraintSystem::<Fr>::new_ref();
        let x = FpVar::new_input(cs.clone(), || Ok(Fr::from(0x0102_u64))).unwrap();
        let b = fp_to_be_bytes(&x, 2).unwrap();
        assert!(cs.is_satisfied().unwrap());
        assert_eq!(bigint_u64(&b[0].value().unwrap()), 1);
        assert_eq!(bigint_u64(&b[1].value().unwrap()), 2);
    }

    #[test]
    fn hash_to_point_gadget_matches_native() {
        let nonce = [9u8; NONCE_LEN];
        let msg = statement_bytes(1, &[2u8; 32], &[3u8; 32], &[4u8; 32], 2300);
        let (c_native, _) = hash_to_point_ct(&nonce, &msg);
        let cs = ConstraintSystem::<Fr>::new_ref();
        let nonce_v = alloc_bytes(cs.clone(), &nonce).unwrap();
        let msg_v = alloc_bytes(cs.clone(), &msg).unwrap();
        for b in nonce_v.iter().chain(msg_v.iter()) {
            enforce_num_bits(b, 8).unwrap();
        }
        let c = hash_to_point_var(cs.clone(), &nonce_v, &msg_v).unwrap();
        assert!(cs.is_satisfied().unwrap(), "HashToPoint r1cs");
        for i in 0..N {
            assert_eq!(
                bigint_u64(&c[i].value().unwrap()),
                c_native[i] as u64,
                "c[{i}]"
            );
        }
        eprintln!(
            "hash_to_point constraints={} witnesses={}",
            cs.num_constraints(),
            cs.num_witness_variables()
        );
    }
}
