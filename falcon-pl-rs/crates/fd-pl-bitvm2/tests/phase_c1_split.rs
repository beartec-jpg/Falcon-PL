//! Phase C.1: verifier-split trace + Disprove taptree.

use fd_pl_bitvm2::abi::test_vk_hash;
use fd_pl_bitvm2::circuit::FplWithdrawValid;
use fd_pl_bitvm2::groth16::{prove, setup};
use fd_pl_bitvm2::leaf::{build_tree, withdraw_leaf_fr};
use fd_pl_bitvm2::verifier_split::{
    build_disprove_taptree, build_verify_trace, disprove_preimage, evaluate_chunk,
    is_fraudulent_transition, ChunkBackend, ChunkId, NUM_SCRIPTABLE_CHUNKS,
};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

fn dest() -> [u8; 20] {
    let mut d = [0u8; 20];
    d[19] = 0xc1;
    d
}

#[test]
fn c1_honest_trace_scriptable_chunks_sound() {
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).unwrap();
    let amount = 12_000u64;
    let note = "c1-honest";
    let leaf = withdraw_leaf_fr(note, &dest(), amount);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    let circuit = FplWithdrawValid::from_withdraw(
        2300,
        amount,
        &dest(),
        root,
        9,
        &[0x33; 32],
        note,
        &[0x44; 32],
        &path,
        &bits,
        test_vk_hash(),
    )
    .unwrap();
    let public = circuit.public_values();
    let proof = prove(&pk, circuit, &mut rng).unwrap();
    let trace = build_verify_trace(&vk, &public, &proof, note, &dest(), amount, root, &path, &bits)
        .unwrap();
    assert!(trace.groth16_ok);
    assert_eq!(trace.chunks.len(), NUM_SCRIPTABLE_CHUNKS + 1);
    for c in &trace.chunks {
        if c.backend == ChunkBackend::Sha256Script {
            let zin: [u8; 32] = hex::decode(&c.z_in_hex).unwrap().try_into().unwrap();
            let aux = hex::decode(&c.aux_hex).unwrap();
            let out = evaluate_chunk(c.id, &zin, &aux).unwrap();
            assert_eq!(hex::encode(out), c.z_out_hex);
            assert!(!is_fraudulent_transition(c.id, &zin, &out, &aux).unwrap());
        } else {
            assert_eq!(c.id, ChunkId::Groth16Pairing);
            assert_eq!(c.backend, ChunkBackend::BitvmBn254);
            let zin: [u8; 32] = hex::decode(&c.z_in_hex).unwrap().try_into().unwrap();
            let aux = hex::decode(&c.aux_hex).unwrap();
            let out = evaluate_chunk(c.id, &zin, &aux).unwrap();
            assert_eq!(hex::encode(out), c.z_out_hex);
        }
    }
}

#[test]
fn c1_tampered_zout_is_fraudulent() {
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).unwrap();
    let amount = 9_000u64;
    let note = "c1-fraud";
    let leaf = withdraw_leaf_fr(note, &dest(), amount);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    let circuit = FplWithdrawValid::from_withdraw(
        2300,
        amount,
        &dest(),
        root,
        1,
        &[0x01; 32],
        note,
        &[0x02; 32],
        &path,
        &bits,
        test_vk_hash(),
    )
    .unwrap();
    let public = circuit.public_values();
    let proof = prove(&pk, circuit, &mut rng).unwrap();
    let trace = build_verify_trace(&vk, &public, &proof, note, &dest(), amount, root, &path, &bits)
        .unwrap();
    let c = &trace.chunks[2]; // LeafAbsorb
    let zin: [u8; 32] = hex::decode(&c.z_in_hex).unwrap().try_into().unwrap();
    let aux = hex::decode(&c.aux_hex).unwrap();
    let mut bad_out = [0u8; 32];
    bad_out[0] = 0xff;
    assert!(is_fraudulent_transition(c.id, &zin, &bad_out, &aux).unwrap());
    let pre = disprove_preimage(c.id.index() as u8, &zin, &aux);
    let dig = Sha256::digest(&pre);
    assert_ne!(dig.as_slice(), bad_out);
}

#[test]
fn c1_disprove_taptree_has_scriptable_and_pack_leaves() {
    let tree = build_disprove_taptree();
    assert_eq!(tree.leaves.len(), NUM_SCRIPTABLE_CHUNKS + 1);
    assert_eq!(tree.merkle_root_hex.len(), 64);
    for leaf in &tree.leaves {
        assert!(!leaf.script_hex.is_empty());
        assert_eq!(leaf.tapleaf_hash_hex.len(), 64);
    }
    assert!(tree.bn254_pack.is_some());
}

#[test]
fn c1_groth16_chunk_evaluates_via_bitvm_pack_wrapper() {
    let out = evaluate_chunk(ChunkId::Groth16Pairing, &[0u8; 32], &[1]).unwrap();
    assert_eq!(out.len(), 32);
    assert_ne!(out, [0u8; 32]);
}
