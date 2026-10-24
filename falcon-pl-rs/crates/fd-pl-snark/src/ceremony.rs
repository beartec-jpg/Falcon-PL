//! Groth16 from a public BN254 Powers-of-Tau (Hermez / PSE `.ptau`) plus
//! circuit-specific δ contributions. Tau is never sampled in this process.

use crate::circuit::FplLcCircuit;
use crate::falcon::circuit::{FplFalconQcCircuit, MAX_OPEN};
use ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G1Projective, G2Affine, G2Projective};
use ark_ec::{AffineRepr, CurveGroup, VariableBaseMSM};
use ark_ff::{Field, One, PrimeField, Zero};
use ark_groth16::{ProvingKey, VerifyingKey};
use ark_poly::{EvaluationDomain, GeneralEvaluationDomain};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem, OptimizationGoal, SynthesisMode};
use ark_serialize::CanonicalSerialize;
use rand::{CryptoRng, RngCore};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Hermez/snarkjs `.ptau` stores Montgomery little-endian Fq.
fn fq_ptau(b: &[u8]) -> Fq {
    let mont_as_int = Fq::from_le_bytes_mod_order(b);
    let r = Fq::from(2u64).pow([256]);
    mont_as_int * r.inverse().expect("R")
}

fn read_g1(b: &[u8]) -> Result<G1Affine, String> {
    if b.len() != 64 {
        return Err("g1 len".into());
    }
    if b.iter().all(|x| *x == 0) {
        return Ok(G1Affine::identity());
    }
    let x = fq_ptau(&b[..32]);
    let y = fq_ptau(&b[32..]);
    let p = G1Affine::new_unchecked(x, y);
    if !p.is_on_curve() {
        return Err("g1 not on curve".into());
    }
    Ok(p)
}

fn read_g2(b: &[u8]) -> Result<G2Affine, String> {
    if b.len() != 128 {
        return Err("g2 len".into());
    }
    if b.iter().all(|x| *x == 0) {
        return Ok(G2Affine::identity());
    }
    let x = Fq2::new(fq_ptau(&b[..32]), fq_ptau(&b[32..64]));
    let y = Fq2::new(fq_ptau(&b[64..96]), fq_ptau(&b[96..]));
    let p = G2Affine::new_unchecked(x, y);
    if !p.is_on_curve() {
        return Err("g2 not on curve".into());
    }
    Ok(p)
}

struct Ptau {
    power: u32,
    tau_g1: Vec<G1Affine>,
    tau_g2: Vec<G2Affine>,
    alpha_tau_g1: Vec<G1Affine>,
    beta_tau_g1: Vec<G1Affine>,
    beta_g2: G2Affine,
}

