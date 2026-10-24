//! v2.9.77.
//!
//! Fix A (mempool, not consensus): a tx a seat dropped as permanently invalid
//! is never re-admitted (stale watcher-eth-lc 157542a2 at seq 4622 ping-ponged
//! between seats for hours), and a permanently invalid same-(account, seq)
//! arrival does not evict a valid pending tx.
//!
//! Fix B (consensus, gated at `TESTNET_2300_KEY_BIND_ONCHAIN_FROM_HEIGHT`):
//! key binding, `PayToKey`'s "destination already has a key" and sender
//! verification use on-chain keys only, never the node-local ring.

use super::*;
use crate::params_v2::TESTNET_2300_KEY_BIND_ONCHAIN_FROM_HEIGHT as HB;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;
const PRE: u64 = HB - 2; // open height HB-1: 2.9.76 rules
const POST: u64 = HB - 1; // open height HB: on-chain only

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

fn pay(n: &Node, amount: u64) -> Tx {
    Tx::sign_with(&n.keys, "faucet", n.state.get("faucet").sequence, "bob", amount, 2, NID).unwrap()
}

/// Put faucet's ring key on chain so the key-bind rules do not interfere
/// with the mempool tests.
fn bind_on_chain(n: &mut Node, id: &str) {
    let pk = n.keys.public_hex(id).unwrap();
    let mut a = n.state.get(id);
    a.meta.public_key = pk;
    n.state.accounts.insert(id.into(), a);
}

// ---- Fix A -------------------------------------------------------------

#[test]
fn sanitize_dropped_tx_is_not_readmitted() {
    let mut n = node(&[], PRE);
    bind_on_chain(&mut n, "faucet");
    let poison = pay(&n, 50_000_000); // insufficient balance: permanent
    assert!(n.submit_tx(poison.clone()).unwrap());
    assert_eq!(n.drop_permanent_applicable_heads(), 1);
    assert!(n.mempool.is_empty());
    // Same tx back from gossip: refused before verify, nothing stored.
    assert_eq!(n.submit_tx(poison.clone()).unwrap_err(), TX_PREVIOUSLY_DROPPED);
    assert!(n.mempool.is_empty());
    // A valid tx at the same (account, seq) is still admitted.
    let good = pay(&n, 10);
    assert!(n.submit_tx(good.clone()).unwrap());
    assert!(n.mempool.by_id.contains_key(&good.tx_id));
}

#[test]
fn invalid_same_seq_arrival_does_not_evict_valid_pending() {
    let mut n = node(&[], PRE);
    bind_on_chain(&mut n, "faucet");
    let good = pay(&n, 10);
    let poison = pay(&n, 50_000_000);
    assert_eq!(good.sequence, poison.sequence);
    assert!(n.submit_tx(good.clone()).unwrap());
    let e = n.submit_tx(poison.clone()).unwrap_err();
    assert!(e.contains("refused same-seq replacement"), "{e}");
    assert!(n.mempool.by_id.contains_key(&good.tx_id), "valid pending kept");
    assert!(!n.mempool.by_id.contains_key(&poison.tx_id));
    // Re-gossip of the poison is now a set lookup.
    assert_eq!(n.submit_tx(poison).unwrap_err(), TX_PREVIOUSLY_DROPPED);
    // The valid tx packs.
    assert!(n.mempool.by_id.contains_key(&good.tx_id));
}

#[test]
fn valid_same_seq_resubmit_still_replaces_stale_pending() {
    let mut n = node(&[], PRE);
    bind_on_chain(&mut n, "faucet");
    let old = pay(&n, 10);
    let newer = pay(&n, 11);
    assert!(n.submit_tx(old.clone()).unwrap());
    assert!(n.submit_tx(newer.clone()).unwrap());
    assert!(!n.mempool.by_id.contains_key(&old.tx_id));
    assert!(n.mempool.by_id.contains_key(&newer.tx_id));
}

#[test]
fn perm_drop_set_is_bounded_and_expires() {
    let mut s = PermDropSet::default();
    for i in 0..(PermDropSet::CAP + 10) {
        s.insert(&format!("id{i}"), 1_000, "t");
    }
    assert_eq!(s.len(), PermDropSet::CAP);
    assert!(!s.contains("id0", 1_000, "t"), "oldest evicted");
    let last = format!("id{}", PermDropSet::CAP + 9);
    assert!(s.contains(&last, 1_000, "t"));
    assert!(!s.contains(&last, 1_000, "t2"), "new tip expires it");
    assert!(!s.contains(&last, 1_000 + PermDropSet::TTL_MS, "t"), "TTL");
}

// ---- Fix B -------------------------------------------------------------

#[test]
fn ring_only_key_binds_before_gate_not_after() {
    // faucet: key in this seat's ring, nothing on chain (the watcher-eth case).
    let mut pre = node(&[], PRE);
    let tx = pay(&pre, 10);
    assert!(!tx.public_key.is_empty());
    assert!(pre.key_bind_check(&tx).is_ok(), "2.9.76 rule: ring binds");
    pre.apply_protocol_tx(&tx).unwrap();

    let mut post = node(&[], POST);
    let tx = pay(&post, 10);
    assert_eq!(post.key_bind_check(&tx).unwrap_err(), KEY_BIND_REFUSED);
    assert!(post.apply_protocol_tx(&tx).unwrap_err().contains("key bind"));
}

