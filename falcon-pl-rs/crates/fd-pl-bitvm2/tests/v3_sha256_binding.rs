use ark_groth16::Groth16;
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
use ark_snark::SNARK;
use fd_pl_bitvm2::*;
use rand::rngs::OsRng;

fn v3(amount: u64, pegout: [u8; 32], header: [u8; 32]) -> FplWithdrawValidV3 {
    let mut d = [0u8; 20];
    d[19] = 7;
    let leaf = withdraw_leaf_fr("v3", &d, amount);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    let inner = FplWithdrawValid::from_withdraw(2300, amount, &d, root, 9, &header, "v3", &pegout, &path, &bits, test_vk_hash()).unwrap();
    FplWithdrawValidV3::new(inner, pegout, header).unwrap()
}

#[test]
fn v3_native_matches_definition() {
    use sha2::{Digest, Sha256};
    let c = v3(5000, [0x44; 32], [0x33; 32]);
    let m = v3_message(&c.pegout_txid, &c.binding_commit());
    assert_eq!(m.len(), 68);
    let mut h: [u8; 32] = Sha256::digest(&m).into();
    h[0] &= 0x1f;
    assert_eq!(fr_to_be32(c.public_values()[0]), h);
}

#[test]
fn v3_sha256_binding_proves_and_binds() {
    let c = v3(5000, [0x44; 32], [0x33; 32]);
    let cs = ConstraintSystem::new_ref();
    c.clone().generate_constraints(cs.clone()).unwrap();
    assert!(cs.is_satisfied().unwrap());
    println!("V3 constraints: {}", cs.num_constraints());

    let mut rng = OsRng;
    let (pk, vk) = Groth16::<ark_bn254::Bn254>::circuit_specific_setup(c.clone(), &mut rng).unwrap();
    assert_eq!(vk.gamma_abc_g1.len(), 2, "exactly one public input");
    let proof = Groth16::<ark_bn254::Bn254>::prove(&pk, c.clone(), &mut rng).unwrap();
    assert!(Groth16::<ark_bn254::Bn254>::verify(&vk, &c.public_values(), &proof).unwrap());
    // a different pegout txid (operator pays someone else) must not verify
    let other_peg = v3(5000, [0x45; 32], [0x33; 32]).public_values();
    assert!(!Groth16::<ark_bn254::Bn254>::verify(&vk, &other_peg, &proof).unwrap());
    let other_amt = v3(5001, [0x44; 32], [0x33; 32]).public_values();
    assert!(!Groth16::<ark_bn254::Bn254>::verify(&vk, &other_amt, &proof).unwrap());
    let other_chk = v3(5000, [0x44; 32], [0x34; 32]).public_values();
    assert!(!Groth16::<ark_bn254::Bn254>::verify(&vk, &other_chk, &proof).unwrap());
}

#[test]
fn v3_rejects_mismatched_bytes() {
    let mut c = v3(5000, [0x44; 32], [0x33; 32]);
    c.pegout_txid[5] ^= 1; // bytes no longer match pegout_hi/lo
    let cs = ConstraintSystem::new_ref();
    c.generate_constraints(cs.clone()).unwrap();
    assert!(!cs.is_satisfied().unwrap());
}