fn load_ptau(path: &Path) -> Result<Ptau, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != b"ptau" {
        return Err("not a ptau file".into());
    }
    let mut u32b = [0u8; 4];
    let mut u64b = [0u8; 8];
    f.read_exact(&mut u32b).map_err(|e| e.to_string())?;
    f.read_exact(&mut u32b).map_err(|e| e.to_string())?;
    let nsec = u32::from_le_bytes(u32b);
    let mut sections = std::collections::BTreeMap::new();
    for _ in 0..nsec {
        f.read_exact(&mut u32b).map_err(|e| e.to_string())?;
        let typ = u32::from_le_bytes(u32b);
        f.read_exact(&mut u64b).map_err(|e| e.to_string())?;
        let sz = u64::from_le_bytes(u64b);
        let off = f.stream_position().map_err(|e| e.to_string())?;
        sections.insert(typ, (off, sz));
        f.seek(SeekFrom::Current(sz as i64)).map_err(|e| e.to_string())?;
    }
    let (h_off, h_sz) = *sections.get(&1).ok_or("ptau missing header")?;
    f.seek(SeekFrom::Start(h_off)).map_err(|e| e.to_string())?;
    f.read_exact(&mut u32b).map_err(|e| e.to_string())?;
    let n8 = u32::from_le_bytes(u32b) as usize;
    let mut q = vec![0u8; n8];
    f.read_exact(&mut q).map_err(|e| e.to_string())?;
    f.read_exact(&mut u32b).map_err(|e| e.to_string())?;
    let power = u32::from_le_bytes(u32b);
    let _ = h_sz;
    let read_g1s = |f: &mut std::fs::File, off: u64, sz: u64| -> Result<Vec<G1Affine>, String> {
        f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        let n = (sz / 64) as usize;
        let mut buf = vec![0u8; sz as usize];
        f.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(read_g1(&buf[i * 64..(i + 1) * 64])?);
        }
        Ok(out)
    };
    let read_g2s = |f: &mut std::fs::File, off: u64, sz: u64| -> Result<Vec<G2Affine>, String> {
        f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        let n = (sz / 128) as usize;
        let mut buf = vec![0u8; sz as usize];
        f.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(read_g2(&buf[i * 128..(i + 1) * 128])?);
        }
        Ok(out)
    };
    let (o, s) = *sections.get(&2).ok_or("ptau tauG1")?;
    let tau_g1 = read_g1s(&mut f, o, s)?;
    let (o, s) = *sections.get(&3).ok_or("ptau tauG2")?;
    let tau_g2 = read_g2s(&mut f, o, s)?;
    let (o, s) = *sections.get(&4).ok_or("ptau alphaTauG1")?;
    let alpha_tau_g1 = read_g1s(&mut f, o, s)?;
    let (o, s) = *sections.get(&5).ok_or("ptau betaTauG1")?;
    let beta_tau_g1 = read_g1s(&mut f, o, s)?;
    let (o, s) = *sections.get(&6).ok_or("ptau betaG2")?;
    let beta_g2s = read_g2s(&mut f, o, s)?;
    if beta_g2s.is_empty() {
        return Err("ptau betaG2 empty".into());
    }
    Ok(Ptau {
        power,
        tau_g1,
        tau_g2,
        alpha_tau_g1,
        beta_tau_g1,
        beta_g2: beta_g2s[0],
    })
}

/// Radix-2 IFFT on G1 (Cooley–Tukey). `a.len()` must be a power of two.
fn ifft_g2(a: &mut [G2Projective], omega_inv: Fr, n_inv: Fr) {
    let n = a.len();
    debug_assert!(n.is_power_of_two());
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let w_len = omega_inv.pow([(n / len) as u64]);
        for i in (0..n).step_by(len) {
            let mut w = Fr::one();
            for j in 0..len / 2 {
                let u = a[i + j];
                let v = a[i + j + len / 2] * w;
                a[i + j] = u + v;
                a[i + j + len / 2] = u - v;
                w *= w_len;
            }
        }
        len <<= 1;
    }
    for p in a.iter_mut() {
        *p *= n_inv;
    }
}

fn ifft_g1(a: &mut [G1Projective], omega_inv: Fr, n_inv: Fr) {
    let n = a.len();
    debug_assert!(n.is_power_of_two());
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let w_len = omega_inv.pow([(n / len) as u64]);
        for i in (0..n).step_by(len) {
            let mut w = Fr::one();
            for j in 0..len / 2 {
                let u = a[i + j];
                let v = a[i + j + len / 2] * w;
                a[i + j] = u + v;
                a[i + j + len / 2] = u - v;
                w *= w_len;
            }
        }
        len <<= 1;
    }
    for p in a.iter_mut() {
        *p *= n_inv;
    }
}

fn msm_g1(bases: &[G1Affine], scalars: &[Fr]) -> Result<G1Projective, String> {
    G1Projective::msm(bases, scalars).map_err(|e| format!("msm g1: {e}"))
}

fn msm_g2(bases: &[G2Affine], scalars: &[Fr]) -> Result<G2Projective, String> {
    G2Projective::msm(bases, scalars).map_err(|e| format!("msm g2: {e}"))
}

