//! v2.9.65 audit C-1: first-seen key binding.
//!
//! Before `TESTNET_2300_KEY_BIND_FROM_HEIGHT` any tx that carries a Falcon key
//! binds it to an account with no key (the PoC theft works). From that height a
//! first key binds only if the account id is the key's classic address or a
//! funder committed the key hash with `PayToKey`.

use super::*;
use crate::params_v2::TESTNET_2300_KEY_BIND_FROM_HEIGHT as H;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;

fn node(extra: &[(&str, u64)], tip: u64) -> Node {
    let mut accts: Vec<(String, u64)> = vec![("faucet".into(), 1_000_000)];
    for (a, b) in extra {
        accts.push(((*a).into(), *b));
    }
    let mut n = Node::new(
        "v1",
        ChainConfig {
            scheme: CryptoScheme::Falcon512,
            network_id: NID,
            pack_budget_ms: 0,
            ..ChainConfig::default()
        },
        KeyRing::new(CryptoScheme::Falcon512),
        &accts,
    );
    n.keys.generate_id("faucet");
    n.tip_height = tip;
    n
}

const PRE: u64 = H - 2; // open height H-1: old rules
const POST: u64 = H - 1; // open height H: new rules

fn ring(id: &str) -> KeyRing {
    let mut k = KeyRing::new(CryptoScheme::Falcon512);
    k.generate_id(id);
    k
}

fn sq(n: &Node, a: &str) -> u64 {
    n.state.get(a).sequence
}

fn bal(n: &Node, a: &str) -> u64 {
    n.state.get(a).balance
}

/// Sign `body` for `account` with the key held under `signer` in `keys`.
fn sign_as(
    keys: &KeyRing,
    signer: &str,
    account: &str,
    seq: u64,
    dest: &str,
    amount: u64,
    body: TxBody,
) -> Tx {
    let payload = Tx::payload(account, seq, dest, amount, 2, NID, &body);
    Tx {
        account: account.into(),
        sequence: seq,
        destination: dest.into(),
        amount,
        fee: 2,
        network_id: NID,
        public_key: keys.public_hex(signer).unwrap(),
        signature: keys.sign(signer, &payload).unwrap(),
        tx_id: sha256_hex(payload.as_bytes()),
        body,
    }
}

#[test]
fn poc_theft_works_before_activation() {
    let mut n = node(&[("victim", 5_000)], PRE);
    let atk = ring("victim");
    let tx = sign_as(&atk, "victim", "victim", sq(&n, "victim"), "attacker", 4_990, TxBody::Pay);
    assert!(n.submit_tx(tx.clone()).unwrap());
    n.apply_protocol_tx(&tx).unwrap();
    assert_eq!(bal(&n, "attacker"), 4_990);
    assert_eq!(bal(&n, "victim"), 8);
}

#[test]
fn poc_theft_fails_after_activation() {
    let mut n = node(&[("victim", 5_000)], POST);
    let atk = ring("victim");
    let tx = sign_as(&atk, "victim", "victim", sq(&n, "victim"), "attacker", 4_990, TxBody::Pay);
    let e = n.submit_tx(tx.clone()).unwrap_err();
    assert!(e.contains("key bind"), "{e}");
    assert!(!n.keys.has_public("victim"), "mempool must not bind the key");
    let e = n.apply_protocol_tx(&tx).unwrap_err();
    assert!(e.contains("key bind"), "{e}");
    assert!(!n.account_key_bound("victim"));
    assert_eq!(bal(&n, "victim"), 5_000);
    assert_eq!(bal(&n, "attacker"), 0);
    // Same for every other enroll body (AssetPay, Claim, NameReserve, VaultOpen).
    for body in [
        TxBody::AssetPay { asset: "BTC".into() },
        TxBody::Claim,
        TxBody::NameReserve,
        TxBody::VaultOpen { destination: "attacker".into() },
    ] {
        let tx = sign_as(&atk, "victim", "victim", sq(&n, "victim"), "attacker", 1, body.clone());
        assert!(n.submit_tx(tx.clone()).is_err(), "{body:?}");
        assert!(n.apply_protocol_tx(&tx).is_err(), "{body:?}");
        assert!(!n.account_key_bound("victim"), "{body:?}");
    }
}

#[test]
fn committed_first_fund_then_first_send_works() {
    let mut n = node(&[], POST);
    let user = ring("newuser");
    let pk = user.public_hex("newuser").unwrap();
    let commit = key_commit_hash(&pk).unwrap();
    // Faucet funds the name and commits the key hash in the same tx.
    let fund = Tx::sign_body(&n.keys, "faucet", sq(&n, "faucet"), "newuser", 2_000, 2, NID,
        TxBody::PayToKey { key_hash: commit.clone() },
    )
    .unwrap();
    assert!(n.submit_tx(fund.clone()).unwrap());
    n.apply_protocol_tx(&fund).unwrap();
    assert_eq!(bal(&n, "newuser"), 2_000);
    assert_eq!(n.state.get("newuser").meta.key_commit, commit);

    // An attacker key cannot take it.
    let atk = ring("newuser");
    let evil = sign_as(&atk, "newuser", "newuser", sq(&n, "newuser"), "attacker", 1_000, TxBody::Pay);
    assert!(n.submit_tx(evil.clone()).unwrap_err().contains("key bind"));
    assert!(n.apply_protocol_tx(&evil).unwrap_err().contains("key bind"));

    // The committed key's first Pay binds it and spends.
    let send = sign_as(&user, "newuser", "newuser", sq(&n, "newuser"), "bob", 500, TxBody::Pay);
    assert!(n.submit_tx(send.clone()).unwrap());
    n.apply_protocol_tx(&send).unwrap();
    assert_eq!(bal(&n, "bob"), 500);
    assert_eq!(n.state.get("newuser").meta.public_key, pk);
    assert!(n.keys.has_public("newuser"));

    // Replay stays blocked.
    let e = n.apply_protocol_tx(&send).unwrap_err();
    assert!(e.contains("bad sequence"), "{e}");
    assert_eq!(bal(&n, "bob"), 500);
    // A second send works with the bound key.
    let send2 = sign_as(&user, "newuser", "newuser", sq(&n, "newuser"), "bob", 100, TxBody::Pay);
    n.apply_protocol_tx(&send2).unwrap();
    assert_eq!(bal(&n, "bob"), 600);
    // Bound now: the attacker still fails, and PayToKey cannot re-commit.
    let evil2 = sign_as(&atk, "newuser", "newuser", sq(&n, "newuser"), "attacker", 1, TxBody::Pay);
    assert!(n.apply_protocol_tx(&evil2).is_err());
    let recommit = Tx::sign_body(&n.keys, "faucet", sq(&n, "faucet"), "newuser", 1, 2, NID,
        TxBody::PayToKey { key_hash: key_commit_hash(&atk.public_hex("newuser").unwrap()).unwrap() },
    )
    .unwrap();
    assert!(n.apply_protocol_tx(&recommit).unwrap_err().contains("already has a key"));
}

