//! Falcon PL 2.10.0 activation gates (`external_addrs_from_height`,
//! `empty_block_from_height`): before the height a 2.10.0 node behaves exactly
//! like 2.9.81 on consensus paths (no `SetExternalAddrs`, no empty ledger, same
//! state root / snapshot bytes); from the height it behaves as new.

use super::*;
use crate::params_v2::*;

const NID: u32 = NETWORK_ID_PUBLIC_TESTNET_2300;
const H: u64 = TESTNET_2300_V2_10_FROM_HEIGHT;

#[test]
fn both_gates_share_the_2_10_height_on_2300_and_are_off_elsewhere() {
    assert_eq!(TESTNET_2300_EXTERNAL_ADDRS_FROM_HEIGHT, H);
    assert_eq!(TESTNET_2300_EMPTY_BLOCK_FROM_HEIGHT, H);
    assert_eq!(external_addrs_from_height(NID), H);
    assert_eq!(empty_block_from_height(NID), H);
    // Activation is after the 2.9.80 gates and the height is a round number.
    assert!(H > TESTNET_2300_SEAT_PAYOUT_FROM_HEIGHT);
    assert_eq!(H % 1_000, 0);
    // Other networks: never active unless an env override is set (not in CI).
    for other in [0u32, 1, NID.wrapping_add(1)] {
        if other == NID {
            continue;
        }
        if std::env::var("FPL_EXTERNAL_ADDRS_FROM_HEIGHT").is_err() {
            assert_eq!(external_addrs_from_height(other), u64::MAX);
        }
        if std::env::var("FPL_EMPTY_BLOCK_FROM_HEIGHT").is_err() {
            assert_eq!(empty_block_from_height(other), u64::MAX);
        }
    }
}

fn node_2300(tip: u64) -> (Node, KeyRing) {
    let mut keys = KeyRing::new(CryptoScheme::Falcon512);
    keys.generate_id("alice");
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("alice".into(), 1_000_000), ("bob".into(), 1_000_000)],
    );
    n.tip_height = tip;
    // 2300 requires the signing key to be committed on the account (key bind).
    let mut a = n.state.get("alice");
    a.meta.public_key = keys.public_hex("alice").unwrap();
    n.state.accounts.insert("alice".into(), a);
    // Fresh nodes pick the production heights up from `Node::new`.
    assert_eq!(n.external_addrs_from_height, H);
    assert_eq!(n.empty_block_from_height, H);
    (n, keys)
}

fn set_addr(keys: &KeyRing, n: &Node, seq: u64) -> Tx {
    Tx::sign_body(
        keys,
        "alice",
        seq,
        "",
        0,
        2,
        NID,
        TxBody::SetExternalAddrs {
            chain: "btc".into(),
            address: "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx".into(),
        },
    )
    .map(|t| {
        let _ = n;
        t
    })
    .unwrap()
}

#[test]
fn set_external_addrs_refused_before_activation_accepted_from_it() {
    // Open height = tip + 1. Last pre-activation open height is H - 1.
    let (mut n, keys) = node_2300(H - 2);
    let seq = n.state.get("alice").sequence;
    let tx = set_addr(&keys, &n, seq);
    assert!(n.external_addrs_tx_pre_activation(&tx));
    assert!(!n.external_addrs_at(H - 1));

    let root_before = n.state.root();
    let acct_before = n.state.get("alice");
    // Admission refuses it: it is not kept, so it is never relayed or exported.
    assert_eq!(n.submit_tx(tx.clone()).unwrap_err(), EXTERNAL_ADDRS_NOT_ACTIVE);
    assert!(n.mempool.is_empty());
    // The apply path refuses it too, and nothing changes (no fee, no sequence bump, no key enroll).
    assert_eq!(n.apply_protocol_tx(&tx).unwrap_err(), EXTERNAL_ADDRS_NOT_ACTIVE);
    assert_eq!(n.state.root(), root_before);
    let acct_after = n.state.get("alice");
    assert_eq!(acct_after.balance, acct_before.balance);
    assert_eq!(acct_after.sequence, acct_before.sequence);
    assert_eq!(acct_after.meta.public_key, acct_before.meta.public_key);
    assert!(acct_after.external_addrs.is_empty());
    // Not a "permanent failure" eviction either: a 2.10.0 node must not
    // drop-list it differently from 2.9.81 (which cannot even decode it).
    assert!(!Node::is_permanent_tx_failure(EXTERNAL_ADDRS_NOT_ACTIVE));

    // Open height H: active.
    n.tip_height = H - 1;
    assert!(n.external_addrs_at(H));
    assert!(!n.external_addrs_tx_pre_activation(&tx));
    assert!(n.submit_tx(tx.clone()).unwrap());
    n.apply_protocol_tx(&tx).unwrap();
    let a = n.state.get("alice");
    assert_eq!(a.sequence, acct_before.sequence + 1);
    assert_eq!(a.external_addrs.get("btc").map(String::as_str), Some("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"));
    assert_ne!(n.state.root(), root_before);
}

