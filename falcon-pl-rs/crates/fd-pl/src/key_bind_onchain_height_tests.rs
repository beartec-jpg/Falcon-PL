//! 2.9.79: Fix B (`TESTNET_2300_KEY_BIND_ONCHAIN_FROM_HEIGHT`) brought forward
//! from 702_000 to 692_000. The ring restore (#118) stays at 702_000.

use super::*;
use crate::params_v2::{
    key_bind_onchain_from_height, ring_restore_from_height, TESTNET_2300_KEY_BIND_ONCHAIN_FROM_HEIGHT as HB,
    TESTNET_2300_RING_RESTORE_FROM_HEIGHT as HR,
};

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;
/// Tip when the height was set (stalled, 22 Oct 2026).
const TIP_AT_SET: u64 = 673_438;
/// Measured rate when the height was set.
const BLOCKS_PER_MIN: u64 = 11;

fn node(extra: &[(&str, u64)], tip: u64) -> Node {
    let mut accts: Vec<(String, u64)> = vec![("faucet".into(), 1_000_000)];
    for (a, b) in extra {
        accts.push(((*a).into(), *b));
    }
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &accts,
    );
    n.keys.generate_id("faucet");
    let pk = n.keys.public_hex("faucet").unwrap();
    let mut f = n.state.get("faucet");
    f.meta.public_key = pk;
    n.state.accounts.insert("faucet".into(), f);
    n.tip_height = tip;
    n.ring_restore_from_height = ring_restore_from_height(NID);
    n
}

#[test]
fn testnet_2300_fix_b_activates_at_692000() {
    assert_eq!(HB, 692_000);
    assert_eq!(key_bind_onchain_from_height(NID), HB);
}

#[test]
fn fix_b_margin_is_one_to_two_days_and_before_ring_restore() {
    let day = BLOCKS_PER_MIN * 60 * 24;
    let ahead = HB - TIP_AT_SET;
    assert!(ahead >= day && ahead <= 2 * day, "{ahead} blocks ahead");
    assert!(HB > crate::params_v2::TESTNET_2300_KEY_BIND_FROM_HEIGHT);
    assert!(HB < HR, "Fix B now precedes the ring restore");
    assert_eq!(HR, 702_000, "ring restore unchanged");
}

#[test]
fn gate_boundary_is_next_block() {
    let mut n = node(&[], HB - 2);
    assert!(!n.key_bind_onchain_gate_open());
    n.tip_height = HB - 1;
    assert!(n.key_bind_onchain_gate_open());
}

#[test]
fn window_692000_to_702000_fix_b_on_ring_restore_off() {
    let n = node(&[], HB - 1);
    assert!(n.key_bind_onchain_gate_open());
    assert!(!n.ring_restore_gate_open());
    let n = node(&[], HR - 1);
    assert!(n.key_bind_onchain_gate_open());
    assert!(n.ring_restore_gate_open());
}

/// scottspersonalnode-bond case: key only in a ring. Bound before HB on a
/// ring-holding seat, refused from HB on every seat.
#[test]
fn ring_only_sender_refused_from_new_height() {
    for (tip, ok) in [(HB - 2, true), (HB - 1, false)] {
        let mut n = node(&[("acct", 1_000)], tip);
        n.keys.generate_id("acct");
        let tx = Tx::sign_with(&n.keys, "acct", 0, "faucet", 1, 2, NID).unwrap();
        let r = n.key_bind_check(&tx);
        assert_eq!(r.is_ok(), ok, "tip {tip}: {r:?}");
        if !ok {
            assert_eq!(r.unwrap_err(), KEY_BIND_REFUSED);
        }
    }
}

/// Recovery after HB: a PayToKey commit is accepted (the ring no longer
/// blocks it) and then the owner's key binds.
#[test]
fn ring_only_account_recovers_with_pay_to_key_after_new_height() {
    let mut n = node(&[("acct", 1_000)], HB - 1);
    n.keys.generate_id("acct"); // stale ring entry on this seat
    let mut owner = KeyRing::new(CryptoScheme::Falcon512);
    owner.generate_id("acct");
    let commit = key_commit_hash(&owner.public_hex("acct").unwrap()).unwrap();
    let fund = Tx::sign_body(&n.keys, "faucet", n.state.get("faucet").sequence, "acct", 1, 2, NID,
        TxBody::PayToKey { key_hash: commit.clone() }).unwrap();
    n.apply_protocol_tx(&fund).unwrap();
    assert_eq!(n.state.get("acct").meta.key_commit, commit);
    let tx = Tx::sign_with(&owner, "acct", 0, "faucet", 1, 2, NID).unwrap();
    assert!(n.key_bind_check(&tx).is_ok());
}

/// testing / testing.vault / scott.personal.vault case: committed, never
/// bound. Still binds from HB with the committed key, and only that key.
#[test]
fn committed_unbound_account_still_binds_after_new_height() {
    let mut n = node(&[("acct", 1_000)], HB - 1);
    let mut owner = KeyRing::new(CryptoScheme::Falcon512);
    owner.generate_id("acct");
    let mut a = n.state.get("acct");
    a.meta.key_commit = key_commit_hash(&owner.public_hex("acct").unwrap()).unwrap();
    n.state.accounts.insert("acct".into(), a);
    let good = Tx::sign_with(&owner, "acct", 0, "faucet", 1, 2, NID).unwrap();
    assert!(n.key_bind_check(&good).is_ok());
    let mut other = KeyRing::new(CryptoScheme::Falcon512);
    other.generate_id("acct");
    let bad = Tx::sign_with(&other, "acct", 0, "faucet", 1, 2, NID).unwrap();
    assert_eq!(n.key_bind_check(&bad).unwrap_err(), KEY_BIND_REFUSED);
}

/// Between HB and HR a restarted seat already verifies a bound account's tx
/// from the on-chain key (Fix B), without the ring restore.
#[test]
fn restarted_seat_verifies_from_chain_key_between_heights() {
    let src = node(&[], HB);
    let tx = Tx::sign_body(&src.keys, "faucet", 0, "", 0, 2, NID,
        TxBody::Unbond { validator: "nobody".into(), amount: 1 }).unwrap();
    let pk = src.keys.public_hex("faucet").unwrap();
    for (tip, verifies) in [(HB - 2, false), (HB - 1, true), (HR - 2, true)] {
        let mut n = node(&[], tip);
        n.keys = KeyRing::new(CryptoScheme::Falcon512);
        let mut a = n.state.get("faucet");
        a.meta.public_key = pk.clone();
        n.state.accounts.insert("faucet".into(), a);
        assert!(!n.ring_restore_gate_open());
        let e = n.apply_protocol_tx(&tx).unwrap_err();
        assert_eq!(!e.contains("unknown key"), verifies, "tip {tip}: {e}");
    }
}
