//! Phase C emulator CLI — honest payout + false claim disprove demos.

use clap::{Parser, Subcommand};
use fd_pl_bitvm2::abi::test_vk_hash;
use fd_pl_bitvm2::circuit::FplWithdrawValid;
use fd_pl_bitvm2::emulator::Emulator;
use fd_pl_bitvm2::groth16::{proof_to_bytes, prove, setup};
use fd_pl_bitvm2::instance::TestnetInstance;
use fd_pl_bitvm2::leaf::{build_tree, withdraw_leaf_fr};
use fd_pl_bitvm2::protocol::{ClaimPhase, InstanceConfig, Timeouts};
use rand::rngs::OsRng;

#[derive(Parser)]
#[command(name = "falcon-pl-bitvm2-emu", about = "BitVM2 Phase C emulator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run in-process e2e: honest claim → timeout → payout.
    Honest,
    /// Run in-process e2e: challenge → assert → disprove invalid proof.
    Disprove,
    /// Print a testnet instance JSON skeleton.
    Skeleton,
}

fn dest() -> [u8; 20] {
    let mut d = [0u8; 20];
    d[19] = 0x42;
    d
}

fn header() -> [u8; 32] {
    [0x11; 32]
}

fn pegout() -> [u8; 32] {
    [0x22; 32]
}

fn make_honest_circuit(amount: u64, note: &str) -> FplWithdrawValid {
    let leaf = withdraw_leaf_fr(note, &dest(), amount);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    FplWithdrawValid::from_withdraw(
        2300,
        amount,
        &dest(),
        root,
        100,
        &header(),
        note,
        &pegout(),
        &path,
        &bits,
        test_vk_hash(),
    )
    .unwrap()
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Honest => run_honest(),
        Cmd::Disprove => run_disprove(),
        Cmd::Skeleton => {
            let cfg = InstanceConfig {
                timeouts: Timeouts::demo(),
                ..Default::default()
            };
            let inst = TestnetInstance::skeleton(cfg, vec!["00".repeat(32)]);
            println!("{}", serde_json::to_string_pretty(&inst).unwrap());
            println!("instance_id_hint={}", inst.instance_id_hint());
            let tree = fd_pl_bitvm2::build_disprove_taptree();
            println!(
                "disprove_taptree_root={} leaves={}",
                tree.merkle_root_hex,
                tree.leaves.len()
            );
            let op = [0x11u8; 32];
            let setup = [[0x22u8; 32], [0x33u8; 32]];
            if let Ok(inst_tree) = fd_pl_bitvm2::build_instance_taptree(&op, 6, &setup) {
                println!(
                    "instance_taptree_root={} leaves={} kickoff={} setup_nofn={} nums_tag={}",
                    inst_tree.merkle_root_hex,
                    inst_tree.leaves.len(),
                    inst_tree.leaf(fd_pl_bitvm2::KIND_KICKOFF).is_some(),
                    inst_tree.leaf(fd_pl_bitvm2::KIND_SETUP).is_some(),
                    inst_tree.nums_tag
                );
            }
        }
    }
}

fn run_honest() {
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).expect("setup");
    let amount = 25_000u64;
    let circuit = make_honest_circuit(amount, "note-honest");
    let public = circuit.public_values();
    let proof = prove(&pk, circuit, &mut rng).expect("prove");
    let proof_bytes = proof_to_bytes(&proof).unwrap();

    let mut cfg = InstanceConfig::default();
    cfg.bond_sats = 100_000;
    cfg.timeouts = Timeouts::demo();
    let mut emu = Emulator::new(cfg, vk, 1_000);
    let id = emu
        .open_claim(
            "op1",
            "note-honest",
            amount,
            &hex::encode(pegout()),
            &proof_bytes,
            &public,
        )
        .expect("claim");
    emu.set_now(1_000 + Timeouts::demo().challenge_secs + 1);
    let paid = emu.payout_optimistic(&id).expect("payout");
    println!("honest ok claim={id} paid={paid} bond_left={}", emu.state.bond_remaining);
    assert_eq!(
        emu.state.claims.get(&id).unwrap().phase,
        ClaimPhase::PaidOptimistic
    );
}

fn run_disprove() {
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).expect("setup");
    let amount = 10_000u64;
    let circuit = make_honest_circuit(amount, "note-bad");
    let mut public = circuit.public_values();
    let proof = prove(&pk, circuit, &mut rng).expect("prove");
    let proof_bytes = proof_to_bytes(&proof).unwrap();
    // Tamper public amount so verify fails
    public[1] = ark_bn254::Fr::from(99_999u64);

    let mut cfg = InstanceConfig::default();
    cfg.bond_sats = 100_000;
    cfg.timeouts = Timeouts::demo();
    let mut emu = Emulator::new(cfg, vk, 1_000);
    let id = emu
        .open_claim(
            "op1",
            "note-bad",
            amount,
            &hex::encode(pegout()),
            &proof_bytes,
            &public,
        )
        .expect("claim");
    emu.challenge(&id, "challenger1").expect("challenge");
    emu.assert_claim(&id).expect("assert");
    let won = emu.disprove(&id).expect("disprove");
    println!(
        "disprove ok claim={id} challenger_won={won} bond_left={}",
        emu.state.bond_remaining
    );
    assert!(won);
    assert_eq!(emu.state.claims.get(&id).unwrap().phase, ClaimPhase::Disproved);
}
