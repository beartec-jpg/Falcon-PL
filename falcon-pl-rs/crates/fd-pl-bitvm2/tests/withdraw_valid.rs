//! Phase B acceptance tests: honest prove/verify; reject bad dest/amount; nullifier.

use ark_bn254::Fr;
use fd_pl_bitvm2::abi::{nullifier, test_vk_hash};
use fd_pl_bitvm2::circuit::FplWithdrawValid;
use fd_pl_bitvm2::groth16::{prove, setup, verify};
use fd_pl_bitvm2::leaf::{build_tree, verify_path, withdraw_leaf_fr};
use rand::rngs::OsRng;
use std::collections::HashSet;


/// Profile-independent unsatisfiability check: (1) the constraint system is
/// unsatisfied, and (2) whatever prove() does (panics in debug, returns a
/// garbage proof in release), no accepted proof results.
fn assert_rejected(pk: &ark_groth16::ProvingKey<ark_bn254::Bn254>, vk: &ark_groth16::VerifyingKey<ark_bn254::Bn254>, c: FplWithdrawValid) {
    use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
    let cs = ConstraintSystem::<Fr>::new_ref();
    c.clone().generate_constraints(cs.clone()).unwrap();
    assert!(!cs.is_satisfied().unwrap(), "constraint system must be unsatisfied");
    let public = c.public_values();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prove(pk, c, &mut OsRng)));
    if let Ok(Ok(proof)) = r {
        assert!(!verify(vk, &public, &proof).unwrap_or(false), "bad witness produced an accepted proof");
    }
}

fn dest_a() -> [u8; 20] {
    let mut d = [0u8; 20];
    d[19] = 0x11;
    d
}

fn dest_b() -> [u8; 20] {
    let mut d = [0u8; 20];
    d[19] = 0x22;
    d
}

fn pegout() -> [u8; 32] {
    let mut t = [0u8; 32];
    t[31] = 0xab;
    t
}

fn header() -> [u8; 32] {
    let mut h = [0u8; 32];
    h[0] = 0xcd;
    h
}

fn fixture(note: &str, dest: &[u8; 20], amount: u64) -> (Fr, Vec<Fr>, Vec<bool>, FplWithdrawValid) {
    let leaf = withdraw_leaf_fr(note, dest, amount);
    let other = withdraw_leaf_fr("other-note", &dest_b(), 1);
    let leaves = vec![leaf, other, Fr::from(3u64), Fr::from(4u64)];
    let (root, path, bits) = build_tree(&leaves, 0).expect("tree");
    assert!(verify_path(leaf, &path, &bits, root));
    let circuit = FplWithdrawValid::from_withdraw(
        2300,
        amount,
        dest,
        root,
        42,
        &header(),
        note,
        &pegout(),
        &path,
        &bits,
        test_vk_hash(),
    )
    .expect("circuit");
    (root, path, bits, circuit)
}

#[test]
fn honest_withdraw_proves_and_verifies() {
    let (_root, _path, _bits, circuit) = fixture("note-alice", &dest_a(), 50_000);
    let public = circuit.public_values();
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).expect("setup");
    let proof = prove(&pk, circuit, &mut rng).expect("prove");
    assert!(verify(&vk, &public, &proof).expect("verify"));
}

#[test]
fn wrong_amount_fails_verify() {
    let (root, path, bits, circuit) = fixture("note-alice", &dest_a(), 50_000);
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).expect("setup");
    let proof = prove(&pk, circuit.clone(), &mut rng).expect("prove");
    let mut bad_public = circuit.public_values();
    bad_public[1] = Fr::from(99_999u64);
    assert!(!verify(&vk, &bad_public, &proof).expect("verify"));

    // amount≠leaf → unsatisfied CS (prove panics in ark-groth16 debug)
    let bad = FplWithdrawValid::from_withdraw(
        2300,
        1,
        &dest_a(),
        root,
        42,
        &header(),
        "note-alice",
        &pegout(),
        &path,
        &bits,
        test_vk_hash(),
    )
    .unwrap();
    assert_rejected(&pk, &vk, bad);
}

#[test]
fn wrong_dest_native_path_rejects() {
    let leaf = withdraw_leaf_fr("note-alice", &dest_a(), 50_000);
    let leaves = vec![leaf, Fr::from(9u64)];
    let (root, path, bits) = build_tree(&leaves, 0).unwrap();
    let wrong_leaf = withdraw_leaf_fr("note-alice", &dest_b(), 50_000);
    assert!(!verify_path(wrong_leaf, &path, &bits, root));
}

#[test]
fn double_claim_nullifier_detects_reuse() {
    let mut spent = HashSet::new();
    let n1 = nullifier("note-alice");
    let n2 = nullifier("note-bob");
    assert!(spent.insert(n1));
    assert!(spent.insert(n2));
    assert!(!spent.insert(n1), "same note_id must collide");
}

#[test]
fn wrong_network_id_fails_at_satisfy() {
    // Network is a public input enforced == 2300. Building constraints with
    // network_id=9999 leaves the CS unsatisfied (caught at prove).
    let leaf = withdraw_leaf_fr("n", &dest_a(), 100);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    let circuit = FplWithdrawValid::from_withdraw(
        9999,
        100,
        &dest_a(),
        root,
        1,
        &header(),
        "n",
        &pegout(),
        &path,
        &bits,
        test_vk_hash(),
    )
    .unwrap();
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).unwrap();
    assert_rejected(&pk, &vk, circuit);
}
