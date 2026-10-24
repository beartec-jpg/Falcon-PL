//! 2.9.79-beta.3 reverse guard rail: from the Fix B height no new wallet
//! account can take a registered validator seat id (with a different key).

use super::*;
use crate::params_v2::TESTNET_2300_KEY_BIND_ONCHAIN_FROM_HEIGHT as HB;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;

/// Node with seat `seatx` (seat key B) and a funder with an on-chain key.
fn setup(tip: u64) -> (Node, KeyRing, KeyRing, KeyRing) {
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("funder".into(), 1_000_000)],
    );
    let mut seat = KeyRing::new(CryptoScheme::Falcon512);
    seat.generate_id("seatx");
    let mut wallet = KeyRing::new(CryptoScheme::Falcon512);
    wallet.generate_id("seatx");
    let mut funder = KeyRing::new(CryptoScheme::Falcon512);
    funder.generate_id("funder");
    n.register_validator("seatx", 1000);
    // Deterministic seat key (as BondV2 binds it on chain).
    let v = n.validators.get_mut("seatx").unwrap();
    v.public_key = seat.public_hex("seatx").unwrap();
    v.v2_seat = true;
    let mut f = n.state.get("funder");
    f.meta.public_key = funder.public_hex("funder").unwrap();
    n.state.accounts.insert("funder".into(), f);
    n.tip_height = tip;
    (n, seat, wallet, funder)
}

fn pay_to_key(n: &Node, funder: &KeyRing, pk: &str) -> Tx {
    let key_hash = key_commit_hash(pk).unwrap();
    Tx::sign_body(funder, "funder", n.state.get("funder").sequence, "seatx", 10, 0, NID, TxBody::PayToKey { key_hash }).unwrap()
}

#[test]
fn pay_to_key_to_seat_id_with_wallet_key_refused_from_fix_b() {
    let (mut n, _seat, wallet, funder) = setup(HB - 1);
    let tx = pay_to_key(&n, &funder, &wallet.public_hex("seatx").unwrap());
    let e = n.apply_protocol_tx(&tx).unwrap_err();
    assert!(e.contains(Node::SEAT_NAME_TAKEN), "{e}");
}

#[test]
fn pay_to_key_to_seat_id_with_seat_key_allowed() {
    let (mut n, seat, _wallet, funder) = setup(HB - 1);
    let tx = pay_to_key(&n, &funder, &seat.public_hex("seatx").unwrap());
    if let Err(e) = n.apply_protocol_tx(&tx) {
        assert!(!e.contains(Node::SEAT_NAME_TAKEN), "{e}");
    }
}

#[test]
fn first_key_bind_on_seat_id_with_other_key_refused_from_fix_b() {
    let (n, seat, wallet, _f) = setup(HB - 1);
    let w = Tx::sign_body(&wallet, "seatx", 0, "", 0, 0, NID, TxBody::Pay).unwrap();
    let e = n.key_bind_check(&w).unwrap_err();
    assert!(e.contains(Node::SEAT_NAME_TAKEN), "{e}");
    let s = Tx::sign_body(&seat, "seatx", 0, "", 0, 0, NID, TxBody::Pay).unwrap();
    if let Err(e) = n.key_bind_check(&s) {
        assert!(!e.contains(Node::SEAT_NAME_TAKEN), "{e}");
    }
}

#[test]
fn guard_inactive_before_fix_b() {
    let (mut n, _seat, wallet, funder) = setup(HB - 10);
    let tx = pay_to_key(&n, &funder, &wallet.public_hex("seatx").unwrap());
    if let Err(e) = n.apply_protocol_tx(&tx) {
        assert!(!e.contains(Node::SEAT_NAME_TAKEN), "{e}");
    }
    let w = Tx::sign_body(&wallet, "seatx", 0, "", 0, 0, NID, TxBody::Pay).unwrap();
    if let Err(e) = n.key_bind_check(&w) {
        assert!(!e.contains(Node::SEAT_NAME_TAKEN), "{e}");
    }
}

/// Review #146 (2): a legacy seat's Hello-learned `public_key` is not consensus
/// state. Two nodes with identical registry/state but different Hello keys
/// must reach the same verdict (the guard uses `registered_key` only).
#[test]
fn legacy_seat_hello_key_does_not_change_verdict() {
    let (mut a, seat, wallet, funder) = setup(HB - 1);
    let (mut b, _, _, _) = setup(HB - 1);
    let mut f = b.state.get("funder");
    f.meta.public_key = funder.public_hex("funder").unwrap();
    b.state.accounts.insert("funder".into(), f);
    // Legacy seat (not in the registry); Hello metadata differs per node.
    for (n, hello) in [(&mut a, &seat), (&mut b, &wallet)] {
        let v = n.validators.get_mut("seatx").unwrap();
        v.v2_seat = false;
        v.public_key = hello.public_hex("seatx").unwrap();
    }
    for pk in [seat.public_hex("seatx").unwrap(), wallet.public_hex("seatx").unwrap()] {
        let tx = pay_to_key(&a, &funder, &pk);
        let ra = a.seat_name_guard("seatx", |k| key_commit_hash(k).as_deref() == Some(match &tx.body { TxBody::PayToKey { key_hash } => key_hash.as_str(), _ => "" }));
        let rb = b.seat_name_guard("seatx", |k| key_commit_hash(k).as_deref() == Some(match &tx.body { TxBody::PayToKey { key_hash } => key_hash.as_str(), _ => "" }));
        assert_eq!(ra, rb);
        assert!(ra.unwrap_err().contains(Node::SEAT_NAME_TAKEN));
    }
}

/// Review #146 (3): SEAT_NAME_TAKEN is a permanent failure (dropped, not retried).
#[test]
fn seat_name_taken_is_permanent() {
    assert!(Node::is_permanent_tx_failure(&format!("pay_to_key: {}", Node::SEAT_NAME_TAKEN)));
}
