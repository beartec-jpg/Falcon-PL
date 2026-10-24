use ark_groth16::Groth16;
use ark_snark::SNARK;
use fd_pl_bitvm2::*;
use rand::rngs::OsRng;

fn circuit(amount: u64) -> FplWithdrawValid {
    let mut d = [0u8; 20];
    d[19] = 7;
    let leaf = withdraw_leaf_fr("v2", &d, amount);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    FplWithdrawValid::from_withdraw(2300, amount, &d, root, 9, &[0x33; 32], "v2", &[0x44; 32], &path, &bits, test_vk_hash()).unwrap()
}

#[test]
fn v2_single_public_input_proves_and_binds() {
    let mut rng = OsRng;
    let c = FplWithdrawValidV2(circuit(5000));
    let (pk, vk) = Groth16::<ark_bn254::Bn254>::circuit_specific_setup(c.clone(), &mut rng).unwrap();
    assert_eq!(vk.gamma_abc_g1.len(), 2, "exactly one public input");
    let public = c.public_values();
    let proof = Groth16::<ark_bn254::Bn254>::prove(&pk, c.clone(), &mut rng).unwrap();
    assert!(Groth16::<ark_bn254::Bn254>::verify(&vk, &public, &proof).unwrap());
    // Wrong digest (different amount) must not verify.
    let other = FplWithdrawValidV2(circuit(5001)).public_values();
    assert!(!Groth16::<ark_bn254::Bn254>::verify(&vk, &other, &proof).unwrap());
}
