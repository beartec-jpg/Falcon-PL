//! Public Groth16 setup interop (iden3 / snarkjs formats) for the FPL circuits.
//!
//! The circuit-specific phase 2 runs with audited snarkjs on a public
//! powers-of-tau phase 1. This module is the arkworks side:
//! * `.r1cs` export of an arkworks circuit (wire 0 = 1, then the public inputs,
//!   then witnesses - the arkworks instance/witness column order);
//! * `.wtns` export of a satisfying assignment (operators prove with
//!   `snarkjs groth16 prove final.zkey w.wtns`; nobody needs an arkworks PK);
//! * import of snarkjs `verification_key.json` / `proof.json` / `public.json`
//!   into arkworks types, serialized exactly like `groth16::vk_to_bytes`
//!   (= `BITVM2_WITHDRAW_VK_HEX`) and BitVM `vk.bin` / `proof.bin`.
//! No secret material is handled here: the toxic waste lives only inside each
//! contributor's snarkjs process.

use ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine};
use ark_ff::{BigInteger, PrimeField};
use ark_groth16::{Proof, VerifyingKey};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem, OptimizationGoal};
use serde_json::{json, Value};

pub struct Synth {
    pub num_instance: usize,
    pub num_witness: usize,
    pub num_constraints: usize,
    pub a: Vec<Vec<(Fr, usize)>>,
    pub b: Vec<Vec<(Fr, usize)>>,
    pub c: Vec<Vec<(Fr, usize)>>,
    /// full assignment [1, instance.., witness..]
    pub assignment: Vec<Fr>,
    pub satisfied: bool,
}

/// Synthesize like ark-groth16 (constraints goal, finalized/inlined).
pub fn synthesize<C: ConstraintSynthesizer<Fr>>(c: C) -> Result<Synth, String> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    cs.set_optimization_goal(OptimizationGoal::Constraints);
    c.generate_constraints(cs.clone()).map_err(|e| e.to_string())?;
    cs.finalize();
    let satisfied = cs.is_satisfied().map_err(|e| e.to_string())?;
    let m = cs.to_matrices().ok_or("no matrices")?;
    let inner = cs.borrow().ok_or("cs borrow")?;
    let mut assignment = inner.instance_assignment.clone();
    assignment.extend(inner.witness_assignment.iter().cloned());
    Ok(Synth {
        num_instance: m.num_instance_variables,
        num_witness: m.num_witness_variables,
        num_constraints: m.num_constraints,
        a: m.a,
        b: m.b,
        c: m.c,
        assignment,
        satisfied,
    })
}

fn fr_le(f: &Fr) -> [u8; 32] {
    let mut v = f.into_bigint().to_bytes_le();
    v.resize(32, 0);
    v.try_into().unwrap()
}

fn prime_le() -> [u8; 32] {
    let mut v = Fr::MODULUS.to_bytes_le();
    v.resize(32, 0);
    v.try_into().unwrap()
}

/// iden3 `.r1cs` v1 (header, constraints, wire->label).
pub fn r1cs_bytes(s: &Synth) -> Vec<u8> {
    let n_wires = (s.num_instance + s.num_witness) as u32;
    let mut hdr = Vec::new();
    hdr.extend(32u32.to_le_bytes());
    hdr.extend(prime_le());
    hdr.extend(n_wires.to_le_bytes());
    hdr.extend(0u32.to_le_bytes()); // public outputs
    hdr.extend(((s.num_instance - 1) as u32).to_le_bytes()); // public inputs
    hdr.extend((s.num_witness as u32).to_le_bytes()); // private inputs (label only)
    hdr.extend((n_wires as u64).to_le_bytes()); // labels
    hdr.extend((s.num_constraints as u32).to_le_bytes());
    let lc = |out: &mut Vec<u8>, row: &Vec<(Fr, usize)>| {
        let mut r: Vec<(usize, Fr)> = row.iter().map(|(f, i)| (*i, *f)).collect();
        r.sort_by_key(|x| x.0);
        out.extend((r.len() as u32).to_le_bytes());
        for (i, f) in r {
            out.extend((i as u32).to_le_bytes());
            out.extend(fr_le(&f));
        }
    };
    let mut cons = Vec::new();
    for k in 0..s.num_constraints {
        lc(&mut cons, &s.a[k]);
        lc(&mut cons, &s.b[k]);
        lc(&mut cons, &s.c[k]);
    }
    let mut labels = Vec::new();
    for i in 0..n_wires as u64 {
        labels.extend(i.to_le_bytes());
    }
    let mut out = b"r1cs".to_vec();
    out.extend(1u32.to_le_bytes());
    out.extend(3u32.to_le_bytes());
    for (ty, body) in [(1u32, hdr), (2, cons), (3, labels)] {
        out.extend(ty.to_le_bytes());
        out.extend((body.len() as u64).to_le_bytes());
        out.extend(body);
    }
    out
}

