//! BabyJubJub Schnorr over the LC statement (native, off-circuit).

use crate::codec::{fr_from_be_hex, fr_to_be_hex, public_inputs};
use crate::mimc::{mask128, mimc2};
use ark_bn254::Fr;
use ark_ec::{AffineRepr, CurveGroup, Group};
use ark_ed_on_bn254::{EdwardsAffine, EdwardsProjective, Fq, Fr as BabyFr};
use ark_ff::{BigInteger, PrimeField, UniformRand, Zero};
use rand::RngCore;
use serde::{Deserialize, Serialize};

pub const MAX_LEAVES: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LcSecret {
    pub id: String,
    pub sk: String,
    pub pk_x: String,
    pub pk_y: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LcPublic {
    pub id: String,
    pub pk_x: String,
    pub pk_y: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LcSig {
    pub id: String,
    pub pk_x: String,
    pub pk_y: String,
    pub r_x: String,
    pub r_y: String,
    pub s: String,
}

pub fn fq_to_hex(f: Fq) -> String {
    fr_to_be_hex(f)
}

pub fn fq_from_hex(s: &str) -> Result<Fq, String> {
    fr_from_be_hex(s)
}

pub fn baby_to_hex(f: BabyFr) -> String {
    let mut le = f.into_bigint().to_bytes_le();
    le.resize(32, 0);
    le.reverse();
    hex::encode(le)
}

pub fn baby_from_hex(s: &str) -> Result<BabyFr, String> {
    let h = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let raw = hex::decode(h).map_err(|e| e.to_string())?;
    if raw.len() > 32 {
        return Err("scalar hex too long".into());
    }
    let mut be = [0u8; 32];
    be[32 - raw.len()..].copy_from_slice(&raw);
    Ok(BabyFr::from_be_bytes_mod_order(&be))
}

fn try_point(x: Fq, y: Fq) -> Result<EdwardsAffine, String> {
    let p = EdwardsAffine::new_unchecked(x, y);
    if !p.is_on_curve() {
        return Err("not on curve".into());
    }
    Ok(p)
}

pub fn pk_from_secret(sk: BabyFr) -> EdwardsAffine {
    (EdwardsProjective::generator() * sk).into_affine()
}

pub fn generate(id: &str, rng: &mut impl RngCore) -> LcSecret {
    loop {
        let sk = BabyFr::rand(rng);
        if sk.is_zero() {
            continue;
        }
        let pk = pk_from_secret(sk);
        if pk.is_zero() {
            continue;
        }
        return LcSecret {
            id: id.to_string(),
            sk: baby_to_hex(sk),
            pk_x: fq_to_hex(pk.x),
            pk_y: fq_to_hex(pk.y),
        };
    }
}

impl LcSecret {
    pub fn public(&self) -> LcPublic {
        LcPublic {
            id: self.id.clone(),
            pk_x: self.pk_x.clone(),
            pk_y: self.pk_y.clone(),
        }
    }

    pub fn sk_fr(&self) -> Result<BabyFr, String> {
        baby_from_hex(&self.sk)
    }

    pub fn pk(&self) -> Result<EdwardsAffine, String> {
        try_point(fq_from_hex(&self.pk_x)?, fq_from_hex(&self.pk_y)?)
    }
}

impl LcPublic {
    pub fn pk(&self) -> Result<EdwardsAffine, String> {
        try_point(fq_from_hex(&self.pk_x)?, fq_from_hex(&self.pk_y)?)
    }

    pub fn identity_pad(id: &str) -> Self {
        let z = EdwardsAffine::zero();
        Self {
            id: id.to_string(),
            pk_x: fq_to_hex(z.x),
            pk_y: fq_to_hex(z.y),
        }
    }
}

pub fn statement_digest(
    height: u64,
    parent: &[u8; 32],
    header: &[u8; 32],
    claim_root: &[u8; 32],
    network_id: u32,
) -> Fr {
    let pubs = public_inputs(height, parent, header, claim_root, Fr::zero(), 0, network_id);
    // height, parent_hi/lo, header_hi/lo, claim_hi/lo, (skip key_root/threshold), network
    let mut s = pubs[0];
    for x in &pubs[1..7] {
        s = mimc2(s, *x);
    }
    mimc2(s, pubs[9])
}

pub fn schnorr_challenge(r: &EdwardsAffine, p: &EdwardsAffine, stmt: Fr) -> Fr {
    mask128(mimc2(mimc2(r.x, p.x), stmt))
}

fn challenge_to_baby(e: Fr) -> BabyFr {
    let mut le = e.into_bigint().to_bytes_le();
    le.resize(32, 0);
    BabyFr::from_le_bytes_mod_order(&le)
}

pub fn sign(sk: BabyFr, stmt: Fr, rng: &mut impl RngCore) -> Result<(EdwardsAffine, BabyFr), String> {
    let p = pk_from_secret(sk);
    if p.is_zero() {
        return Err("zero pk".into());
    }
    loop {
        let k = BabyFr::rand(rng);
        if k.is_zero() {
            continue;
        }
        let r = (EdwardsProjective::generator() * k).into_affine();
        if r.is_zero() {
            continue;
        }
        let e = challenge_to_baby(schnorr_challenge(&r, &p, stmt));
        let s = k + e * sk;
        return Ok((r, s));
    }
}

pub fn verify_sig(p: &EdwardsAffine, r: &EdwardsAffine, s: BabyFr, stmt: Fr) -> bool {
    if p.is_zero() || r.is_zero() || s.is_zero() {
        return false;
    }
    if !p.is_on_curve() || !r.is_on_curve() {
        return false;
    }
    let e = challenge_to_baby(schnorr_challenge(r, p, stmt));
    let sg = EdwardsProjective::generator() * s;
    let rhs = r.into_group() + p.into_group() * e;
    sg.into_affine() == rhs.into_affine()
}

impl LcSecret {
    pub fn sign_stmt(&self, stmt: Fr, rng: &mut impl RngCore) -> Result<LcSig, String> {
        let sk = self.sk_fr()?;
        let (r, s) = sign(sk, stmt, rng)?;
        Ok(LcSig {
            id: self.id.clone(),
            pk_x: self.pk_x.clone(),
            pk_y: self.pk_y.clone(),
            r_x: fq_to_hex(r.x),
            r_y: fq_to_hex(r.y),
            s: baby_to_hex(s),
        })
    }
}

impl LcSig {
    pub fn r(&self) -> Result<EdwardsAffine, String> {
        try_point(fq_from_hex(&self.r_x)?, fq_from_hex(&self.r_y)?)
    }
    pub fn p(&self) -> Result<EdwardsAffine, String> {
        try_point(fq_from_hex(&self.pk_x)?, fq_from_hex(&self.pk_y)?)
    }
    pub fn s_fr(&self) -> Result<BabyFr, String> {
        baby_from_hex(&self.s)
    }
    pub fn verify(&self, stmt: Fr) -> bool {
        let Ok(p) = self.p() else {
            return false;
        };
        let Ok(r) = self.r() else {
            return false;
        };
        let Ok(s) = self.s_fr() else {
            return false;
        };
        verify_sig(&p, &r, s, stmt)
    }
}

pub fn pad_committee(mut pks: Vec<LcPublic>) -> Vec<LcPublic> {
    pks.retain(|p| !p.id.starts_with("pad"));
    pks.sort_by(|a, b| a.id.cmp(&b.id));
    while pks.len() < MAX_LEAVES {
        pks.push(LcPublic::identity_pad(&format!("pad{}", pks.len())));
    }
    pks.truncate(MAX_LEAVES);
    pks
}

pub fn committee_key_root(pks: &[LcPublic]) -> Result<Fr, String> {
    let padded = if pks.len() == MAX_LEAVES {
        pks.to_vec()
    } else {
        pad_committee(pks.to_vec())
    };
    let mut leaves = [Fr::zero(); MAX_LEAVES];
    for (i, p) in padded.iter().enumerate() {
        let pk = p.pk()?;
        leaves[i] = mimc2(pk.x, pk.y);
    }
    Ok(crate::mimc::merkle8(&leaves))
}

pub fn split_header_fields(
    parent_hex: &str,
    header_hex: &str,
    claim_hex: &str,
) -> Result<([u8; 32], [u8; 32], [u8; 32]), String> {
    use crate::codec::hex_to_bytes32;
    Ok((
        hex_to_bytes32(parent_hex)?,
        hex_to_bytes32(header_hex)?,
        hex_to_bytes32(claim_hex)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    #[test]
    fn pad_committee_keeps_validators_before_identity() {
        let mut rng = ChaCha20Rng::from_seed([1u8; 32]);
        let secs: Vec<_> = (1..=6).map(|i| generate(&format!("v{i}"), &mut rng)).collect();
        let pubs: Vec<_> = secs.iter().map(|s| s.public()).collect();
        let a = pad_committee(pubs.clone());
        let b = pad_committee(a.clone());
        assert_eq!(a.iter().map(|p| p.id.clone()).collect::<Vec<_>>(), b.iter().map(|p| p.id.clone()).collect::<Vec<_>>());
        assert_eq!(a[0].id, "v1");
        assert_eq!(a[5].id, "v6");
        assert!(a[6].id.starts_with("pad"));
        assert_eq!(committee_key_root(&pubs).unwrap(), committee_key_root(&a).unwrap());
    }

    #[test]
    fn schnorr_roundtrip() {
        let mut rng = ChaCha20Rng::from_seed([7u8; 32]);
        let sec = generate("v1", &mut rng);
        let parent = [1u8; 32];
        let header = [2u8; 32];
        let claim = [3u8; 32];
        let stmt = statement_digest(9, &parent, &header, &claim, 2300);
        let sig = sec.sign_stmt(stmt, &mut rng).unwrap();
        assert!(sig.verify(stmt));
        let stmt2 = statement_digest(10, &parent, &header, &claim, 2300);
        assert!(!sig.verify(stmt2));
    }
}
