//! Phase C.1b: BitVM BN254/Groth16 pack bridge.

use fd_pl_bitvm2::abi::test_vk_hash;
use fd_pl_bitvm2::circuit::FplWithdrawValid;
use fd_pl_bitvm2::groth16::{prove, setup};
use fd_pl_bitvm2::leaf::{build_tree, withdraw_leaf_fr};
use fd_pl_bitvm2::verifier_split::{
    build_disprove_taptree, build_verify_trace, default_manifest, evaluate_chunk,
    is_fraudulent_transition, pack_commitment, pack_disprove_preimage, vendor_bitvm_present,
    verify_vendor_constants, ChunkBackend, ChunkId, BITVM_NUM_TAPS, NUM_CHUNKS,
    NUM_SCRIPTABLE_CHUNKS, PACK_ID, UPSTREAM_COMMIT,
};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn dest() -> [u8; 20] {
    let mut d = [0u8; 20];
    d[19] = 0xcb;
    d
}

fn falcon_pl_rs_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn c1b_manifest_matches_bitvm_api_constants() {
    let m = default_manifest();
    assert_eq!(m.pack_id, PACK_ID);
    assert_eq!(m.upstream_commit, UPSTREAM_COMMIT);
    assert_eq!(m.num_taps, BITVM_NUM_TAPS);
    assert_eq!(m.num_taps, m.num_hash + 1);
    assert_eq!(m.entrypoints.generate_partial_scripts, "api_generate_partial_script");
    let c = pack_commitment(&m);
    assert_ne!(c, [0u8; 32]);
}

#[test]
fn c1b_vendor_bitvm_constants_match_when_present() {
    let root = falcon_pl_rs_root();
    if !vendor_bitvm_present(&root) {
        eprintln!("skip: vendor/BitVM not checked out");
        return;
    }
    verify_vendor_constants(&root).expect("BitVM api constants drifted — bump bn254_pack pins");
}

#[test]
fn c1b_groth16_chunk_uses_bitvm_backend_and_evaluates() {
    let mut rng = OsRng;
    let (pk, vk) = setup(&mut rng).unwrap();
    let amount = 15_000u64;
    let note = "c1b-pack";
    let leaf = withdraw_leaf_fr(note, &dest(), amount);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    let circuit = FplWithdrawValid::from_withdraw(
        2300,
        amount,
        &dest(),
        root,
        3,
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
    let trace = build_verify_trace(&vk, &public, &proof, note, &dest(), amount, root, &path, &bits)
        .unwrap();
    assert_eq!(trace.chunks.len(), NUM_CHUNKS);
    let g = trace.chunks.last().unwrap();
    assert_eq!(g.id, ChunkId::Groth16Pairing);
    assert_eq!(g.backend, ChunkBackend::BitvmBn254);
    let zin: [u8; 32] = hex::decode(&g.z_in_hex).unwrap().try_into().unwrap();
    let aux = hex::decode(&g.aux_hex).unwrap();
    let out = evaluate_chunk(g.id, &zin, &aux).unwrap();
    assert_eq!(hex::encode(out), g.z_out_hex);
    assert!(!is_fraudulent_transition(g.id, &zin, &out, &aux).unwrap());
    let mut bad = out;
    bad[0] ^= 0xff;
    assert!(is_fraudulent_transition(g.id, &zin, &bad, &aux).unwrap());
}

#[test]
fn c1b_taptree_includes_pack_bind_leaf() {
    let tree = build_disprove_taptree();
    assert_eq!(tree.leaves.len(), NUM_SCRIPTABLE_CHUNKS + 1);
    let pack = tree.leaves.last().unwrap();
    assert_eq!(pack.chunk_id, ChunkId::Groth16Pairing.index() as u8);
    assert_eq!(pack.kind, "bitvm_bn254_pack");
    assert!(tree.bn254_pack.is_some());
    assert_eq!(tree.bn254_pack.as_ref().unwrap().num_taps, BITVM_NUM_TAPS);
    let expected = hex::encode(pack_commitment(&default_manifest()));
    assert_eq!(
        tree.bn254_pack_commitment_hex.as_deref(),
        Some(expected.as_str())
    );
    let pre = pack_disprove_preimage(&default_manifest());
    let dig = Sha256::digest(&pre);
    assert_eq!(dig.len(), 32);
}