fn eval_at_tau_g1(
    domain: usize,
    sparse: &[(u32, Fr)],
    lagrange: &[G1Affine],
) -> G1Projective {
    if sparse.is_empty() {
        return G1Projective::zero();
    }
    let mut acc = G1Projective::zero();
    for (i, c) in sparse {
        if (*i as usize) < domain && !c.is_zero() {
            acc += lagrange[*i as usize] * c;
        }
    }
    acc
}

/// Circuit-specific Groth16 PK/VK from a public `.ptau`. Initial δ = 1.
pub fn setup_from_ptau(
    ptau_path: &Path,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>, String), String> {
    setup_from_ptau_circuit(ptau_path, FplLcCircuit::default())
}

pub fn setup_falcon_from_ptau(
    ptau_path: &Path,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>, String), String> {
    setup_falcon_open_from_ptau::<MAX_OPEN>(ptau_path)
}

/// OPEN=1 is the live Sepolia path (four proofs, one vk). OPEN=4 is a single
/// circuit that does not fit a 15GiB setup.
pub fn setup_falcon_open_from_ptau<const OPEN: usize>(
    ptau_path: &Path,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>, String), String> {
    setup_from_ptau_circuit(ptau_path, FplFalconQcCircuit::<OPEN>::default())
}

pub fn setup_falcon_v2_from_ptau(
    ptau_path: &Path,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>, String), String> {
    setup_from_ptau_circuit(ptau_path, crate::FplFalconQcCircuitV2::default())
}