#[test]
fn ordinary_txs_are_unaffected_by_the_external_addrs_gate() {
    let (mut n, keys) = node_2300(H - 2);
    let seq = n.state.get("alice").sequence;
    let tx = Tx::sign_with(&keys, "alice", seq, "bob", 10, 2, NID).unwrap();
    assert!(!n.external_addrs_tx_pre_activation(&tx));
    assert!(n.submit_tx(tx).unwrap());
}

#[test]
fn state_root_and_account_bytes_unchanged_while_external_addrs_empty() {
    let (n, _) = node_2300(H - 2);
    let acct = n.state.get("alice");
    let json = serde_json::to_string(&acct).unwrap();
    assert!(!json.contains("external_addrs"), "empty map must not be serialized: {json}");
    // A 2.9.81-encoded account (no field) decodes and re-encodes byte-identically.
    let back: Account = serde_json::from_str(&json).unwrap();
    assert!(back.external_addrs.is_empty());
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
    // World-state root: same whether accounts were round-tripped or not.
    let mut ws = n.state.clone();
    for (_, a) in ws.accounts.iter_mut() {
        let s = serde_json::to_string(a).unwrap();
        *a = serde_json::from_str(&s).unwrap();
    }
    assert_eq!(ws.root(), n.state.root());

    // Once an address is set, the field appears (and the root changes): this
    // is exactly what must not happen before activation.
    let mut with = acct.clone();
    with.external_addrs.insert("btc".into(), "x".into());
    assert!(serde_json::to_string(&with).unwrap().contains("external_addrs"));
}

/// Build a node that has permanently-failing txs in its mempool and an
/// otherwise empty block to seal.
fn node_with_evictable(gate_open: bool) -> (Node, String) {
    let cfg = ChainConfig {
        pack_budget_ms: 0,
        max_txs_per_ledger: 16,
        soft_ok_votes: 1,
        min_fee: 0,
        fee_escalate: false,
        ..Default::default()
    };
    let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1_000, &[("alice".into(), 100_000), ("bob".into(), 100_000), ("carol".into(), 100_000)], cfg);
    net.pong_all();
    let winner = net.nodes.values().next().unwrap().lottery_packer().unwrap();
    let nid = net.cfg.network_id;
    let uk = net.user_keys.clone();
    let mut n = net.nodes.remove(&winner).unwrap();
    n.empty_block_from_height = if gate_open { 0 } else { u64::MAX };
    // PayToKey to a destination that already has a key: permanent failure.
    let mut bob = n.state.get("bob");
    bob.meta.public_key = uk.public_hex("bob").unwrap();
    n.state.accounts.insert("bob".into(), bob);
    let tx = Tx::sign_body(&uk, "carol", n.state.get("carol").sequence, "bob", 10, 1, nid, TxBody::PayToKey { key_hash: "33".repeat(32) }).unwrap();
    let _ = n.mempool.insert(tx, n.cfg.max_mempool_txs);
    (n, winner)
}

#[test]
fn empty_fallback_block_not_sealed_before_activation() {
    let (mut n, winner) = node_with_evictable(false);
    assert!(!n.can_seal_empty());
    let root = n.state.root();
    // Behaves like 2.9.81: the permanent-failure tx is evicted locally but no
    // empty block is sealed (2.9.81 seats would reject it).
    let out = n.pack_or_skip(&winner).expect("no error");
    assert!(out.is_none(), "must not seal an empty ledger before activation");
    assert!(n.mempool.is_empty(), "local eviction is node-local and still happens");
    assert_eq!(n.state.root(), root);
    assert_eq!(n.tip_height, 0);
}

#[test]
fn empty_fallback_block_sealed_and_accepted_from_activation() {
    let (mut n, winner) = node_with_evictable(true);
    assert!(n.empty_block_at(1));
    let ledger = n.pack_or_skip(&winner).unwrap().expect("empty fallback block");
    assert!(ledger.txs.is_empty());
    assert_eq!(ledger.height, 1);

    // The same empty ledger is rejected by a node whose gate is still closed
    // (this is the 2.9.81-equivalent view).
    n.empty_block_from_height = 2;
    assert!(!n.empty_block_at(ledger.height));
    assert_eq!(n.accept_ledger(&ledger).unwrap_err(), "empty ledger rejected");
    assert_eq!(n.tip_height, 0);
    n.empty_block_from_height = 1;

    // ... and accepted once the gate covers its height.
    n.accept_ledger(&ledger).expect("active: empty ledger accepted");
}
