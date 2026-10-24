//! Restart loss of ring-only keys (host1 2026-10-19/20, `watcher-eth`).
//!
//! An account whose key was bound before the key-bind gate, but never
//! recorded on the account (`meta.public_key` empty), only had that key in
//! the node-local ring. A seat restart rebuilt the ring without it, and from
//! then on `key_bind_check` refused the account's txs with
//! `KEY_BIND_REFUSED` on every restarted packer. The ring sidecar written
//! next to the snapshot restores it on the same seat.

use super::*;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;
const TIP: u64 = crate::params_v2::TESTNET_2300_KEY_BIND_FROM_HEIGHT + 100;

fn seat() -> Node {
    let mut n = Node::new(
        "v6",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("watcher-eth".into(), 5_000), ("faucet".into(), 1_000_000)],
    );
    n.tip_height = TIP;
    n
}

fn v3_tx(n: &Node, k: &KeyRing) -> Tx {
    let body = TxBody::Swap { pool_id: "p".into(), token_in: "FPL".into(), amount_in: 1, min_out: 0 };
    let seq = n.state.get("watcher-eth").sequence;
    let payload = Tx::payload("watcher-eth", seq, "", 0, 2, NID, &body);
    Tx {
        account: "watcher-eth".into(),
        sequence: seq,
        destination: String::new(),
        amount: 0,
        fee: 2,
        network_id: NID,
        public_key: k.public_hex("watcher-eth").unwrap(),
        signature: k.sign("watcher-eth", &payload).unwrap(),
        tx_id: sha256_hex(payload.as_bytes()),
        body,
    }
}

/// The seat before the restart: key in the ring (bound pre key-bind), not on the account.
fn before_restart() -> (Node, KeyRing) {
    let mut w = KeyRing::new(CryptoScheme::Falcon512);
    w.generate_id("watcher-eth");
    let mut n = seat();
    n.keys.insert_public("watcher-eth", &w.public_hex("watcher-eth").unwrap()).unwrap();
    assert!(n.account_public_key("watcher-eth").is_none());
    (n, w)
}

fn restart(old: &Node) -> Node {
    let mut n = seat();
    n.state = old.state.clone();
    n
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fpl-ring-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&d).unwrap();
    d.join("v6.json")
}

#[test]
fn restart_without_sidecar_loses_ring_only_key() {
    let (n, w) = before_restart();
    let tx = v3_tx(&n, &w);
    assert!(n.key_bind_check(&tx).is_ok(), "before restart the ring key binds");
    let after = restart(&n);
    assert_eq!(after.key_bind_check(&tx).unwrap_err(), KEY_BIND_REFUSED);
}

#[test]
fn ring_sidecar_restores_key_after_restart() {
    let (n, w) = before_restart();
    let snap = tmp("restore");
    let ring = Node::ring_sidecar_path(&snap);
    assert!(ring.ends_with("v6.ring.json"));
    assert!(n.save_ring_publics(&ring).unwrap() >= 1);
    let mut after = restart(&n);
    assert_eq!(after.merge_ring_publics(&ring), 1);
    let tx = v3_tx(&after, &w);
    assert!(after.key_bind_check(&tx).is_ok());
    // Idempotent: a second merge adds nothing.
    assert_eq!(after.merge_ring_publics(&ring), 0);
}

#[test]
fn ring_sidecar_never_overrides_chain_or_ring() {
    let (n, _w) = before_restart();
    let ring = Node::ring_sidecar_path(tmp("override"));
    n.save_ring_publics(&ring).unwrap();
    let mut other = KeyRing::new(CryptoScheme::Falcon512);
    other.generate_id("watcher-eth");
    let other_pk = other.public_hex("watcher-eth").unwrap();

    // Account already binds a different key on chain: sidecar key is skipped.
    let mut after = restart(&n);
    let mut acct = after.state.get("watcher-eth");
    acct.meta.public_key = other_pk.clone();
    after.state.accounts.insert("watcher-eth".into(), acct);
    assert_eq!(after.merge_ring_publics(&ring), 0);
    assert!(!after.keys.has_public("watcher-eth"));

    // Ring already holds a key: it is kept.
    let mut after = restart(&n);
    after.keys.insert_public("watcher-eth", &other_pk).unwrap();
    assert_eq!(after.merge_ring_publics(&ring), 0);
    assert!(crate::crypto::hex_eq(&after.keys.public_hex("watcher-eth").unwrap(), &other_pk));
}

#[test]
fn ring_sidecar_missing_or_wrong_scheme_adds_nothing() {
    let (n, _w) = before_restart();
    let mut after = restart(&n);
    assert_eq!(after.merge_ring_publics(tmp("missing").with_file_name("nope.ring.json")), 0);
    let ring = Node::ring_sidecar_path(tmp("scheme"));
    Node::write_ring_publics(&ring, CryptoScheme::HmacDev, &n.keys.export_publics()).unwrap();
    assert_eq!(after.merge_ring_publics(&ring), 0);
}

#[test]
fn join_snapshot_does_not_carry_ring_keys() {
    let (n, _w) = before_restart();
    let js = serde_json::to_string(&n.to_join_snapshot()).unwrap();
    let pk = n.keys.public_hex("watcher-eth").unwrap();
    assert!(!js.contains(&pk));
}