pub fn setup_from_ptau_circuit<C: ConstraintSynthesizer<Fr>>(
    ptau_path: &Path,
    circuit: C,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>, String), String> {
    let ptau = load_ptau(ptau_path)?;
    let cs = ConstraintSystem::<Fr>::new_ref();
    cs.set_optimization_goal(OptimizationGoal::Constraints);
    cs.set_mode(SynthesisMode::Setup);
    circuit
        .generate_constraints(cs.clone())
        .map_err(|e| format!("synth: {e}"))?;
    cs.finalize();
    let matrices = cs.to_matrices().ok_or("r1cs matrices")?;
    let n_cons = matrices.num_constraints;
    let n_inst = matrices.num_instance_variables;
    let n_wit = matrices.num_witness_variables;
    let n_vars = n_inst + n_wit;
    let domain = GeneralEvaluationDomain::<Fr>::new(n_cons + n_inst)
        .ok_or("domain")?;
    let n = domain.size();
    if ptau.tau_g1.len() < 2 * n || ptau.tau_g2.len() < n || ptau.alpha_tau_g1.len() < n || ptau.beta_tau_g1.len() < n
    {
        return Err(format!(
            "ptau power {} too small for domain {n} (tauG1={})",
            ptau.power,
            ptau.tau_g1.len()
        ));
    }
    // Lagrange bases via one IFFT each: [L_i(τ)]G, [α L_i(τ)]G, [β L_i(τ)]G, [L_i(τ)]H.
    let omega_inv = domain.group_gen_inv();
    let n_inv = Fr::from(n as u64).inverse().ok_or("n inv")?;
    let lagrange_ifft = |src: &[G1Affine]| -> Vec<G1Affine> {
        let mut lg: Vec<G1Projective> = src[..n].iter().map(|p| p.into_group()).collect();
        ifft_g1(&mut lg, omega_inv, n_inv);
        lg.iter().map(|p| p.into_affine()).collect()
    };
    let lagrange = lagrange_ifft(&ptau.tau_g1);
    let lagrange_alpha = lagrange_ifft(&ptau.alpha_tau_g1);
    let lagrange_beta = lagrange_ifft(&ptau.beta_tau_g1);
    let mut lg2: Vec<G2Projective> = ptau.tau_g2[..n].iter().map(|p| p.into_group()).collect();
    ifft_g2(&mut lg2, omega_inv, n_inv);
    let lagrange_g2: Vec<G2Affine> = lg2.iter().map(|p| p.into_affine()).collect();

    let mut a_sp = vec![Vec::<(u32, Fr)>::new(); n_vars];
    let mut b_sp = vec![Vec::<(u32, Fr)>::new(); n_vars];
    let mut c_sp = vec![Vec::<(u32, Fr)>::new(); n_vars];
    for (ci, row) in matrices.a.iter().enumerate() {
        for (coeff, var) in row {
            a_sp[*var].push((ci as u32, *coeff));
        }
    }
    for (ci, row) in matrices.b.iter().enumerate() {
        for (coeff, var) in row {
            b_sp[*var].push((ci as u32, *coeff));
        }
    }
    for (ci, row) in matrices.c.iter().enumerate() {
        for (coeff, var) in row {
            c_sp[*var].push((ci as u32, *coeff));
        }
    }

    let mut a_query = vec![G1Affine::identity(); n_vars];
    let mut b_g1_query = vec![G1Affine::identity(); n_vars];
    let mut b_g2_query = vec![G2Affine::identity(); n_vars];
    let mut gamma_abc = vec![G1Projective::zero(); n_inst];
    let mut l_query = vec![G1Projective::zero(); n_vars.saturating_sub(n_inst)];

    for j in 0..n_vars {
        let a_tau = eval_at_tau_g1(n, &a_sp[j], &lagrange);
        let b_tau = eval_at_tau_g1(n, &b_sp[j], &lagrange);
        let c_tau = eval_at_tau_g1(n, &c_sp[j], &lagrange);
        a_query[j] = a_tau.into_affine();
        b_g1_query[j] = b_tau.into_affine();
        let mut acc2 = G2Projective::zero();
        for (i, c) in &b_sp[j] {
            if (*i as usize) < n && !c.is_zero() {
                acc2 += lagrange_g2[*i as usize] * c;
            }
        }
        b_g2_query[j] = acc2.into_affine();
        let beta_a = eval_at_tau_g1(n, &a_sp[j], &lagrange_beta);
        let alpha_b = eval_at_tau_g1(n, &b_sp[j], &lagrange_alpha);
        let abc = beta_a + alpha_b + c_tau;
        if j < n_inst {
            // libsnark: instance vars also take Lagrange slot u[n_cons + j] on A.
            let extra = n_cons + j;
            if extra < n {
                a_query[j] = (a_tau + lagrange[extra]).into_affine();
                gamma_abc[j] = abc + lagrange_beta[extra];
            } else {
                gamma_abc[j] = abc;
            }
        } else {
            l_query[j - n_inst] = abc;
        }
    }

    // H query: [(τ^{n+i} − τ^i)/δ] G1 with δ=1.
    let mut h_query = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let hi = ptau.tau_g1[n + i].into_group() - ptau.tau_g1[i].into_group();
        h_query.push(hi.into_affine());
    }

    let vk = VerifyingKey::<Bn254> {
        alpha_g1: ptau.alpha_tau_g1[0],
        beta_g2: ptau.beta_g2,
        gamma_g2: ptau.tau_g2[0], // [1] G2, γ=1
        delta_g2: ptau.tau_g2[0], // δ=1 before contributions
        gamma_abc_g1: gamma_abc.iter().map(|p| p.into_affine()).collect(),
    };
    let pk = ProvingKey::<Bn254> {
        vk: vk.clone(),
        beta_g1: ptau.beta_tau_g1[0],
        delta_g1: ptau.tau_g1[0],
        a_query,
        b_g1_query,
        b_g2_query,
        h_query,
        l_query: l_query.iter().map(|p| p.into_affine()).collect(),
    };
    let mut hasher = Sha256::new();
    hasher.update(b"fpl-ptau-setup-v1|");
    hasher.update(ptau.power.to_le_bytes());
    hasher.update((n as u64).to_le_bytes());
    hasher.update((n_cons as u64).to_le_bytes());
    let digest = hex::encode(hasher.finalize());
    Ok((pk, vk, digest))
}

