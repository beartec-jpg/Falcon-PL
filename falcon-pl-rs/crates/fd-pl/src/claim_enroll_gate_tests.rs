//! 8980c0b made `Claim` bind a first-seen key. No seat ever ran it (the 2.9.63+
//! deploy line took only its account-tail hunks), and it changes which blocks
//! a node accepts, so on main it is gated at `claim_enroll_from_height`
//! (`u64::MAX` on 2300). Below the gate a Claim from an unbound key must fail
//! exactly like on the seats: same error, same state and ring bytes, on both
//! the mempool and the apply path.

use super::*;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;
const TIP: u64 = crate::params_v2::TESTNET_2300_KEY_BIND_FROM_HEIGHT + 100;

fn node(gate: u64) -> (Node, KeyRing) {
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("bob".into(), 1_000_000), ("faucet".into(), 1_000_000)],
    );
    n.keys.generate_id("faucet");
    n.tip_height = TIP;
    n.claim_enroll_from_height = gate;
    let mut b = KeyRing::new(CryptoScheme::Falcon512);
    b.generate_id("bob");
    assert!(!n.keys.has_public("bob"));
    assert!(n.state.get("bob").meta.public_key.is_empty());
    (n, b)
}

fn signed(n: &Node, k: &KeyRing, body: TxBody, amount: u64, dest: &str) -> Tx {
    let seq = n.state.get("bob").sequence;
    let payload = Tx::payload("bob", seq, dest, amount, 2, NID, &body);
    Tx {
        account: "bob".into(),
        sequence: seq,
        destination: dest.into(),
        amount,
        fee: 2,
        network_id: NID,
        public_key: k.public_hex("bob").unwrap(),
        signature: k.sign("bob", &payload).unwrap(),
        tx_id: sha256_hex(payload.as_bytes()),
        body,
    }
}

fn snap(n: &Node) -> (Vec<u8>, BTreeMap<String, String>, usize) {
    (
        serde_json::to_vec(&n.state).unwrap(),
        n.keys.export_publics().into_iter().filter(|(id, _)| id != "faucet").collect(),
        n.mempool.len(),
    )
}

#[test]
fn testnet_2300_claim_enroll_is_never_until_set() {
    assert_eq!(crate::params_v2::TESTNET_2300_CLAIM_ENROLL_FROM_HEIGHT, u64::MAX);
    assert_eq!(crate::params_v2::claim_enroll_from_height(NID), u64::MAX);
    let n = Node::new(
        "v1",
        ChainConfig { network_id: NID, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[],
    );
    assert_eq!(n.claim_enroll_from_height, u64::MAX);
}

#[test]
fn claim_is_not_a_name_enroll_body_like_on_the_seats() {
    let (n, b) = node(u64::MAX);
    let claim = signed(&n, &b, TxBody::Claim, 0, "");
    assert!(!claim.is_name_enroll());
    assert!(!n.tx_name_enroll(&claim));
    // Pay is unchanged.
    let pay = signed(&n, &b, TxBody::Pay, 1, "faucet");
    assert!(pay.is_name_enroll());
    assert!(n.tx_name_enroll(&pay));
}

/// A classic-address account (C-1 lets it bind its own key) with that key
/// not yet in the ring or in state.
fn classic(gate: u64) -> (Node, KeyRing, String) {
    let mut k = KeyRing::new(CryptoScheme::Falcon512);
    k.generate_id("tmp");
    let addr = crate::falcon_classic_address(&k.public_hex("tmp").unwrap()).unwrap();
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[(addr.clone(), 1_000_000), ("faucet".into(), 1_000_000)],
    );
    n.keys.generate_id("faucet");
    n.tip_height = TIP;
    n.claim_enroll_from_height = gate;
    assert!(!n.keys.has_public(&addr));
    (n, k, addr)
}

fn signed_as(n: &Node, k: &KeyRing, acct: &str, body: TxBody, amount: u64, dest: &str) -> Tx {
    let seq = n.state.get(acct).sequence;
    let payload = Tx::payload(acct, seq, dest, amount, 2, NID, &body);
    Tx {
        account: acct.into(),
        sequence: seq,
        destination: dest.into(),
        amount,
        fee: 2,
        network_id: NID,
        public_key: k.public_hex("tmp").unwrap(),
        signature: k.sign("tmp", &payload).unwrap(),
        tx_id: sha256_hex(payload.as_bytes()),
        body,
    }
}

fn snap_of(n: &Node, acct: &str) -> (Vec<u8>, bool, usize) {
    (serde_json::to_vec(&n.state).unwrap(), n.keys.has_public(acct), n.mempool.len())
}

#[test]
fn pre_gate_claim_does_not_bind_on_apply_or_mempool() {
    let (mut n, k, addr) = classic(u64::MAX);
    let tx = signed_as(&n, &k, &addr, TxBody::Claim, 0, "");
    // Mempool first: refused exactly like a seat, nothing admitted or bound.
    let before = snap_of(&n, &addr);
    assert_eq!(n.submit_tx(tx.clone()).unwrap_err(), "bad tx crypto: unknown key");
    assert_eq!(before, snap_of(&n, &addr), "nothing admitted, nothing bound");
    // Apply: same error, ring not bound. (apply_protocol_tx records the
    // sender key on the account before verify, as on the seats; block apply
    // owns atomicity, so only the ring and the error are compared here.)
    assert_eq!(n.apply_protocol_tx(&tx).unwrap_err(), "bad tx crypto: unknown key");
    assert!(!n.keys.has_public(&addr));
    // Same account, same key: Pay still binds (the setup itself is valid).
    let pay = signed_as(&n, &k, &addr, TxBody::Pay, 10, "faucet");
    n.apply_protocol_tx(&pay).unwrap();
    assert!(n.keys.has_public(&addr));
}

#[test]
fn from_gate_claim_binds_first_seen_key() {
    let (mut n, k, addr) = classic(TIP + 1);
    let tx = signed_as(&n, &k, &addr, TxBody::Claim, 0, "");
    assert!(n.tx_name_enroll(&tx));
    if let Err(e) = n.apply_protocol_tx(&tx) {
        assert!(!e.contains("unknown key") && !e.contains("key bind"), "{e}");
    }
    assert!(n.keys.has_public(&addr));
}