/// iden3 `.wtns` v2.
pub fn wtns_bytes(s: &Synth) -> Vec<u8> {
    let mut hdr = Vec::new();
    hdr.extend(32u32.to_le_bytes());
    hdr.extend(prime_le());
    hdr.extend((s.assignment.len() as u32).to_le_bytes());
    let mut w = Vec::new();
    for f in &s.assignment {
        w.extend(fr_le(f));
    }
    let mut out = b"wtns".to_vec();
    out.extend(2u32.to_le_bytes());
    out.extend(2u32.to_le_bytes());
    for (ty, body) in [(1u32, hdr), (2, w)] {
        out.extend(ty.to_le_bytes());
        out.extend((body.len() as u64).to_le_bytes());
        out.extend(body);
    }
    out
}

fn dec<F: PrimeField>(v: &Value) -> Result<F, String> {
    let s = v.as_str().ok_or("expected decimal string")?;
    let mut acc = F::zero();
    let ten = F::from(10u64);
    for ch in s.chars() {
        let d = ch.to_digit(10).ok_or_else(|| format!("bad digit in {s}"))?;
        acc = acc * ten + F::from(d as u64);
    }
    Ok(acc)
}

fn to_dec<F: PrimeField>(f: &F) -> String {
    f.into_bigint().to_string()
}

fn g1(v: &Value) -> Result<G1Affine, String> {
    let p = G1Affine::new_unchecked(dec::<Fq>(&v[0])?, dec::<Fq>(&v[1])?);
    if !p.is_on_curve() || !p.is_in_correct_subgroup_assuming_on_curve() {
        return Err("G1 point not on curve".into());
    }
    Ok(p)
}

fn g2(v: &Value) -> Result<G2Affine, String> {
    let x = Fq2::new(dec::<Fq>(&v[0][0])?, dec::<Fq>(&v[0][1])?);
    let y = Fq2::new(dec::<Fq>(&v[1][0])?, dec::<Fq>(&v[1][1])?);
    let p = G2Affine::new_unchecked(x, y);
    if !p.is_on_curve() || !p.is_in_correct_subgroup_assuming_on_curve() {
        return Err("G2 point not on curve / subgroup".into());
    }
    Ok(p)
}

fn g1j(p: &G1Affine) -> Value {
    json!([to_dec(&p.x), to_dec(&p.y), "1"])
}
fn g2j(p: &G2Affine) -> Value {
    json!([[to_dec(&p.x.c0), to_dec(&p.x.c1)], [to_dec(&p.y.c0), to_dec(&p.y.c1)], ["1", "0"]])
}

pub fn vk_from_snarkjs(v: &Value) -> Result<VerifyingKey<Bn254>, String> {
    if v["protocol"] != "groth16" || v["curve"] != "bn128" {
        return Err("need protocol groth16, curve bn128".into());
    }
    let ic: Vec<G1Affine> = v["IC"].as_array().ok_or("IC")?.iter().map(g1).collect::<Result<_, _>>()?;
    if v["nPublic"].as_u64() != Some(ic.len() as u64 - 1) {
        return Err("nPublic != IC.len()-1".into());
    }
    Ok(VerifyingKey {
        alpha_g1: g1(&v["vk_alpha_1"])?,
        beta_g2: g2(&v["vk_beta_2"])?,
        gamma_g2: g2(&v["vk_gamma_2"])?,
        delta_g2: g2(&v["vk_delta_2"])?,
        gamma_abc_g1: ic,
    })
}

pub fn vk_to_snarkjs(vk: &VerifyingKey<Bn254>) -> Value {
    json!({"protocol": "groth16", "curve": "bn128", "nPublic": vk.gamma_abc_g1.len() - 1,
        "vk_alpha_1": g1j(&vk.alpha_g1), "vk_beta_2": g2j(&vk.beta_g2), "vk_gamma_2": g2j(&vk.gamma_g2),
        "vk_delta_2": g2j(&vk.delta_g2), "IC": vk.gamma_abc_g1.iter().map(g1j).collect::<Vec<_>>()})
}

pub fn proof_from_snarkjs(v: &Value) -> Result<Proof<Bn254>, String> {
    Ok(Proof { a: g1(&v["pi_a"])?, b: g2(&v["pi_b"])?, c: g1(&v["pi_c"])? })
}

