//! M1 node-level activation gate: before `bitvm2_m1_from_height` nothing changes
//! (V2 attest refused, no checkpoints, legacy paths open); from it, V2 is the only
//! attest path and checkpoints are recorded in rail state.

use super::*;

fn ring() -> KeyRing {
    let mut k = KeyRing::new(CryptoScheme::HmacDev);
    k.generate_many(["alice", "v1"]);
    k
}

fn node() -> Node {
    let mut n = Node::new("v1", ChainConfig { min_fee: 0, ..Default::default() }, ring(), &[("alice".into(), 1_000_000)]);
    n.register_validator("v1", 1000);
    n
}

fn attest() -> crate::bridge::Bitvm2AttestV2 {
    crate::bridge::Bitvm2AttestV2 {
        note_id: "n".into(),
        claim_txid_hex: "cc".repeat(32),
        checkpoint_height: 63,
        btc_height: 1,
        pegout_raw_tx_hex: String::new(),
        merkle_path: vec![],
        merkle_index: 0,
        dest_lock_abort_commit_hex: String::new(),
        proof_hex: String::new(),
        public_hex: vec![],
        statement_digest_hex: String::new(),
    }
}

fn v2_tx(n: &Node, k: &KeyRing) -> Tx {
    let seq = n.state.get("alice").sequence;
    Tx::sign_body(k, "alice", seq, "", 0, 0, n.cfg.network_id, TxBody::Bitvm2AttestV2 { attest: attest() }).unwrap()
}

#[test]
fn testnet_2300_m1_is_off() {
    assert_eq!(crate::params_v2::bitvm2_m1_from_height(crate::NETWORK_ID_PUBLIC_TESTNET_2300), 692_000);
    let n = node();
    assert!(!n.bitvm2_m1_at(n.tip_height + 1));
    assert!(!n.bitvm2_m1_at(u64::MAX - 1));
}

#[test]
fn before_activation_v2_refused_and_no_checkpoints() {
    let k = ring();
    let mut n = node();
    n.bitvm2_m1_from_height = u64::MAX;
    let e = n.apply_protocol_tx(&v2_tx(&n, &k)).unwrap_err();
    assert!(e.contains("before the M1 height"), "{e}");
    // Legacy attest is not refused by the gate (fails later for its own reasons).
    let e = n.rail_bitvm2_attest("n", &"aa".repeat(32), &"cc".repeat(32), "", &[], "", "op").unwrap_err();
    assert!(!e.contains("M1"), "{e}");
    let mut b = n.bridge.clone();
    let before = serde_json::to_vec(&b).unwrap();
    n.prune_eth_exec_for_ledger(&mut b, 128);
    assert_eq!(before, serde_json::to_vec(&b).unwrap(), "pre-M1 ledger must not change rail state/commitment");
}

#[test]
fn from_activation_v2_only_and_checkpoints_recorded() {
    let k = ring();
    let mut n = node();
    n.bitvm2_m1_from_height = n.tip_height + 1;
    // V2 passes the gate and is checked by the rail (no ceremony VK here).
    let e = n.apply_protocol_tx(&v2_tx(&n, &k)).unwrap_err();
    assert!(!e.contains("before the M1 height"), "{e}");
    let e = n.rail_bitvm2_attest("n", &"aa".repeat(32), &"cc".repeat(32), "", &[], "", "op").unwrap_err();
    assert!(e.contains("Bitvm2AttestV2"), "{e}");
    let mut b = n.bridge.clone();
    n.prune_eth_exec_for_ledger(&mut b, 128);
    let r = b.rail(crate::bridge::RAIL_BTC).unwrap();
    assert!(r.fpl_checkpoints.contains_key(&127));
}

#[test]
fn before_activation_v2_never_admitted_to_mempool() {
    let k = ring();
    let mut n = node();
    n.bitvm2_m1_from_height = u64::MAX;
    let e = n.submit_tx(v2_tx(&n, &k)).unwrap_err();
    assert!(e.contains("before the M1 height"), "{e}");
    assert_eq!(n.mempool.len(), 0, "a 2.9.74 seat could not decode it");
}