#[test]
fn pay_to_key_rules() {
    // Not admitted or applied before activation (2.9.64 cannot decode it).
    let mut pre = node(&[], PRE);
    let c = "ab".repeat(32);
    let t = Tx::sign_body(&pre.keys, "faucet", sq(&pre, "faucet"), "x1", 10, 2, NID, TxBody::PayToKey { key_hash: c.clone() }).unwrap();
    assert!(pre.submit_tx(t.clone()).unwrap_err().contains("not active"));
    assert!(pre.apply_protocol_tx(&t).unwrap_err().contains("not active"));
    assert_eq!(bal(&pre, "x1"), 0);

    let mut n = node(&[], POST);
    let bad = Tx::sign_body(&n.keys, "faucet", sq(&n, "faucet"), "x1", 10, 2, NID, TxBody::PayToKey { key_hash: "zz".into() }).unwrap();
    assert!(n.apply_protocol_tx(&bad).is_err());
    let t = Tx::sign_body(&n.keys, "faucet", sq(&n, "faucet"), "x1", 10, 2, NID, TxBody::PayToKey { key_hash: c.clone() }).unwrap();
    n.apply_protocol_tx(&t).unwrap();
    // Same commitment again is fine (top-up); a different one is refused.
    let t2 = Tx::sign_body(&n.keys, "faucet", sq(&n, "faucet"), "x1", 10, 2, NID, TxBody::PayToKey { key_hash: c }).unwrap();
    n.apply_protocol_tx(&t2).unwrap();
    let t3 = Tx::sign_body(&n.keys, "faucet", sq(&n, "faucet"), "x1", 10, 2, NID, TxBody::PayToKey { key_hash: "cd".repeat(32) }).unwrap();
    assert!(n.apply_protocol_tx(&t3).unwrap_err().contains("another key"));
    assert_eq!(bal(&n, "x1"), 20);
    // A plain Pay to an uncommitted name still works; it just cannot be spent by a stranger.
    let p = Tx::sign_with(&n.keys, "faucet", sq(&n, "faucet"), "x2", 10, 2, NID).unwrap();
    n.apply_protocol_tx(&p).unwrap();
    assert_eq!(bal(&n, "x2"), 10);
}

#[test]
fn classic_address_account_binds_its_own_key() {
    let user = ring("tmp");
    let pk = user.public_hex("tmp").unwrap();
    let addr = falcon_classic_address(&pk).unwrap();
    assert!(addr.starts_with('r'), "{addr}");
    let mut n = node(&[(addr.as_str(), 3_000)], POST);
    let send = sign_as(&user, "tmp", &addr, sq(&n, &addr), "bob", 1_000, TxBody::Pay);
    assert!(n.submit_tx(send.clone()).unwrap());
    n.apply_protocol_tx(&send).unwrap();
    assert_eq!(bal(&n, "bob"), 1_000);
    // A different key cannot claim that address.
    let mut m = node(&[(addr.as_str(), 3_000)], POST);
    let atk = ring("tmp");
    let evil = sign_as(&atk, "tmp", &addr, sq(&m, &addr), "attacker", 1_000, TxBody::Pay);
    assert!(m.submit_tx(evil.clone()).is_err());
    assert!(m.apply_protocol_tx(&evil).is_err());
}

#[test]
fn already_bound_accounts_unchanged_after_activation() {
    let mut n = node(&[], POST);
    // faucet's key is in the ring (bound before H): it keeps sending.
    let p = Tx::sign_with(&n.keys, "faucet", sq(&n, "faucet"), "bob", 10, 2, NID).unwrap();
    assert!(n.submit_tx(p.clone()).unwrap());
    n.apply_protocol_tx(&p).unwrap();
    assert_eq!(bal(&n, "bob"), 10);
}

#[test]
fn classic_address_matches_known_vector() {
    // ripple base58 of version 0 + 20 zero bytes is the well-known ACCOUNT_ZERO.
    let mut p = vec![0u8; 21];
    let c1 = <sha2::Sha256 as sha2::Digest>::digest(&p);
    let c2 = <sha2::Sha256 as sha2::Digest>::digest(c1);
    p.extend_from_slice(&c2[..4]);
    assert_eq!(base58_ripple(&p), "rrrrrrrrrrrrrrrrrrrrrhoLvTp");
}

