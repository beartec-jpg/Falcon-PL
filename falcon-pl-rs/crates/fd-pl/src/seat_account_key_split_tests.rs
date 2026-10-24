//! 2.9.79-beta.3: seat/account key split. An account named like a validator
//! seat (`nodesafe`) has wallet key A on chain; the ring holds seat key B under
//! the same id. From the Fix B height account txs verify against A (on chain);
//! seat votes/proposals keep verifying against B (ring).

use super::*;
use crate::params_v2::TESTNET_2300_KEY_BIND_ONCHAIN_FROM_HEIGHT as HB;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;

/// (node with seat key B in ring and account key A on chain, wallet ring with A)
fn setup(tip: u64) -> (Node, KeyRing) {
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("nodesafe".into(), 1_000_000)],
    );
    n.keys.generate_id("nodesafe"); // seat key B
    let mut wallet = KeyRing::new(CryptoScheme::Falcon512);
    wallet.generate_id("nodesafe"); // wallet key A
    let a = wallet.public_hex("nodesafe").unwrap();
    assert_ne!(Some(a.clone()), n.keys.public_hex("nodesafe"));
    let mut acct = n.state.get("nodesafe");
    acct.meta.public_key = a;
    n.state.accounts.insert("nodesafe".into(), acct);
    n.tip_height = tip;
    n.ring_restore_from_height = crate::params_v2::ring_restore_from_height(NID);
    (n, wallet)
}

fn bodies() -> Vec<TxBody> {
    vec![
        TxBody::Claim,
        TxBody::Swap { pool_id: "FPL/USDC".into(), token_in: "FPL".into(), amount_in: 1, min_out: 0 },
    ]
}

fn sig_err(e: &str) -> bool {
    e.contains("bad tx crypto") || e.contains("sender key does not match")
}

#[test]
fn nodesafe_claim_and_swap_signed_with_onchain_key_pass_from_fix_b() {
    for body in bodies() {
        let (mut n, w) = setup(HB - 1);
        let tx = Tx::sign_body(&w, "nodesafe", 0, "", 0, 2, NID, body.clone()).unwrap();
        assert!(n.verify_account_tx(&tx).is_ok(), "{body:?}");
        if let Err(e) = n.apply_protocol_tx(&tx) {
            assert!(!sig_err(&e), "{body:?}: {e}");
        }
        let (mut n2, _) = setup(HB - 1);
        let mut a = n2.state.get("nodesafe");
        a.meta.public_key = w.public_hex("nodesafe").unwrap();
        n2.state.accounts.insert("nodesafe".into(), a);
        if let Err(e) = n2.submit_tx(tx.clone()) {
            assert!(!sig_err(&e), "mempool {body:?}: {e}");
        }
    }
}

#[test]
fn before_fix_b_unchanged_ring_key_wins() {
    let (n, w) = setup(HB - 2);
    let tx = Tx::sign_body(&w, "nodesafe", 0, "", 0, 2, NID, TxBody::Claim).unwrap();
    let e = n.verify_account_tx(&tx).unwrap_err();
    assert!(e.contains("key mismatch"), "{e}");
}

#[test]
fn seat_key_no_longer_signs_for_the_account_after_fix_b() {
    let (n, _) = setup(HB - 1);
    let tx = Tx::sign_body(&n.keys, "nodesafe", 0, "", 0, 2, NID, TxBody::Claim).unwrap();
    assert!(sig_err(&n.verify_account_tx(&tx).unwrap_err()));
}

#[test]
fn seat_votes_still_use_ring_seat_key() {
    let (n, w) = setup(HB + 10);
    let b = n.keys.public_hex("nodesafe").unwrap();
    let msg = "pl-vote:test";
    let sig_b = n.keys.sign("nodesafe", msg).unwrap();
    assert!(n.keys.verify_message(n.cfg.scheme, "nodesafe", &b, msg, &sig_b).is_ok());
    let a = w.public_hex("nodesafe").unwrap();
    let sig_a = w.sign("nodesafe", msg).unwrap();
    assert!(n.keys.verify_message(n.cfg.scheme, "nodesafe", &a, msg, &sig_a).is_err(), "wallet key is not a seat key");
    assert!(n.keys.verify("nodesafe", msg, &sig_b));
    assert!(!n.keys.verify("nodesafe", msg, &sig_a));
}

#[test]
fn pack_preverify_uses_onchain_key_from_fix_b() {
    let (n, w) = setup(HB - 1);
    let tx = Tx::sign_body(&w, "nodesafe", 0, "", 0, 2, NID, TxBody::Claim).unwrap();
    let on = n.onchain_sig_keys(&[&tx]);
    assert!(Node::pack_sig_ok(&n.keys, &on, &tx));
    let (n0, _) = setup(HB - 2);
    let on0 = n0.onchain_sig_keys(&[&tx]);
    assert!(on0.is_empty());
    assert!(!Node::pack_sig_ok(&n0.keys, &on0, &tx));
}