pub fn public_from_snarkjs(v: &Value) -> Result<Vec<Fr>, String> {
    v.as_array().ok_or("public.json must be an array")?.iter().map(dec::<Fr>).collect()
}

pub fn public_to_snarkjs(p: &[Fr]) -> Value {
    json!(p.iter().map(to_dec).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_snark::SNARK;

    fn demo_v1() -> crate::FplWithdrawValid {
        let dest = [9u8; 20];
        let leaf = crate::withdraw_leaf_fr("n", &dest, 1000);
        let (root, path, bits) = crate::build_tree(&[leaf], 0).unwrap();
        crate::FplWithdrawValid::from_withdraw(2300, 1000, &dest, root, 64, &[1u8; 32], "n", &[2u8; 32], &path, &bits, crate::test_vk_hash()).unwrap()
    }

    #[test]
    fn synth_matches_ark_and_formats() {
        let s = synthesize(demo_v1()).unwrap();
        assert!(s.satisfied);
        assert_eq!(s.assignment.len(), s.num_instance + s.num_witness);
        assert_eq!(&s.assignment[1..s.num_instance], &demo_v1().public_values()[..], "wires 1..=nPub are the public inputs, in order");
        let r = r1cs_bytes(&s);
        assert_eq!(&r[..4], b"r1cs");
        let w = wtns_bytes(&s);
        assert_eq!(w.len(), 4 + 4 + 4 + 12 + 40 + 12 + 32 * s.assignment.len());
    }

    #[test]
    fn vk_proof_json_roundtrip_verifies() {
        let (pk, vk) = crate::groth16::setup_from_seed([5u8; 32]).unwrap();
        let c = demo_v1();
        let public = c.public_values();
        let proof = ark_groth16::Groth16::<Bn254>::prove(&pk, c, &mut rand::rngs::OsRng).unwrap();
        let vk2 = vk_from_snarkjs(&vk_to_snarkjs(&vk)).unwrap();
        assert_eq!(crate::groth16::vk_to_bytes(&vk2).unwrap(), crate::groth16::vk_to_bytes(&vk).unwrap());
        let pj = json!({"pi_a": g1j(&proof.a), "pi_b": g2j(&proof.b), "pi_c": g1j(&proof.c), "protocol": "groth16"});
        let p2 = proof_from_snarkjs(&pj).unwrap();
        let pub2 = public_from_snarkjs(&public_to_snarkjs(&public)).unwrap();
        assert!(crate::groth16::verify(&vk2, &pub2, &p2).unwrap());
        let mut bad = vk_to_snarkjs(&vk);
        bad["vk_alpha_1"][1] = json!("5");
        assert!(vk_from_snarkjs(&bad).is_err(), "off-curve point rejected");
    }

    /// Real snarkjs output (DRY-RUN setup on the box: local ptau 2^13, 2 local
    /// contributions, fixed beacon - NOT a usable key): zkey from our `.r1cs`,
    /// proof from our demo `.wtns`. The proof verifies in arkworks under the
    /// imported VK, and snarkjs' public signals equal the Rust `public_values()`
    /// (wire order is right).
    #[test]
    fn snarkjs_dryrun_proof_verifies_in_arkworks() {
        let vk = vk_from_snarkjs(&serde_json::from_str(include_str!("../testdata/dryrun_v1_vk.json")).unwrap()).unwrap();
        let proof = proof_from_snarkjs(&serde_json::from_str(include_str!("../testdata/dryrun_v1_proof.json")).unwrap()).unwrap();
        let public = public_from_snarkjs(&serde_json::from_str(include_str!("../testdata/dryrun_v1_public.json")).unwrap()).unwrap();
        let dest = [0x11u8; 20];
        let leaf = crate::withdraw_leaf_fr("fpl-setup-demo", &dest, 10_000);
        let (root, path, bits) = crate::build_tree(&[leaf], 0).unwrap();
        let c = crate::FplWithdrawValid::from_withdraw(2300, 10_000, &dest, root, 64, &[0x22; 32], "fpl-setup-demo", &[0x33; 32], &path, &bits, crate::test_vk_hash()).unwrap();
        assert_eq!(public, c.public_values());
        assert!(crate::groth16::verify(&vk, &public, &proof).unwrap());
        let mut wrong = public.clone();
        wrong[0] += Fr::from(1u64);
        assert!(!crate::groth16::verify(&vk, &wrong, &proof).unwrap());
    }
}
