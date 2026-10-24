//! Phase C: optimistic engine e2e (emulator).

use ark_bn254::Fr;
use fd_pl_bitvm2::abi::test_vk_hash;
use fd_pl_bitvm2::circuit::FplWithdrawValid;
use fd_pl_bitvm2::emulator::Emulator;
use fd_pl_bitvm2::groth16::{proof_to_bytes, prove, setup};
use fd_pl_bitvm2::instance::TestnetInstance;
use fd_pl_bitvm2::leaf::{build_tree, withdraw_leaf_fr};
use fd_pl_bitvm2::protocol::{ClaimPhase, InstanceConfig, Timeouts};
use rand::rngs::OsRng;

fn dest() -> [u8; 20] {
    let mut d = [0u8; 20];
    d[19] = 0x7e;
    d
}

fn make_proof(note: &str, amount: u64) -> (Vec<u8>, Vec<Fr>, ark_groth16::VerifyingKey<ark_bn254::Bn254>) {
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).unwrap();
    let leaf = withdraw_leaf_fr(note, &dest(), amount);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    let circuit = FplWithdrawValid::from_withdraw(
        2300,
        amount,
        &dest(),
        root,
        7,
        &[0x55; 32],
        note,
        &[0x66; 32],
        &path,
        &bits,
        test_vk_hash(),
    )
    .unwrap();
    let public = circuit.public_values();
    let proof = prove(&pk, circuit, &mut rng).unwrap();
    (proof_to_bytes(&proof).unwrap(), public, vk)
}

#[test]
fn e2e_honest_claim_timeout_payout() {
    let amount = 40_000u64;
    let (proof, public, vk) = make_proof("e2e-honest", amount);
    let cfg = InstanceConfig {
        bond_sats: 200_000,
        timeouts: Timeouts::demo(),
        ..Default::default()
    };
    let mut emu = Emulator::new(cfg, vk, 10);
    let id = emu
        .open_claim("op", "e2e-honest", amount, &hex::encode([0x66; 32]), &proof, &public)
        .unwrap();
    assert!(emu.payout_optimistic(&id).is_err());
    emu.set_now(10 + Timeouts::demo().challenge_secs + 1);
    let paid = emu.payout_optimistic(&id).unwrap();
    assert_eq!(paid, amount);
    assert_eq!(emu.state.claims[&id].phase, ClaimPhase::PaidOptimistic);
    assert_eq!(emu.state.bond_remaining, 200_000 - amount);
}

#[test]
fn e2e_false_claim_disproved() {
    let amount = 15_000u64;
    let (proof, mut public, vk) = make_proof("e2e-false", amount);
    public[1] = Fr::from(1u64); // mismatch → verify fails
    let cfg = InstanceConfig {
        bond_sats: 100_000,
        timeouts: Timeouts::demo(),
        ..Default::default()
    };
    let mut emu = Emulator::new(cfg, vk, 100);
    let id = emu
        .open_claim("op", "e2e-false", amount, "aabb", &proof, &public)
        .unwrap();
    emu.challenge(&id, "c1").unwrap();
    emu.assert_claim(&id).unwrap();
    assert!(emu.disprove(&id).unwrap());
    assert_eq!(emu.state.claims[&id].phase, ClaimPhase::Disproved);
    assert_eq!(emu.state.bond_remaining, 100_000 - amount);
}

#[test]
fn e2e_valid_proof_disprove_fails_then_payout() {
    let amount = 8_000u64;
    let (proof, public, vk) = make_proof("e2e-valid-chal", amount);
    let cfg = InstanceConfig {
        bond_sats: 50_000,
        timeouts: Timeouts::demo(),
        ..Default::default()
    };
    let mut emu = Emulator::new(cfg, vk, 50);
    let id = emu
        .open_claim("op", "e2e-valid-chal", amount, "ccdd", &proof, &public)
        .unwrap();
    emu.challenge(&id, "c1").unwrap();
    emu.assert_claim(&id).unwrap();
    assert!(!emu.disprove(&id).unwrap()); // valid proof
    emu.set_now(50 + Timeouts::demo().disprove_secs + 1);
    let paid = emu.payout_optimistic(&id).unwrap();
    assert_eq!(paid, amount);
}

#[test]
fn testnet_skeleton_json() {
    let inst = TestnetInstance::skeleton(InstanceConfig::default(), vec!["aa".repeat(32)]);
    assert_eq!(inst.network, "bitcoin-testnet");
    assert!(!inst.instance_id_hint().is_empty());
    let data = TestnetInstance::claim_commit_script_data(&[0u8; 32]);
    assert_eq!(&data[..4], b"FPL2");
    assert_eq!(data.len(), 36);
}
