//! 2.9.63 memory leak regressions (seat OOMs, 12 Oct 2026):
//! `body_fill` grew on every relayed `TxsBatch`; in-sync seats asked for a
//! full snapshot every 5 s.

use super::*;
use crate::crypto::{CryptoScheme, KeyRing};

fn node_and_tx() -> (Node, Tx, Ledger) {
    let mut keys = KeyRing::new(CryptoScheme::HmacDev);
    keys.generate_many(["v1", "alice", "bob"]);
    let tx = Tx::sign_body(&keys, "alice", 0, "bob", 1, 1, 2300, TxBody::Pay).unwrap();
    let shell = Ledger::seal(1, "00".into(), 2300, "v1", &keys, vec![tx.clone()], "root".into(), 1).unwrap();
    let users = vec![("alice".to_string(), 1_000_000u64), ("bob".to_string(), 1_000_000u64)];
    let n = Node::new("v1", ChainConfig { min_fee: 0, ..Default::default() }, keys, &users);
    (n, tx, shell)
}

fn with_id(tx: &Tx, id: String) -> Tx {
    let mut t = tx.clone();
    t.tx_id = id;
    t
}

#[test]
fn body_fill_stays_bounded_under_steady_relay() {
    let (mut n, tx, _) = node_and_tx();
    // About 14 hours of 20 tx/s relay, fed in TxsBatch-sized chunks.
    for chunk in 0..1_000u64 {
        let batch: Vec<Tx> = (0..1_000u64)
            .map(|i| with_id(&tx, format!("{:064x}", chunk * 1_000 + i)))
            .collect();
        n.note_body_fill(batch);
        assert!(n.body_fill.len() <= Node::BODY_FILL_CAP, "len={}", n.body_fill.len());
    }
}

#[test]
fn body_fill_trim_keeps_txs_a_waiting_shell_needs() {
    let (mut n, tx, mut shell) = node_and_tx();
    let need = format!("{:064x}", u64::MAX);
    shell.ledger_hash = "shell".into();
    shell.tx_ids = vec![need.clone()];
    n.body_shells.insert(shell.ledger_hash.clone(), shell);
    n.note_body_fill([with_id(&tx, need.clone())]);
    let filler: Vec<Tx> =
        (0..(Node::BODY_FILL_CAP as u64 * 3)).map(|i| with_id(&tx, format!("{i:064x}"))).collect();
    n.note_body_fill(filler);
    assert!(n.body_fill.len() <= Node::BODY_FILL_CAP);
    assert!(n.body_fill.contains_key(&need), "shell tx survived the trim");
}

#[test]
fn supply_body_txs_goes_through_the_cap() {
    let (mut n, tx, _) = node_and_tx();
    let txs: Vec<Tx> =
        (0..(Node::BODY_FILL_CAP as u64 * 2)).map(|i| with_id(&tx, format!("{i:064x}"))).collect();
    assert!(n.supply_body_txs("none", txs).is_none());
    assert!(n.body_fill.len() <= Node::BODY_FILL_CAP);
}

fn users() -> Vec<(String, u64)> {
    vec![("alice".into(), 1_000_000), ("bob".into(), 1_000_000)]
}

fn seat_with_archive_peer(tip: u64, best: u64) -> Node {
    let mut n = Node::new(
        "v2",
        ChainConfig::default(),
        KeyRing::new(CryptoScheme::HmacDev),
        &users(),
    );
    n.register_validator("v1", 1000);
    n.register_validator("v2", 1000);
    n.tip_height = tip;
    n.best_peer_tip = best;
    if let Some(v) = n.validators.get_mut("v1") {
        v.advertised_archive = true;
        v.last_pong_ms = n.now_ms;
    }
    n
}

#[test]
fn in_sync_seat_never_wants_a_snapshot_from_empty_residuals() {
    for streak in [3u32, 9, 1_000] {
        let mut n = seat_with_archive_peer(487_000, 487_000);
        n.empty_residual_streak = streak;
        assert!(!n.wants_snapshot(), "lag 0, streak {streak}: no join snap");
        assert!(!n.take_snapshot_pull(), "lag 0: no NeedSnapshot");
    }
}

#[test]
fn small_hole_still_join_snaps() {
    let mut n = seat_with_archive_peer(10, 12);
    n.empty_residual_streak = 3;
    assert!(n.wants_snapshot(), "tip-2 hole after three empties must snap");
}