/// Circuit-specific phase-2: δ ← δ·x. Contributor never learns τ.
pub fn contribute_delta<R: RngCore + CryptoRng>(
    pk: &ProvingKey<Bn254>,
    rng: &mut R,
    tag: &str,
) -> Result<(ProvingKey<Bn254>, String), String> {
    let mut seed = [0u8; 32];
    rng.fill_bytes(&mut seed);
    let mut h = Sha256::new();
    h.update(b"fpl-groth16-delta-v1|");
    h.update(tag.as_bytes());
    h.update(seed);
    let hashed = h.finalize();
    let x = Fr::from_be_bytes_mod_order(&hashed);
    if x.is_zero() {
        return Err("delta x is zero".into());
    }
    let x_inv = x.inverse().ok_or("delta inv")?;
    seed.fill(0);
    apply_delta(pk, x, x_inv, tag)
}

pub fn contribute_beacon(pk: &ProvingKey<Bn254>, beacon: &[u8], tag: &str) -> Result<(ProvingKey<Bn254>, String), String> {
    let mut h = Sha256::new();
    h.update(b"fpl-groth16-beacon-v1|");
    h.update(tag.as_bytes());
    h.update(beacon);
    let hashed = h.finalize();
    let x = Fr::from_be_bytes_mod_order(&hashed);
    if x.is_zero() {
        return Err("beacon x is zero".into());
    }
    let x_inv = x.inverse().ok_or("beacon inv")?;
    apply_delta(pk, x, x_inv, tag)
}

fn apply_delta(
    pk: &ProvingKey<Bn254>,
    x: Fr,
    x_inv: Fr,
    tag: &str,
) -> Result<(ProvingKey<Bn254>, String), String> {
    let delta_g1 = (pk.delta_g1.into_group() * x).into_affine();
    let delta_g2 = (pk.vk.delta_g2.into_group() * x).into_affine();
    let h_query: Vec<G1Affine> = pk
        .h_query
        .iter()
        .map(|p| (p.into_group() * x_inv).into_affine())
        .collect();
    let l_query: Vec<G1Affine> = pk
        .l_query
        .iter()
        .map(|p| (p.into_group() * x_inv).into_affine())
        .collect();
    let mut vk = pk.vk.clone();
    vk.delta_g2 = delta_g2;
    let out = ProvingKey {
        vk: vk.clone(),
        beta_g1: pk.beta_g1,
        delta_g1,
        a_query: pk.a_query.clone(),
        b_g1_query: pk.b_g1_query.clone(),
        b_g2_query: pk.b_g2_query.clone(),
        h_query,
        l_query,
    };
    let mut hasher = Sha256::new();
    hasher.update(b"fpl-delta-transcript-v1|");
    hasher.update(tag.as_bytes());
    let mut buf = Vec::new();
    vk.delta_g2.serialize_uncompressed(&mut buf).map_err(|e| e.to_string())?;
    hasher.update(&buf);
    Ok((out, hex::encode(hasher.finalize())))
}

pub fn ptau_sha256(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    use std::io::Read;
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ec::AffineRepr;
    use ark_ff::BigInteger;

    #[test]
    fn ptau_g1_zero_and_mont_generator() {
        assert_eq!(read_g1(&[0u8; 64]).unwrap(), G1Affine::identity());
        // Montgomery LE of x=1,y=2 (Ethereum/ark BN254 generator).
        let r = Fq::from(2u64).pow([256]);
        let x = (Fq::from(1u64) * r).into_bigint();
        let y = (Fq::from(2u64) * r).into_bigint();
        let xb = x.to_bytes_le();
        let yb = y.to_bytes_le();
        let mut buf = [0u8; 64];
        buf[..xb.len()].copy_from_slice(&xb);
        buf[32..32 + yb.len()].copy_from_slice(&yb);
        let g = read_g1(&buf).unwrap();
        assert_eq!(g, G1Affine::generator());
    }

    #[test]
    fn open1_setup_entry_exists() {
        // Live FalconQcBridge uses OPEN=1. OPEN=4 is a different vk.
        let _ = setup_falcon_open_from_ptau::<1> as fn(&Path) -> _;
        let _ = setup_falcon_from_ptau as fn(&Path) -> _;
    }
}
