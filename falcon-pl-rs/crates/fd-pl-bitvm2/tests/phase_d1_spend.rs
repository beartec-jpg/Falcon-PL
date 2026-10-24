//! Phase D.1: instance spend graph — no watch key, n-of-n Challenge.

use fd_pl_bitvm2::spend::{
    build_assert_connector_taptree, build_instance_taptree, instance_has_only_safe_leaves,
    operator_recover_script, payout_optimistic_script, push_csv, script_commits_xonly,
    setup_nofn_script, ASSERT_CSV_BLOCKS, INSTANCE_NUMS_TAG, KIND_EQUIVOCATION, KIND_OPERATOR_RECOVER,
    KIND_KICKOFF, KIND_PAYOUT, KIND_SETUP, PAYOUT_CSV_BLOCKS,
};
use fd_pl_bitvm2::winternitz;
use fd_pl_bitvm2::verifier_split::{merkle_path, merkle_root, tapleaf_hash};
use sha2::{Digest, Sha256};

fn xonly(tag: &str) -> [u8; 32] {
    Sha256::digest(tag.as_bytes()).into()
}

fn setup_keys() -> [[u8; 32]; 2] {
    [xonly("setup-a"), xonly("setup-b")]
}

#[test]
fn payout_script_is_csv_plus_operator_not_anyone() {
    let op = xonly("operator");
    let s = payout_optimistic_script(PAYOUT_CSV_BLOCKS, &op);
    assert!(script_commits_xonly(&s, &op));
    assert_eq!(*s.last().unwrap(), 0xac); // CHECKSIG, not OP_TRUE
    assert!(!s.contains(&0x51), "payout must not be OP_TRUE");
    let csv = push_csv(PAYOUT_CSV_BLOCKS);
    assert!(s.starts_with(&csv));
    assert_eq!(s[csv.len()], 0xb2); // CSV
}

#[test]
fn push_csv_sign_bit_for_144() {
    // 144 = 0x90 would look negative as a 1-byte CScriptNum.
    assert_eq!(push_csv(144), vec![0x02, 0x90, 0x00]);
    assert_eq!(push_csv(6), vec![0x56]);
}

#[test]
fn setup_nofn_rejects_single_key() {
    let one = [xonly("only")];
    assert!(setup_nofn_script(&one).is_err());
}

#[test]
fn instance_tree_is_kickoff_and_setup_only() {
    let op = xonly("operator");
    let setup = setup_keys();
    let tree = build_instance_taptree(&op, 6, &setup).unwrap();
    assert_eq!(tree.nums_tag.as_bytes(), INSTANCE_NUMS_TAG);
    assert_eq!(tree.leaves.len(), 2);
    assert!(instance_has_only_safe_leaves(&tree));
    assert!(tree.leaf(KIND_KICKOFF).is_some());
    assert!(tree.leaf(KIND_SETUP).is_some());
    assert!(tree.leaf(KIND_PAYOUT).is_none());
    let kickoff = hex::decode(&tree.leaf(KIND_KICKOFF).unwrap().script_hex).unwrap();
    assert!(script_commits_xonly(&kickoff, &op));
    assert!(!kickoff.contains(&0x51), "no OP_TRUE");
    assert!(!kickoff.contains(&0xb2), "kickoff must not CSV");
    for pk in &setup {
        assert!(!script_commits_xonly(&kickoff, pk));
    }
    let setup_script = hex::decode(&tree.leaf(KIND_SETUP).unwrap().script_hex).unwrap();
    for pk in &setup {
        assert!(script_commits_xonly(&setup_script, pk));
    }
}

#[test]
fn instance_merkle_path_reconstructs_root() {
    let op = xonly("operator");
    let tree = build_instance_taptree(&op, 6, &setup_keys()).unwrap();
    let hashes: Vec<[u8; 32]> = tree
        .leaves
        .iter()
        .map(|l| {
            let s = hex::decode(&l.script_hex).unwrap();
            tapleaf_hash(&s)
        })
        .collect();
    let root = merkle_root(&hashes);
    assert_eq!(hex::encode(root), tree.merkle_root_hex);
    for i in 0..hashes.len() {
        let path = merkle_path(&hashes, i).unwrap();
        let mut h = hashes[i];
        for sib in &path {
            h = fd_pl_bitvm2::verifier_split::tapbranch(&h, sib);
        }
        assert_eq!(h, root, "leaf {i}");
    }
}

#[test]
fn assert_connector_recover_is_csv_operator_not_true() {
    let op = xonly("operator");
    let setup = setup_keys();
    let tree = build_assert_connector_taptree(&setup, ASSERT_CSV_BLOCKS, &op, None).unwrap();
    let rec = hex::decode(&tree.leaf(KIND_OPERATOR_RECOVER).unwrap().script_hex).unwrap();
    assert_eq!(rec, operator_recover_script(ASSERT_CSV_BLOCKS, &op));
    assert_ne!(*rec.last().unwrap(), 0x51);
    assert!(tree.leaf(KIND_SETUP).is_some());
    assert!(tree.leaf(KIND_EQUIVOCATION).is_none());
}

#[test]
fn lamport_equivocation_detected_and_script_bound() {
    let mut i = 0u8;
    let fill = |buf: &mut [u8]| {
        for b in buf.iter_mut() {
            *b = i;
            i = i.wrapping_add(1);
        }
    };
    let (sk, pk) = winternitz::keygen(fill);
    let msg_a = [0u8; 32];
    let mut msg_b = [0u8; 32];
    msg_b[0] = 0x80; // bit 0 differs
    let sa = winternitz::sign(&sk, &msg_a).unwrap();
    let sb = winternitz::sign(&sk, &msg_b).unwrap();
    winternitz::verify(&pk, &msg_a, &sa).unwrap();
    winternitz::verify(&pk, &msg_b, &sb).unwrap();
    let (p0, p1) = winternitz::equivocation_preimages(None, &sa, &sb, &msg_a, &msg_b, 0).unwrap();
    let (pk0, pk1) = pk.bit_pks(0).unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(Sha256::digest(p0).as_slice(), pk0);
    assert_eq!(Sha256::digest(p1).as_slice(), pk1);
    let op = xonly("operator");
    let tree = build_assert_connector_taptree(&setup_keys(), 6, &op, Some(&pk)).unwrap();
    let eq = tree
        .leaves
        .iter()
        .filter(|l| l.kind == KIND_EQUIVOCATION)
        .count();
    assert_eq!(eq, winternitz::LAMPORT_BITS);
}

#[test]
fn operator_key_changes_instance_root() {
    let a = build_instance_taptree(&xonly("op-a"), 6, &setup_keys()).unwrap();
    let b = build_instance_taptree(&xonly("op-b"), 6, &setup_keys()).unwrap();
    assert_ne!(a.merkle_root_hex, b.merkle_root_hex);
}
