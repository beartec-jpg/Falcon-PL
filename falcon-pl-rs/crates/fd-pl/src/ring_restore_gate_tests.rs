//! #118: ring restore from the account's on-chain key is activation-gated.
//!
//! The key ring is node-local and not rebuilt on restart, so restoring a key in
//! verify changes which blocks a node accepts. Before
//! `ring_restore_from_height` (u64::MAX on 2300) the mempool and apply paths
//! must behave exactly as before #118: same error, same state bytes, ring
//! untouched (so a proposer can never pack a tx a restarted seat rejects).

use super::*;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;
const TIP: u64 = crate::params_v2::TESTNET_2300_KEY_BIND_FROM_HEIGHT + 100;

/// A node "after restart": alice's key is bound in state (`meta.public_key`)
/// but missing from the in-memory ring. Returns (node, alice's signing ring).
fn restarted(gate: u64) -> (Node, KeyRing) {
    let mut a = KeyRing::new(CryptoScheme::Falcon512);
    a.generate_id("alice");
    restarted_with(gate, a)
}

fn restarted_with(gate: u64, a: KeyRing) -> (Node, KeyRing) {
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("alice".into(), 1_000_000), ("faucet".into(), 1_000_000)],
    );
    n.keys.generate_id("faucet");
    n.tip_height = TIP;
    n.ring_restore_from_height = gate;
    let mut acct = n.state.get("alice");
    acct.meta.public_key = a.public_hex("alice").unwrap();
    n.state.accounts.insert("alice".into(), acct);
    assert!(!n.keys.has_public("alice"));
    (n, a)
}

fn swap(n: &Node, k: &KeyRing, signer: &str) -> Tx {
    let body = TxBody::Swap { pool_id: "p".into(), token_in: "FPL".into(), amount_in: 10, min_out: 0 };
    let seq = n.state.get("alice").sequence;
    let payload = Tx::payload("alice", seq, "", 0, 2, NID, &body);
    Tx {
        account: "alice".into(),
        sequence: seq,
        destination: String::new(),
        amount: 0,
        fee: 2,
        network_id: NID,
        public_key: k.public_hex(signer).unwrap(),
        signature: k.sign(signer, &payload).unwrap(),
        tx_id: sha256_hex(payload.as_bytes()),
        body,
    }
}

fn snap(n: &Node) -> (Vec<u8>, Vec<u8>, BTreeMap<String, String>, usize) {
    (
        serde_json::to_vec(&n.state).unwrap(),
        serde_json::to_vec(&n.defi).unwrap(),
        // The per-node random faucet key is not part of what is compared.
        n.keys.export_publics().into_iter().filter(|(id, _)| id != "faucet").collect(),
        n.mempool.len(),
    )
}

#[test]
fn testnet_2300_ring_restore_activates_at_702000() {
    let h = crate::params_v2::ring_restore_from_height(NID);
    assert_eq!(h, 702_000);
    assert_eq!(h, crate::params_v2::TESTNET_2300_RING_RESTORE_FROM_HEIGHT);
    let (mut n, _) = restarted(h);
    // Gate is judged for the next block (tip + 1).
    for (tip, open) in [(TIP, false), (h - 2, false), (h - 1, true), (h, true), (h + 1_000, true)] {
        n.tip_height = tip;
        assert_eq!(n.ring_restore_gate_open(), open, "tip {tip}");
    }
}

#[test]
fn testnet_2300_restore_off_below_on_from_height() {
    let h = crate::params_v2::TESTNET_2300_RING_RESTORE_FROM_HEIGHT;
    // 2.9.79: Fix B (on-chain verify key) now activates before the ring
    // restore, so "unknown key" is only expected below both heights.
    let hb = crate::params_v2::TESTNET_2300_KEY_BIND_ONCHAIN_FROM_HEIGHT;
    let below_both = h.min(hb);
    // Next block is below both gates: unchanged "unknown key", ring untouched.
    let (mut n, a) = restarted(h);
    n.tip_height = below_both - 2;
    let tx = swap(&n, &a, "alice");
    let before = snap(&n);
    assert_eq!(n.apply_protocol_tx(&tx).unwrap_err(), "bad tx crypto: unknown key");
    assert_eq!(before, snap(&n));
    // Next block is h: bound key restored.
    let (mut n, a) = restarted(h);
    n.tip_height = h - 1;
    let tx = swap(&n, &a, "alice");
    if let Err(e) = n.apply_protocol_tx(&tx) {
        assert!(!e.contains("unknown key"), "{e}");
    }
    assert!(n.keys.has_public("alice"));
}

#[test]
fn pre_activation_apply_is_unchanged() {
    let (mut n, a) = restarted(u64::MAX);
    let tx = swap(&n, &a, "alice");
    let before = snap(&n);
    assert_eq!(n.apply_protocol_tx(&tx).unwrap_err(), "bad tx crypto: unknown key");
    assert_eq!(before, snap(&n), "state, defi and ring byte-identical");
}

#[test]
fn pre_activation_mempool_does_not_restore_so_proposer_cannot_pack_it() {
    let (mut n, a) = restarted(u64::MAX);
    let tx = swap(&n, &a, "alice");
    let before = snap(&n);
    assert_eq!(n.submit_tx(tx.clone()).unwrap_err(), "bad tx crypto: unknown key");
    assert_eq!(before, snap(&n), "nothing admitted, ring untouched");
    // The mempool attempt left no key behind for the apply/propose path.
    assert_eq!(n.apply_protocol_tx(&tx).unwrap_err(), "bad tx crypto: unknown key");
    assert_eq!(before, snap(&n));
}

#[test]
fn pre_activation_known_key_path_identical_to_open_gate() {
    // With the key already in the ring the gate is irrelevant: same result, same bytes.
    let mut alice = KeyRing::new(CryptoScheme::Falcon512);
    alice.generate_id("alice");
    let run = |gate: u64| {
        let (mut n, a) = restarted_with(gate, alice.clone());
        n.keys.insert_public("alice", &a.public_hex("alice").unwrap()).unwrap();
        let tx = swap(&n, &a, "alice");
        let r = n.apply_protocol_tx(&tx);
        (r, snap(&n))
    };
    assert_eq!(run(u64::MAX), run(TIP + 1));
}

#[test]
fn from_activation_restores_bound_key_only() {
    let (mut n, a) = restarted(TIP + 1);
    let tx = swap(&n, &a, "alice");
    let r = n.apply_protocol_tx(&tx);
    if let Err(e) = &r {
        assert!(!e.contains("unknown key"), "{e}");
    }
    assert!(n.keys.has_public("alice"));

    // A key the account does not bind is never added.
    let (mut n, _) = restarted(TIP + 1);
    let mut evil = KeyRing::new(CryptoScheme::Falcon512);
    evil.generate_id("alice");
    let ring_before = n.keys.export_publics();
    assert!(n.apply_protocol_tx(&swap(&n, &evil, "alice")).is_err());
    assert_eq!(ring_before, n.keys.export_publics());
}