#[test]
fn seats_with_and_without_ring_key_agree_after_gate() {
    let with_ring = node(&[], POST);
    let mut no_ring = node(&[], POST);
    no_ring.keys = KeyRing::new(CryptoScheme::Falcon512);
    let tx = pay(&with_ring, 10);
    assert_eq!(
        with_ring.key_bind_check(&tx).is_ok(),
        no_ring.key_bind_check(&tx).is_ok()
    );
    assert_eq!(with_ring.account_key_bound("faucet"), no_ring.account_key_bound("faucet"));
}

#[test]
fn account_key_bound_reads_chain_only_after_gate() {
    let pre = node(&[], PRE);
    assert!(pre.account_key_bound("faucet"), "2.9.76: ring counts");
    let mut post = node(&[], POST);
    assert!(!post.account_key_bound("faucet"), "ring ignored");
    bind_on_chain(&mut post, "faucet");
    assert!(post.account_key_bound("faucet"));
}

#[test]
fn pay_to_key_destination_check_ignores_ring_after_gate() {
    let mut n = node(&[("dest", 0)], POST);
    bind_on_chain(&mut n, "faucet");
    n.keys.generate_id("dest"); // ring-only on this seat
    let user = {
        let mut k = KeyRing::new(CryptoScheme::Falcon512);
        k.generate_id("dest");
        k
    };
    let commit = key_commit_hash(&user.public_hex("dest").unwrap()).unwrap();
    let fund = Tx::sign_body(&n.keys, "faucet", n.state.get("faucet").sequence, "dest", 100, 2, NID,
        TxBody::PayToKey { key_hash: commit.clone() }).unwrap();
    n.apply_protocol_tx(&fund).unwrap();
    assert_eq!(n.state.get("dest").meta.key_commit, commit);

    let mut pre = node(&[("dest", 0)], PRE);
    bind_on_chain(&mut pre, "faucet");
    pre.keys.generate_id("dest");
    let fund = Tx::sign_body(&pre.keys, "faucet", pre.state.get("faucet").sequence, "dest", 100, 2, NID,
        TxBody::PayToKey { key_hash: commit }).unwrap();
    assert!(pre.apply_protocol_tx(&fund).unwrap_err().contains("already has a key"));
}

#[test]
fn verify_falls_back_to_on_chain_key_after_gate() {
    // Non-enroll body (Unbond), carried key, ring lost (restart), key on
    // chain. Before the gate verify needs the ring: "unknown key". After it
    // the on-chain key verifies and the tx reaches its body checks.
    let src = node(&[], POST);
    let tx = Tx::sign_body(&src.keys, "faucet", 0, "", 0, 2, NID,
        TxBody::Unbond { validator: "nobody".into(), amount: 1 }).unwrap();
    assert!(!tx.public_key.is_empty());
    let pk = src.keys.public_hex("faucet").unwrap();
    for tip in [PRE, POST] {
        let mut n = node(&[], tip);
        n.keys = KeyRing::new(CryptoScheme::Falcon512);
        let mut acct = n.state.get("faucet");
        acct.meta.public_key = pk.clone();
        n.state.accounts.insert("faucet".into(), acct);
        let e = n.apply_protocol_tx(&tx).unwrap_err();
        if tip == PRE {
            assert!(e.contains("unknown key"), "pre: {e}");
        } else {
            assert!(!e.contains("bad tx crypto"), "post: {e}");
        }
    }
}

#[test]
fn pack_strips_only_keys_stored_on_chain() {
    // The node binary strips with `account_public_key` (state only).
    let mut n = node(&[], POST);
    assert!(n.keys.has_public("faucet"));
    assert_eq!(n.account_public_key("faucet"), None, "ring-only: key stays on the body");
    bind_on_chain(&mut n, "faucet");
    assert!(n.account_public_key("faucet").is_some());
}

/// v2.9.78: a tombstone only holds at the tip it was dropped at. After the
/// tip moves (state the failure depended on may have changed) the identical
/// tx id is admitted again and re-evaluated.
#[test]
fn tombstone_expires_when_tip_changes() {
    let mut n = node(&[], PRE);
    bind_on_chain(&mut n, "faucet");
    let poison = pay(&n, 50_000_000);
    assert!(n.submit_tx(poison.clone()).unwrap());
    assert_eq!(n.drop_permanent_applicable_heads(), 1);
    // Same tip: refused every time (stops the gossip ping-pong).
    assert_eq!(n.submit_tx(poison.clone()).unwrap_err(), TX_PREVIOUSLY_DROPPED);
    assert_eq!(n.submit_tx(poison.clone()).unwrap_err(), TX_PREVIOUSLY_DROPPED);
    // New tip whose state makes it valid (faucet funded): admitted and kept.
    n.tip_hash = "next-tip".into();
    let mut f = n.state.get("faucet");
    f.balance = 100_000_000;
    n.state.accounts.insert("faucet".into(), f);
    assert!(n.submit_tx(poison.clone()).unwrap());
    n.perm_head_memo.clear();
    assert_eq!(n.drop_permanent_applicable_heads(), 0, "valid after state change");
    assert!(n.mempool.by_id.contains_key(&poison.tx_id));
}
