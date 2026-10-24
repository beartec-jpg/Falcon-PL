//! Native Falcon-512 verify matching PQClean (pqcrypto-falcon 0.3).
//!
//! Witness extraction only: decompress pk/sig, SHAKE256 HashToPoint-CT,
//! recover s1 = c − s2·h in Z_q[x]/(x^n+1), check ||(s1,s2)||².

use crate::mimc::mimc2;
use ark_bn254::Fr;
use ark_ff::Zero;
use serde::{Deserialize, Serialize};
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::Shake256;

pub const LOGN: usize = 9;
pub const N: usize = 1 << LOGN;
pub const Q: u32 = 12289;
pub const NONCE_LEN: usize = 40;
pub const PK_LEN: usize = 897;
pub const SIG_MAX_LEN: usize = 666;
pub const L2BOUND: u64 = 34_034_726;
pub const H2P_OVER: usize = 205;
pub const H2P_SAMPLES: usize = N + H2P_OVER; // 717
pub const H2P_BYTES: usize = H2P_SAMPLES * 2; // 1434
pub const PK_HEADER: u8 = 0x00 + LOGN as u8; // 0x09
pub const SIG_HEADER: u8 = 0x30 + LOGN as u8; // 0x39
pub const REJECT: u16 = 0xFFFF;
pub const FIVE_Q: u32 = 5 * Q; // 61445
pub const PACK_COEFFS: usize = 16; // 16 × 14-bit < 254-bit Fr
pub const PACK_WORDS: usize = N / PACK_COEFFS; // 32
pub const MAX_LEAVES: usize = 8;
pub const STATEMENT_DOMAIN: &[u8] = b"fpl-falcon-qc-v1";
/// Outer-hash statement. 16 bytes. Claim is NOT a field — it is opened from `header`.
pub const STATEMENT_DOMAIN_V2: &[u8] = b"fpl-falcon-qc-v2";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FalconPublic {
    pub id: String,
    pub pk_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FalconSig {
    pub id: String,
    pub sig_hex: String,
}

#[derive(Clone)]
pub struct FalconVerifyWitness {
    pub h: [u16; N],
    pub s2: [i16; N],
    pub nonce: [u8; NONCE_LEN],
    pub c: [u16; N],
    pub s1: [i32; N],
}

pub fn statement_bytes(
    height: u64,
    parent: &[u8; 32],
    header: &[u8; 32],
    claim: &[u8; 32],
    network_id: u32,
) -> Vec<u8> {
    let mut m = Vec::with_capacity(STATEMENT_DOMAIN.len() + 8 + 32 + 32 + 32 + 4);
    m.extend_from_slice(STATEMENT_DOMAIN);
    m.extend_from_slice(&height.to_be_bytes());
    m.extend_from_slice(parent);
    m.extend_from_slice(header);
    m.extend_from_slice(claim);
    m.extend_from_slice(&network_id.to_be_bytes());
    m
}

/// v2 Groth16 message: domain || height_be8 || parent32 || header32 || nid_be4 (92 bytes).
/// `header` is the outer ledger_hash that already binds claimRoot + next-6.
pub fn statement_bytes_v2(
    height: u64,
    parent: &[u8; 32],
    header: &[u8; 32],
    network_id: u32,
) -> Vec<u8> {
    let mut m = Vec::with_capacity(STATEMENT_DOMAIN_V2.len() + 8 + 32 + 32 + 4);
    m.extend_from_slice(STATEMENT_DOMAIN_V2);
    m.extend_from_slice(&height.to_be_bytes());
    m.extend_from_slice(parent);
    m.extend_from_slice(header);
    m.extend_from_slice(&network_id.to_be_bytes());
    m
}

pub fn shake256(input: &[u8], out_len: usize) -> Vec<u8> {
    let mut h = Shake256::default();
    h.update(input);
    let mut r = h.finalize_xof();
    let mut out = vec![0u8; out_len];
    r.read(&mut out);
    out
}

/// PQClean `hash_to_point_ct` for logn=9. Returns (c[512], raw 717 samples).
pub fn hash_to_point_ct(nonce: &[u8], message: &[u8]) -> ([u16; N], [u16; H2P_SAMPLES]) {
    let mut input = Vec::with_capacity(nonce.len() + message.len());
    input.extend_from_slice(nonce);
    input.extend_from_slice(message);
    let stream = shake256(&input, H2P_BYTES);
    let mut samples = [0u16; H2P_SAMPLES];
    for u in 0..H2P_SAMPLES {
        let w = u32::from(stream[2 * u]) << 8 | u32::from(stream[2 * u + 1]);
        samples[u] = if w >= FIVE_Q {
            REJECT
        } else {
            (w % Q) as u16
        };
    }
    let first_valid = first_valid_512(&samples);
    (first_valid, samples)
}

fn first_valid_512(samples: &[u16; H2P_SAMPLES]) -> [u16; N] {
    let mut out = [0u16; N];
    let mut k = 0usize;
    for &sv in samples {
        if sv != REJECT {
            if k < N {
                out[k] = sv;
            }
            k += 1;
        }
    }
    assert!(k >= N, "HashToPoint undersampled (should be ~2^-256)");
    out
}

/// PQClean logarithmic compact. Kept to prove it equals first-valid.
#[cfg(test)]
fn compact_ct(mut samples: [u16; H2P_SAMPLES]) -> [u16; N] {
    let m = H2P_SAMPLES;
    let over = H2P_OVER;
    let mut p = 1usize;
    while p <= over {
        let mut v = 0usize;
        for u in 0..m {
            let sv = samples[u];
            let j = u - v;
            let valid = sv < 0x8000;
            if valid {
                v += 1;
            }
            if u >= p {
                let dv = samples[u - p];
                let swap = valid && (j & p) != 0;
                if swap {
                    samples[u] = dv;
                    samples[u - p] = sv;
                }
            }
        }
        p <<= 1;
    }
    let mut out = [0u16; N];
    out.copy_from_slice(&samples[..N]);
    out
}

pub fn modq_decode(buf: &[u8]) -> Result<[u16; N], String> {
    let in_len = (N * 14 + 7) / 8;
    if buf.len() < in_len {
        return Err("pk short".into());
    }
    let buf = &buf[..in_len];
    let mut x = [0u16; N];
    let mut acc: u32 = 0;
    let mut acc_len: i32 = 0;
    let mut u = 0usize;
    let mut i = 0usize;
    while u < N {
        acc = (acc << 8) | buf[i] as u32;
        i += 1;
        acc_len += 8;
        if acc_len >= 14 {
            acc_len -= 14;
            let w = (acc >> acc_len) & 0x3FFF;
            if w >= Q {
                return Err("pk coeff >= q".into());
            }
            x[u] = w as u16;
            u += 1;
        }
    }
    if acc & ((1u32 << acc_len) - 1) != 0 {
        return Err("pk extra bits".into());
    }
    Ok(x)
}

pub fn decode_pk(pk: &[u8]) -> Result<[u16; N], String> {
    if pk.len() != PK_LEN {
        return Err(format!("pk len {} want {PK_LEN}", pk.len()));
    }
    if pk[0] != PK_HEADER {
        return Err(format!("pk header 0x{:02x}", pk[0]));
    }
    modq_decode(&pk[1..])
}

pub fn comp_decode(buf: &[u8]) -> Result<[i16; N], String> {
    let mut x = [0i16; N];
    let mut v = 0usize;
    let mut acc: u32 = 0;
    let mut acc_len: u32 = 0;
    for u in 0..N {
        if v >= buf.len() {
            return Err("sig short".into());
        }
        acc = (acc << 8) | buf[v] as u32;
        v += 1;
        let b = acc >> acc_len;
        let s = b & 128;
        let mut m = b & 127;
        loop {
            if acc_len == 0 {
                if v >= buf.len() {
                    return Err("sig short".into());
                }
                acc = (acc << 8) | buf[v] as u32;
                v += 1;
                acc_len = 8;
            }
            acc_len -= 1;
            if ((acc >> acc_len) & 1) != 0 {
                break;
            }
            m += 128;
            if m > 2047 {
                return Err("sig coeff".into());
            }
        }
        if s != 0 && m == 0 {
            return Err("sig minus-zero".into());
        }
        x[u] = if s != 0 { -(m as i16) } else { m as i16 };
    }
    if acc & ((1u32 << acc_len) - 1) != 0 {
        return Err("sig extra bits".into());
    }
    if v != buf.len() {
        return Err(format!("sig trailing {} of {}", v, buf.len()));
    }
    Ok(x)
}

pub fn decode_sig(sig: &[u8]) -> Result<([u8; NONCE_LEN], [i16; N]), String> {
    if sig.len() < 1 + NONCE_LEN + 1 {
        return Err("sig short".into());
    }
    if sig[0] != SIG_HEADER {
        return Err(format!("sig header 0x{:02x}", sig[0]));
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&sig[1..1 + NONCE_LEN]);
    let s2 = comp_decode(&sig[1 + NONCE_LEN..])?;
    Ok((nonce, s2))
}

pub fn to_q_nonneg(z: i16) -> u16 {
    if z < 0 {
        (z as i32 + Q as i32) as u16
    } else {
        z as u16
    }
}

pub fn poly_mul_negacyclic(a: &[u16; N], b: &[u16; N]) -> [u16; N] {
    let mut acc = [0i64; N];
    for i in 0..N {
        let ai = a[i] as i64;
        for j in 0..N {
            let p = ai * b[j] as i64;
            let k = i + j;
            if k < N {
                acc[k] += p;
            } else {
                acc[k - N] -= p;
            }
        }
    }
    let q = Q as i64;
    let mut out = [0u16; N];
    for i in 0..N {
        let mut r = acc[i] % q;
        if r < 0 {
            r += q;
        }
        out[i] = r as u16;
    }
    out
}

pub fn poly_sub(a: &[u16; N], b: &[u16; N]) -> [u16; N] {
    let mut out = [0u16; N];
    let q = Q as i32;
    for i in 0..N {
        let mut r = a[i] as i32 - b[i] as i32;
        if r < 0 {
            r += q;
        }
        out[i] = r as u16;
    }
    out
}

pub fn center(w: u16) -> i32 {
    let z = w as i32;
    if z > (Q as i32) / 2 {
        z - Q as i32
    } else {
        z
    }
}

pub fn is_short(s1: &[i32; N], s2: &[i16; N]) -> bool {
    let mut s: u64 = 0;
    for i in 0..N {
        let z1 = s1[i] as i64;
        let z2 = s2[i] as i64;
        s = s.saturating_add((z1 * z1) as u64);
        s = s.saturating_add((z2 * z2) as u64);
        if s > L2BOUND {
            return false;
        }
    }
    true
}

pub fn recover_s1(c: &[u16; N], s2: &[i16; N], h: &[u16; N]) -> [i32; N] {
    let mut s2q = [0u16; N];
    for i in 0..N {
        s2q[i] = to_q_nonneg(s2[i]);
    }
    let s2h = poly_mul_negacyclic(&s2q, h);
    let s1q = poly_sub(c, &s2h);
    let mut s1 = [0i32; N];
    for i in 0..N {
        s1[i] = center(s1q[i]);
    }
    s1
}

pub fn extract_witness(pk: &[u8], sig: &[u8], message: &[u8]) -> Result<FalconVerifyWitness, String> {
    let h = decode_pk(pk)?;
    let (nonce, s2) = decode_sig(sig)?;
    let (c, _) = hash_to_point_ct(&nonce, message);
    let s1 = recover_s1(&c, &s2, &h);
    if !is_short(&s1, &s2) {
        return Err("norm".into());
    }
    Ok(FalconVerifyWitness {
        h,
        s2,
        nonce,
        c,
        s1,
    })
}

pub fn verify_detached(pk: &[u8], sig: &[u8], message: &[u8]) -> bool {
    extract_witness(pk, sig, message).is_ok()
}

pub fn pack_h(h: &[u16; N]) -> [Fr; PACK_WORDS] {
    let base = Fr::from(1u64 << 14);
    let mut out = [Fr::zero(); PACK_WORDS];
    for i in 0..PACK_WORDS {
        let mut acc = Fr::zero();
        for j in 0..PACK_COEFFS {
            acc = acc * base + Fr::from(h[i * PACK_COEFFS + j] as u64);
        }
        out[i] = acc;
    }
    out
}

pub fn pk_leaf(h: &[u16; N]) -> Fr {
    let packed = pack_h(h);
    let mut acc = Fr::zero();
    for p in packed {
        acc = mimc2(acc, p);
    }
    acc
}

pub fn dummy_leaf() -> Fr {
    pk_leaf(&[0u16; N])
}

pub fn committee_key_root(pks: &[[u16; N]]) -> Result<Fr, String> {
    if pks.len() > MAX_LEAVES {
        return Err("too many Falcon pks".into());
    }
    let mut leaves = [dummy_leaf(); MAX_LEAVES];
    for (i, h) in pks.iter().enumerate() {
        leaves[i] = pk_leaf(h);
    }
    Ok(crate::mimc::merkle8(&leaves))
}

pub fn pad_pks(pks: Vec<[u16; N]>) -> Vec<[u16; N]> {
    let mut v = pks;
    while v.len() < MAX_LEAVES {
        v.push([0u16; N]);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use pqcrypto_falcon::falcon512;
    use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _};

    fn sign_once(msg: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (pk, sk) = falcon512::keypair();
        let sig = falcon512::detached_sign(msg, &sk);
        (pk.as_bytes().to_vec(), sig.as_bytes().to_vec())
    }

    #[test]
    fn native_verify_matches_pqcrypto() {
        let msg = b"fpl-falcon-qc-v1 test vector";
        let (pk, sig) = sign_once(msg);
        assert!(falcon512::verify_detached_signature(
            &falcon512::DetachedSignature::from_bytes(&sig).unwrap(),
            msg,
            &falcon512::PublicKey::from_bytes(&pk).unwrap()
        )
        .is_ok());
        assert!(verify_detached(&pk, &sig, msg), "algebraic verify must match PQClean");
        assert!(!verify_detached(&pk, &sig, b"tampered"));
    }

    #[test]
    fn hash_to_point_ct_equals_first_valid() {
        let nonce = [7u8; NONCE_LEN];
        let msg = b"abc";
        let (c, samples) = hash_to_point_ct(&nonce, msg);
        assert_eq!(c, compact_ct(samples));
        assert_eq!(c, first_valid_512(&samples));
        assert!(c.iter().all(|&x| (x as u32) < Q));
    }

    #[test]
    fn pk_leaf_changes_with_h() {
        let mut h = [0u16; N];
        let a = pk_leaf(&h);
        h[0] = 1;
        assert_ne!(a, pk_leaf(&h));
    }

    #[test]
    fn statement_len_fixed() {
        let m = statement_bytes(1, &[2u8; 32], &[3u8; 32], &[4u8; 32], 2300);
        assert_eq!(m.len(), STATEMENT_DOMAIN.len() + 8 + 32 + 32 + 32 + 4);
        assert_eq!(m.len(), 124);
        assert!(m.starts_with(STATEMENT_DOMAIN));
    }

    #[test]
    fn statement_v2_len_fixed_no_claim() {
        let m = statement_bytes_v2(1, &[2u8; 32], &[3u8; 32], 2300);
        assert_eq!(m.len(), STATEMENT_DOMAIN_V2.len() + 8 + 32 + 32 + 4);
        assert_eq!(m.len(), 92);
        assert!(m.starts_with(STATEMENT_DOMAIN_V2));
        assert_ne!(STATEMENT_DOMAIN, STATEMENT_DOMAIN_V2);
        let v1 = statement_bytes(1, &[2u8; 32], &[3u8; 32], &[4u8; 32], 2300);
        assert_eq!(v1.len(), 124);
        assert_ne!(m, v1[..92]);
    }

    #[test]
    fn extract_witness_norm_ok() {
        let msg = statement_bytes(9, &[1u8; 32], &[2u8; 32], &[3u8; 32], 2300);
        let (pk, sig) = sign_once(&msg);
        let w = extract_witness(&pk, &sig, &msg).expect("witness");
        assert_eq!(w.h.len(), N);
        assert!(w.c.iter().all(|&x| (x as u32) < Q));
        assert!(is_short(&w.s1, &w.s2));
    }
}
